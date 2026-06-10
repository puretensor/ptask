//! Optional application-level auth for mutating HTTP routes.
//!
//! `pt serve` still defaults to unauthenticated localhost/Tailscale operation
//! for backwards compatibility. Set `PTASK_API_TOKEN` to require callers of
//! `/sync`, `/capture`, `/email`, and `/metrics` to send either:
//!   - `Authorization: Bearer <token>`
//!   - `X-PTask-Token: <token>`

use axum::Json;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::sync::Once;
use tracing::warn;

const API_TOKEN_ENV: &str = "PTASK_API_TOKEN";
const METRICS_TOKEN_ENV: &str = "PTASK_METRICS_TOKEN";
const TOKEN_HEADER: &str = "x-ptask-token";

pub fn require_write_token(headers: &HeaderMap) -> Option<Response> {
    let expected = configured_token(API_TOKEN_ENV)?;
    match presented_token(headers) {
        Some(token) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => None,
        _ => Some(unauthorized()),
    }
}

/// Read-path gate, used by `/metrics` (leaks task/store counts but mutates
/// nothing). Accepts `PTASK_METRICS_TOKEN` *or* the write token, so a
/// Prometheus scraper can hold a read-only credential instead of the
/// fleet-wide write token. Enforce-if-configured: with neither env set the
/// scrape stays open (back-compat); with only `PTASK_API_TOKEN` set the
/// behaviour is unchanged from before.
pub fn require_read_token(headers: &HeaderMap) -> Option<Response> {
    let allowed: Vec<String> = [
        configured_token(METRICS_TOKEN_ENV),
        configured_token(API_TOKEN_ENV),
    ]
    .into_iter()
    .flatten()
    .collect();
    if allowed.is_empty() {
        return None;
    }
    match presented_token(headers) {
        Some(token)
            if allowed
                .iter()
                .any(|t| constant_time_eq(token.as_bytes(), t.as_bytes())) =>
        {
            None
        }
        _ => Some(unauthorized()),
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error": "missing or invalid API token"})),
    )
        .into_response()
}

/// Emit a single loud warning at startup if `PTASK_API_TOKEN` is unset, so an
/// operator running unauthenticated sees it once in the log without flooding
/// it on every `/metrics` scrape. Mirrors the fail-open-but-warn-when-unset
/// posture: auth enforces only once a token is configured.
pub fn warn_if_unconfigured() {
    static WARNED: Once = Once::new();
    if configured_token(API_TOKEN_ENV).is_none() {
        WARNED.call_once(|| {
            warn!(
                target: "ptask::auth",
                "{} is unset — /sync, /capture, /email, and /metrics are UNAUTHENTICATED. \
                 Set {} (and send `Authorization: Bearer <token>` from callers) to enforce auth.",
                API_TOKEN_ENV, API_TOKEN_ENV
            );
        });
    }
}

/// Compare two byte slices in time independent of where they first differ,
/// to avoid leaking the token via response-timing. Length difference short
/// circuits (an attacker already learns length from other channels), but equal
/// length inputs are always fully scanned.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn configured_token(env: &str) -> Option<String> {
    std::env::var(env)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn presented_token(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        let trimmed = value.trim();
        if let Some(token) = trimmed
            .strip_prefix("Bearer ")
            .or_else(|| trimmed.strip_prefix("bearer "))
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(token.to_string());
        }
    }

    headers
        .get(TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
