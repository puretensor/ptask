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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
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
#[derive(Clone)]
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
pub(crate) static COMMIT_ORDER: Mutex<()> = Mutex::new(());

/// Run a blocking mutation that enqueues its outbound event before
/// returning, so no other request can commit and enqueue in between. With
/// no subscriber configured there is nothing to order and no lock is taken.
pub fn commit_ordered<T>(outbox: &Outbox, f: impl FnOnce() -> T) -> T {
    if outbox.0.is_none() {
        return f();
    }
    let _order = COMMIT_ORDER.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

/// Per-URL backlog cap. A subscriber that is down or hanging (10s per POST)
/// falls behind; past this many queued events, new ones for that URL are
/// dropped (logged and counted in `pt_webhook_dropped_total`) instead of
/// growing memory without bound.
const MAX_BACKLOG_PER_URL: usize = 10_000;

/// One subscriber's ordered queue.
struct Lane {
    url: String,
    tx: tokio::sync::mpsc::Sender<OutboundEvent>,
}

/// What a request's [`Outbox`] holds: every lane plus the drop counter.
struct Lanes {
    lanes: Vec<Lane>,
    dropped: Arc<AtomicU64>,
}

/// The server's outbound queue, shared by every request through
/// [`AppState`]: one bounded, ordered worker per subscriber URL, so a hung
/// subscriber delays only its own deliveries. Workers start on first use
/// (inside the runtime) and hold only the DB and webhook config; dropping
/// the senders at shutdown ends each once its backlog is delivered.
pub struct OutboundQueue {
    lanes: Mutex<Option<Arc<Lanes>>>,
    workers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    closed: std::sync::atomic::AtomicBool,
    dropped: Arc<AtomicU64>,
    backlog: usize,
}

impl Default for OutboundQueue {
    fn default() -> Self {
        Self::with_backlog(MAX_BACKLOG_PER_URL)
    }
}

impl OutboundQueue {
    /// A queue whose lanes each hold at most `backlog` undelivered events.
    pub fn with_backlog(backlog: usize) -> Self {
        Self {
            lanes: Mutex::default(),
            workers: Mutex::default(),
            closed: Default::default(),
            dropped: Arc::default(),
            backlog: backlog.max(1),
        }
    }

    /// Events dropped because a subscriber's backlog was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn lanes(&self, db: &Db, cfg: &Arc<WebhookConfig>) -> Option<Arc<Lanes>> {
        if cfg.outbound_urls.is_empty() || self.closed.load(Ordering::SeqCst) {
            return None;
        }
        let mut lanes = self.lanes.lock().unwrap_or_else(|e| e.into_inner());
        if lanes.is_none() {
            let mut workers = self.workers.lock().unwrap_or_else(|e| e.into_inner());
            let mut all = Vec::with_capacity(cfg.outbound_urls.len());
            for url in &cfg.outbound_urls {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<OutboundEvent>(self.backlog);
                let db = db.clone();
                let one = WebhookConfig {
                    outbound_urls: vec![url.clone()],
                    ..(**cfg).clone()
                };
                workers.push(tokio::spawn(async move {
                    while let Some(e) = rx.recv().await {
                        dispatch(&db, &one, e).await;
                    }
                }));
                all.push(Lane {
                    url: url.clone(),
                    tx,
                });
            }
            *lanes = Some(Arc::new(Lanes {
                lanes: all,
                dropped: self.dropped.clone(),
            }));
        }
        lanes.clone()
    }

    /// Stop accepting events and wait up to `timeout` for every backlog to
    /// be delivered. Call after the server stopped taking requests.
    pub async fn drain(&self, timeout: Duration) {
        self.closed.store(true, Ordering::SeqCst);
        // Dropping the senders closes each channel once it is empty.
        drop(self.lanes.lock().unwrap_or_else(|e| e.into_inner()).take());
        let workers = std::mem::take(&mut *self.workers.lock().unwrap_or_else(|e| e.into_inner()));
        if tokio::time::timeout(timeout, futures_util::future::join_all(workers))
            .await
            .is_err()
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
pub struct Outbox(Option<Arc<Lanes>>);

impl Outbox {
    pub fn start(state: &AppState) -> Self {
        Self(state.outbound.lanes(&state.db, &state.webhooks))
    }

    /// Enqueue `event` for every subscriber without waiting. A subscriber
    /// whose backlog is full loses this event (logged and counted).
    pub fn send(&self, event: OutboundEvent) {
        let Some(lanes) = &self.0 else { return };
        for lane in &lanes.lanes {
            match lane.tx.try_send(event.clone()) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(e)) => {
                    lanes.dropped.fetch_add(1, Ordering::Relaxed);
                    warn!(
                        target: "ptask::webhook",
                        url = %lane.url,
                        event = %e.event_type,
                        "outbound webhook backlog full; event dropped for this subscriber"
                    );
                }
                // Closed only after shutdown.
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {}
            }
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
