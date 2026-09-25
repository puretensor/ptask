//! Outbound HMAC-signed webhook dispatch.
//!
//! Config comes from `AppState.webhooks` (populated by the entrypoint's
//! `Config::from_env`): `outbound_urls` = comma-separated POST targets,
//! `outbound_secret` = HMAC-SHA256 shared secret.
//!
//! Each event becomes one POST per URL with body:
//!   { "event_type": "...", "task_uuid": "...?", "payload": {...}, "ts": "<iso>" }
//! and header `X-PTask-Signature: sha256=<hex>` over the raw body bytes.
//!
//! A request streams its committed events into an [`Outbox`]; one background
//! task per request delivers them in order. Inline delivery made the request
//! wait on every subscriber: a hung URL cost 10s per event, so a 200-command
//! `/sync` could stall for over half an hour. Events already handed over
//! still go out if the client disconnects mid-batch.

use crate::AppState;
use hmac::{Hmac, Mac};
use ptask_core::webhook_log::{Direction, record};
use sha2::Sha256;
use std::sync::OnceLock;
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
pub struct OutboundEvent {
    pub event_type: String,
    pub task_uuid: Option<String>,
    pub payload: serde_json::Value,
}

/// One request's outbound queue. Delivery runs on its own task, so the
/// request never waits on a subscriber and a dropped request still drains
/// what it sent. A no-op when no URL is configured.
pub struct Outbox(Option<tokio::sync::mpsc::UnboundedSender<OutboundEvent>>);

impl Outbox {
    pub fn start(state: &AppState) -> Self {
        if state.webhooks.outbound_urls.is_empty() {
            return Self(None);
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<OutboundEvent>();
        let state = state.clone();
        tokio::spawn(async move {
            while let Some(e) = rx.recv().await {
                dispatch(&state, &e.event_type, e.task_uuid.as_deref(), &e.payload).await;
            }
        });
        Self(Some(tx))
    }

    pub fn send(&self, event: OutboundEvent) {
        if let Some(tx) = &self.0 {
            // The receiver lives until this sender drops.
            let _ = tx.send(event);
        }
    }
}

/// Fan-out one event to every configured URL. Logs each attempt (sent or
/// failed) to pt_webhook_log. No retries.
async fn dispatch(
    state: &AppState,
    event_type: &str,
    task_uuid: Option<&str>,
    payload: &serde_json::Value,
) {
    let cfg = &state.webhooks;
    if cfg.outbound_urls.is_empty() {
        return;
    }
    let ts = ptask_core::dates::format_iso(
        &ptask_core::dates::now_in_operator_tz().unwrap_or_else(|_| {
            // Should never fail; fall back to UTC to avoid swallowing the event.
            jiff::Zoned::now().with_time_zone(jiff::tz::TimeZone::UTC)
        }),
    );
    let envelope = serde_json::json!({
        "event_type": event_type,
        "task_uuid": task_uuid,
        "payload": payload,
        "ts": ts,
    });
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
        let db = state.db.clone();
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
