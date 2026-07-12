//! Optional application-level auth for mutating HTTP routes.
//!
//! Loopback `pt serve` keeps unauthenticated local-development compatibility.
//! Non-loopback listeners require both `PTASK_API_TOKEN` and dashboard Basic
//! auth unless the explicit unauthenticated override is set. The API token
//! requires machine-API callers to send either:
//!   - `Authorization: Bearer <token>`
//!   - `X-PTask-Token: <token>`

use axum::Json;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use ptask_core::Db;
use ptask_core::config::{AuthConfig, DashConfig};
use ptask_core::tokens::{self, Identity, Scope};
use std::net::SocketAddr;
use std::sync::Once;
use tracing::warn;

const API_TOKEN_ENV: &str = "PTASK_API_TOKEN";
const DASH_PASS_ENV: &str = "PTASK_DASH_PASS";
const ALLOW_UNAUTH_ENV: &str = "PTASK_ALLOW_UNAUTHENTICATED";
const TOKEN_HEADER: &str = "x-ptask-token";

/// Resolve the caller to an [`Identity`] with at least `required` scope.
///
/// Resolution order for a presented credential:
///   1. the legacy env write token  → `legacy-env` (Write)
///   2. the env metrics token       → `metrics-scraper` (Read)
///   3. `pt_api_tokens` hash lookup → named identity + its scope
///
/// No credential presented: allowed only in unauthenticated back-compat
/// mode (no env write token configured) as `anonymous` (Write) — identical
/// enforcement semantics to the pre-v1.17 single-token gate.
#[allow(clippy::result_large_err)] // the Err IS the ready-made 401 Response
pub fn authenticate(
    db: &Db,
    auth: &AuthConfig,
    headers: &HeaderMap,
    required: Scope,
) -> std::result::Result<Identity, Response> {
    let identity = match presented_token(headers) {
        Some(token) => {
            if auth
                .api_token
                .as_deref()
                .is_some_and(|t| constant_time_eq(token.as_bytes(), t.as_bytes()))
            {
                Identity {
                    client_id: "legacy-env".into(),
                    scope: Scope::Write,
                }
            } else if auth
                .metrics_token
                .as_deref()
                .is_some_and(|t| constant_time_eq(token.as_bytes(), t.as_bytes()))
            {
                Identity {
                    client_id: "metrics-scraper".into(),
                    scope: Scope::Read,
                }
            } else {
                match tokens::resolve(db, &token) {
                    Ok(Some(id)) => id,
                    Ok(None) => return Err(unauthorized()),
                    Err(e) => {
                        warn!(target: "ptask::auth", error = %e, "token lookup failed");
                        return Err(unauthorized());
                    }
                }
            }
        }
        None => {
            if auth.api_token.is_none() {
                Identity {
                    client_id: "anonymous".into(),
                    scope: Scope::Write,
                }
            } else {
                return Err(unauthorized());
            }
        }
    };
    if identity.scope < required {
        return Err(unauthorized());
    }
    Ok(identity)
}

/// Back-compat shim: `None` = authorized (write scope). Prefer
/// [`authenticate`] where the caller identity is needed.
pub fn require_write_token(db: &Db, auth: &AuthConfig, headers: &HeaderMap) -> Option<Response> {
    authenticate(db, auth, headers, Scope::Write).err()
}

/// Read-path gate, used by `/metrics` (leaks task/store counts but mutates
/// nothing). Accepts `PTASK_METRICS_TOKEN` *or* the write token, so a
/// Prometheus scraper can hold a read-only credential instead of the
/// fleet-wide write token. Enforce-if-configured: with neither env set the
/// scrape stays open (back-compat); with only `PTASK_API_TOKEN` set the
/// behaviour is unchanged from before.
pub fn require_read_token(db: &Db, auth: &AuthConfig, headers: &HeaderMap) -> Option<Response> {
    authenticate(db, auth, headers, Scope::Read).err()
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
/// both machine-API bearer auth and dashboard Basic auth configured, unless an
/// operator explicitly sets `PTASK_ALLOW_UNAUTHENTICATED=1` for a deliberately
/// isolated deployment. The dashboard routes are always mounted, so an API
/// token alone does not make the listener safe.
pub fn validate_bind_auth(
    addr: &SocketAddr,
    auth: &AuthConfig,
    dash: &DashConfig,
) -> Result<(), String> {
    validate_bind_auth_state(
        addr,
        auth.api_token.is_some(),
        dash.pass.is_some(),
        auth.allow_unauthenticated,
    )
}

fn validate_bind_auth_state(
    addr: &SocketAddr,
    api_token_configured: bool,
    dash_password_configured: bool,
    allow_unauthenticated: bool,
) -> Result<(), String> {
    if addr.ip().is_loopback()
        || (api_token_configured && dash_password_configured)
        || allow_unauthenticated
    {
        return Ok(());
    }

    Err(format!(
        "refusing to bind {addr} without complete application auth; non-loopback listeners require both {API_TOKEN_ENV} for machine APIs and {DASH_PASS_ENV} for the always-mounted dashboard. Set the missing credential(s) or bind to 127.0.0.1. For an intentional isolated deployment only, set {ALLOW_UNAUTH_ENV}=1."
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
        assert!(validate_bind_auth_state(&addr, false, false, false).is_ok());
    }

    #[test]
    fn validate_bind_auth_rejects_non_loopback_without_token() {
        let addr: SocketAddr = "100.121.42.54:9501".parse().unwrap();
        let err = validate_bind_auth_state(&addr, false, false, false).unwrap_err();
        assert!(err.contains(API_TOKEN_ENV));
        assert!(err.contains(DASH_PASS_ENV));
        assert!(err.contains("refusing"));
    }

    #[test]
    fn validate_bind_auth_rejects_non_loopback_with_only_api_token() {
        let addr: SocketAddr = "100.121.42.54:9501".parse().unwrap();
        assert!(validate_bind_auth_state(&addr, true, false, false).is_err());
    }

    #[test]
    fn validate_bind_auth_rejects_non_loopback_with_only_dashboard_password() {
        let addr: SocketAddr = "100.121.42.54:9501".parse().unwrap();
        assert!(validate_bind_auth_state(&addr, false, true, false).is_err());
    }

    #[test]
    fn validate_bind_auth_allows_non_loopback_with_both_credentials() {
        let addr: SocketAddr = "100.121.42.54:9501".parse().unwrap();
        assert!(validate_bind_auth_state(&addr, true, true, false).is_ok());
    }

    #[test]
    fn validate_bind_auth_allows_explicit_override() {
        let addr: SocketAddr = "0.0.0.0:9501".parse().unwrap();
        assert!(validate_bind_auth_state(&addr, false, false, true).is_ok());
    }
}
