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
use ptask_core::config::AuthConfig;
use std::net::SocketAddr;
use std::sync::Once;
use tracing::warn;

const API_TOKEN_ENV: &str = "PTASK_API_TOKEN";
const ALLOW_UNAUTH_ENV: &str = "PTASK_ALLOW_UNAUTHENTICATED";
const TOKEN_HEADER: &str = "x-ptask-token";

pub fn require_write_token(auth: &AuthConfig, headers: &HeaderMap) -> Option<Response> {
    let expected = auth.api_token.as_deref()?;
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
pub fn require_read_token(auth: &AuthConfig, headers: &HeaderMap) -> Option<Response> {
    let allowed: Vec<&str> = [auth.metrics_token.as_deref(), auth.api_token.as_deref()]
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

/// Refuse externally reachable unauthenticated API listeners by default.
///
/// Loopback keeps the old local-dev behaviour. Any non-loopback bind must have
/// `PTASK_API_TOKEN` configured, unless an operator explicitly sets
/// `PTASK_ALLOW_UNAUTHENTICATED=1` for a deliberately isolated deployment.
pub fn validate_bind_auth(addr: &SocketAddr, auth: &AuthConfig) -> Result<(), String> {
    validate_bind_auth_state(addr, auth.api_token.is_some(), auth.allow_unauthenticated)
}

fn validate_bind_auth_state(
    addr: &SocketAddr,
    api_token_configured: bool,
    allow_unauthenticated: bool,
) -> Result<(), String> {
    if addr.ip().is_loopback() || api_token_configured || allow_unauthenticated {
        return Ok(());
    }

    Err(format!(
        "{} is unset while binding {addr}; refusing to expose /sync, /capture, /email, and read APIs without application auth. Set {} or bind to 127.0.0.1. For an intentional isolated deployment only, set {}=1.",
        API_TOKEN_ENV, API_TOKEN_ENV, ALLOW_UNAUTH_ENV
    ))
}

/// Emit a single loud warning at startup if `PTASK_API_TOKEN` is unset, so an
/// operator running unauthenticated sees it once in the log without flooding
/// it on every `/metrics` scrape. Mirrors the fail-open-but-warn-when-unset
/// posture: auth enforces only once a token is configured.
pub fn warn_if_unconfigured(auth: &AuthConfig) {
    static WARNED: Once = Once::new();
    if auth.api_token.is_none() {
        WARNED.call_once(|| {
            warn!(
                target: "ptask::auth",
                "{} is unset — only loopback or {}=1 binds may run unauthenticated. \
                 Set {} (and send `Authorization: Bearer <token>` from callers) before exposing pt serve.",
                API_TOKEN_ENV, ALLOW_UNAUTH_ENV, API_TOKEN_ENV
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_bind_auth_allows_loopback_without_token() {
        let addr: SocketAddr = "127.0.0.1:9501".parse().unwrap();
        assert!(validate_bind_auth_state(&addr, false, false).is_ok());
    }

    #[test]
    fn validate_bind_auth_rejects_non_loopback_without_token() {
        let addr: SocketAddr = "100.121.42.54:9501".parse().unwrap();
        let err = validate_bind_auth_state(&addr, false, false).unwrap_err();
        assert!(err.contains(API_TOKEN_ENV));
        assert!(err.contains("refusing"));
    }

    #[test]
    fn validate_bind_auth_allows_non_loopback_with_token() {
        let addr: SocketAddr = "100.121.42.54:9501".parse().unwrap();
        assert!(validate_bind_auth_state(&addr, true, false).is_ok());
    }

    #[test]
    fn validate_bind_auth_allows_explicit_override() {
        let addr: SocketAddr = "0.0.0.0:9501".parse().unwrap();
        assert!(validate_bind_auth_state(&addr, false, true).is_ok());
    }
}
