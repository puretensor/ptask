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
//! The dispatch is awaited inline in the originating request — small fleet,
//! few subscribers, easier to debug. Promote to a background queue if it
//! ever bites.

use crate::AppState;
use hmac::{Hmac, Mac};
use ptask_core::webhook_log::{Direction, record};
use sha2::Sha256;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::{info, warn};

type HmacSha256 = Hmac<Sha256>;

/// Shared outbound client. Dispatch is awaited inline in the originating
/// request, so a hung subscriber must not be able to stall `/sync` forever:
/// reqwest's default client has NO timeout. Bound both connect and total
/// request time, and reuse the client (connection pool) across dispatches.
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

/// Fan-out one event to every configured URL. Logs each attempt (sent or
/// failed) to pt_webhook_log. No retries in v0.3.5.
pub async fn dispatch(
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
        let _ = record(&state.db, Direction::Out, url, &envelope, outcome);
    }
}
