//! Outbound HMAC-signed webhook dispatch.
//!
//! Config comes from `AppState.webhooks` (populated by the entrypoint's
//! `Config::from_env`): `outbound_urls` = comma-separated POST targets,
//! `outbound_secret` = HMAC-SHA256 shared secret.
//!
//! Each event becomes one POST per URL with body:
//!   { "event_type": "...", "task_uuid": "...?", "payload": {...},
//!     "ts": "<iso>", "event_id": <n>? }
//! and header `X-PTask-Signature: sha256=<hex>` over the raw body bytes.
//! `ts` is the event's commit time (its `pt_event_log.ts`) and `event_id`
//! its journal id, when the event is journaled; not the delivery time.
//!
//! A request streams its committed events into an [`Outbox`], a handle on
//! the server's single [`OutboundQueue`]: one worker delivers every event,
//! from every request, one at a time in the order they were enqueued.
//! Inline delivery made the request wait on every subscriber (a hung URL
//! cost 10s per event); a worker per request delivered concurrent requests'
//! events out of commit order. Events already handed over still go out if
//! the client disconnects mid-batch, and graceful shutdown drains the queue
//! for a bounded time ([`OutboundQueue::drain`]).

use crate::AppState;
use hmac::{Hmac, Mac};
use ptask_core::Db;
use ptask_core::config::WebhookConfig;
use ptask_core::webhook_log::{Direction, record};
use sha2::Sha256;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{info, warn};

type HmacSha256 = Hmac<Sha256>;

/// Shared outbound client. reqwest's default client has NO timeout, so a
/// hung subscriber would hold the delivery task forever. Bound both connect
/// and total request time, and reuse the client (connection pool) across
/// dispatches.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client construction cannot fail with static config")
    })
}

/// Sign a body with HMAC-SHA256. Returns the hex digest. Empty secret →
/// returns an empty string (caller can decide whether to send the header).
pub fn sign(body: &[u8], secret: &str) -> String {
    if secret.is_empty() {
        return String::new();
    }
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any-length key");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// One journal event bound for the outbound subscribers.
pub struct OutboundEvent {
    pub event_type: String,
    pub task_uuid: Option<String>,
    pub payload: serde_json::Value,
    /// The event's `pt_event_log.uuid`: the worker reads its commit time and
    /// journal id from the row. `None` (or no row) falls back to now.
    pub event_uuid: Option<String>,
}

/// Process-wide order for "commit, then enqueue". Holding it across both
/// makes enqueue order equal commit order for concurrent requests; SQLite
/// serialises the writes anyway, so it costs no write concurrency.
static COMMIT_ORDER: Mutex<()> = Mutex::new(());

/// Run a blocking mutation that enqueues its outbound event before
/// returning, so no other request can commit and enqueue in between.
pub fn commit_ordered<T>(f: impl FnOnce() -> T) -> T {
    let _order = COMMIT_ORDER.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

/// The server's single outbound queue, shared by every request through
/// [`AppState`]. The worker starts on first use (inside the runtime) and
/// holds only the DB and webhook config, so dropping the queue's sender
/// at shutdown ends it once the backlog is delivered.
#[derive(Default)]
pub struct OutboundQueue {
    tx: Mutex<Option<UnboundedSender<OutboundEvent>>>,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
    closed: std::sync::atomic::AtomicBool,
}

impl OutboundQueue {
    fn sender(&self, db: &Db, cfg: &Arc<WebhookConfig>) -> Option<UnboundedSender<OutboundEvent>> {
        if cfg.outbound_urls.is_empty() || self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        let mut tx = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        if tx.is_none() {
            let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel::<OutboundEvent>();
            let (db, cfg) = (db.clone(), cfg.clone());
            let worker = tokio::spawn(async move {
                while let Some(e) = rx.recv().await {
                    dispatch(&db, &cfg, e).await;
                }
            });
            *self.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(worker);
            *tx = Some(sender);
        }
        tx.clone()
    }

    /// Stop accepting events and wait up to `timeout` for the backlog to be
    /// delivered. Call after the server stopped taking requests.
    pub async fn drain(&self, timeout: Duration) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        // Dropping the last sender closes the channel once it is empty.
        drop(self.tx.lock().unwrap_or_else(|e| e.into_inner()).take());
        let worker = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(worker) = worker
            && tokio::time::timeout(timeout, worker).await.is_err()
        {
            warn!(
                target: "ptask::webhook",
                timeout_s = timeout.as_secs(),
                "outbound webhook backlog not drained before shutdown; remaining events dropped"
            );
        }
    }
}

/// One request's handle on the outbound queue. A no-op when no URL is
/// configured.
#[derive(Clone)]
pub struct Outbox(Option<UnboundedSender<OutboundEvent>>);

impl Outbox {
    pub fn start(state: &AppState) -> Self {
        Self(state.outbound.sender(&state.db, &state.webhooks))
    }

    pub fn send(&self, event: OutboundEvent) {
        if let Some(tx) = &self.0 {
            // Fails only after shutdown closed the queue.
            let _ = tx.send(event);
        }
    }
}

/// `(ts, id)` of a journaled event.
fn committed(db: &Db, event_uuid: &str) -> Option<(String, i64)> {
    db.with_conn(|c| {
        Ok(c.query_row(
            "SELECT ts, id FROM pt_event_log WHERE uuid = ?1",
            [event_uuid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
    .ok()
}

/// Fan-out one event to every configured URL. Logs each attempt (sent or
/// failed) to pt_webhook_log. No retries.
async fn dispatch(db: &Db, cfg: &WebhookConfig, e: OutboundEvent) {
    if cfg.outbound_urls.is_empty() {
        return;
    }
    let journal = match e.event_uuid.clone() {
        Some(uuid) => {
            let db = db.clone();
            crate::blocking::db_value(move || committed(&db, &uuid))
                .await
                .ok()
                .flatten()
        }
        None => None,
    };
    let (ts, event_id) = match journal {
        Some((ts, id)) => (ts, Some(id)),
        None => (
            ptask_core::dates::format_iso(&ptask_core::dates::now_in_operator_tz().unwrap_or_else(
                |_| {
                    // Should never fail; fall back to UTC to avoid swallowing the event.
                    jiff::Zoned::now().with_time_zone(jiff::tz::TimeZone::UTC)
                },
            )),
            None,
        ),
    };
    let (event_type, task_uuid, payload) = (&e.event_type, e.task_uuid.as_deref(), &e.payload);
    let mut envelope = serde_json::json!({
        "event_type": event_type,
        "task_uuid": task_uuid,
        "payload": payload,
        "ts": ts,
    });
    if let Some(id) = event_id {
        envelope["event_id"] = serde_json::json!(id);
    }
    let body = serde_json::to_vec(&envelope).unwrap_or_default();
    let sig = sign(&body, &cfg.outbound_secret);
    let client = http_client();

    for url in &cfg.outbound_urls {
        let mut req = client
            .post(url)
            .header("content-type", "application/json")
            .body(body.clone());
        if !sig.is_empty() {
            req = req.header("X-PTask-Signature", format!("sha256={}", sig));
        }
        let outcome = match req.send().await {
            Ok(resp) => {
                let ok = resp.status().is_success();
                info!(
                    target: "ptask::webhook",
                    url = %url,
                    status = %resp.status(),
                    event = %event_type,
                    "outbound webhook"
                );
                ok
            }
            Err(e) => {
                warn!(
                    target: "ptask::webhook",
                    url = %url,
                    error = %e,
                    event = %event_type,
                    "outbound webhook failed"
                );
                false
            }
        };
        let db = db.clone();
        let url_owned = url.clone();
        let envelope = envelope.clone();
        match crate::blocking::db_value(move || {
            record(&db, Direction::Out, &url_owned, &envelope, outcome)
        })
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                warn!(
                    target: "ptask::webhook",
                    url = %url,
                    error = %e,
                    "outbound audit write failed"
                );
            }
            Err(e) => {
                warn!(
                    target: "ptask::webhook",
                    url = %url,
                    error = %e,
                    "outbound audit write aborted"
                );
            }
        }
    }
}
