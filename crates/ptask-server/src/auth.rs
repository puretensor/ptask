//! Optional application-level auth for mutating HTTP routes.
//!
//! Loopback `pt serve` keeps unauthenticated local-development compatibility
//! until a token is configured (the env token or any unrevoked named token),
//! and in that mode answers anonymous requests only when they address one of
//! the server's own names (DNS rebinding). Non-loopback listeners require API auth
//! (`PTASK_API_TOKEN` or a named token) and dashboard Basic auth unless the
//! explicit unauthenticated override is set, and never serve anonymous callers.
//! Machine-API callers send either:
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
/// mode — no env token configured and no unrevoked named token — as
/// `anonymous` (Write). An active named token closes anonymous access just as
/// the env token does, so finishing the rotation off `PTASK_API_TOKEN` does
/// not reopen every route.
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
            if auth.api_token.is_some() || auth.metrics_token.is_some() || auth.anonymous_forbidden
            {
                return Err(unauthorized());
            }
            // A DNS-rebinding page has no credential either: anonymous access
            // answers only to requests addressed to one of our own names.
            if headers.get(header::HOST).is_some_and(|h| {
                !h.to_str()
                    .is_ok_and(|h| host_allowed(h, &auth.allowed_hosts))
            }) {
                return Err(unauthorized());
            }
            match tokens::any_active(db) {
                Ok(false) => Identity {
                    client_id: "anonymous".into(),
                    scope: Scope::Write,
                },
                Ok(true) => return Err(unauthorized()),
                Err(e) => {
                    warn!(target: "ptask::auth", error = %e, "named-token check failed");
                    return Err(unauthorized());
                }
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
/// scrape stays open (back-compat); configuring either token closes anonymous
/// access to every gated route.
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
/// both machine-API bearer auth (the env token or an unrevoked named token)
/// and dashboard Basic auth configured, unless an operator explicitly sets
/// `PTASK_ALLOW_UNAUTHENTICATED=1` for a deliberately isolated deployment. The
/// dashboard routes are always mounted, so an API token alone does not make
/// the listener safe.
pub fn validate_bind_auth(
    addr: &SocketAddr,
    auth: &AuthConfig,
    dash: &DashConfig,
    named_tokens_active: bool,
) -> Result<(), String> {
    validate_bind_auth_state(
        addr,
        auth.api_token.is_some() || named_tokens_active,
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
        "refusing to bind {addr} without complete application auth; non-loopback listeners require machine-API auth ({API_TOKEN_ENV} or a named token from `pt token create`) and {DASH_PASS_ENV} for the always-mounted dashboard. Set the missing credential(s) or bind to 127.0.0.1. For an intentional isolated deployment only, set {ALLOW_UNAUTH_ENV}=1."
    ))
}

/// Emit a single loud warning at startup when no API credential at all is
/// configured (no env token, no named token), so an operator running
/// unauthenticated sees it once in the log without flooding it on every
/// `/metrics` scrape.
pub fn warn_if_unconfigured(auth: &AuthConfig, named_tokens_active: bool) {
    static WARNED: Once = Once::new();
    if auth.api_token.is_none() && !named_tokens_active {
        WARNED.call_once(|| {
            warn!(
                target: "ptask::auth",
                "no API token is configured ({} unset, no named tokens) — only loopback or {}=1 binds may run unauthenticated. \
                 Create one with `pt token create` (and send `Authorization: Bearer <token>` from callers) before exposing pt serve.",
                API_TOKEN_ENV, ALLOW_UNAUTH_ENV
            );
        });
    }
}

/// Compare two byte slices in time independent of where they first differ,
/// to avoid leaking the token via response-timing. Length difference short
/// circuits (an attacker already learns length from other channels), but equal
/// length inputs are always fully scanned.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// True when a Host header names this server rather than a stranger: IP
/// literals, `localhost`, `*.ts.net` (Tailscale serves that zone, so a page
/// cannot rebind one of its names to an address of its choosing), and the
/// configured list — which carries the machine's own short name and suffix
/// entries starting with ".". Every other dotless name is refused: a hostile
/// LAN can resolve one through its DHCP search domain, LLMNR or NBT-NS.
pub(crate) fn host_allowed(value: &str, extra: &[String]) -> bool {
    let host = value.trim().to_ascii_lowercase();
    if let Some(rest) = host.strip_prefix('[') {
        let Some((name, after)) = rest.split_once(']') else {
            return false;
        };
        if !after.is_empty() && !after.strip_prefix(':').is_some_and(valid_port) {
            return false;
        }
        return name.parse::<std::net::Ipv6Addr>().is_ok();
    }
    let (name, port) = match host.split_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (host.as_str(), None),
    };
    if port.is_some_and(|p| !valid_port(p)) {
        return false;
    }
    let name = name.trim_end_matches('.');
    if name.is_empty() {
        return false;
    }
    if name.parse::<std::net::Ipv4Addr>().is_ok() || name == "localhost" {
        return true;
    }
    if extra
        .iter()
        .any(|e| e == name || (e.starts_with('.') && name.ends_with(e.as_str())))
    {
        return true;
    }
    name.contains('.') && name.ends_with(".ts.net")
}

fn valid_port(port: &str) -> bool {
    (1..=5).contains(&port.len()) && port.bytes().all(|b| b.is_ascii_digit())
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
        let addr: SocketAddr = "10.0.0.10:9501".parse().unwrap();
        let err = validate_bind_auth_state(&addr, false, false, false).unwrap_err();
        assert!(err.contains(API_TOKEN_ENV));
        assert!(err.contains(DASH_PASS_ENV));
        assert!(err.contains("refusing"));
    }

    #[test]
    fn validate_bind_auth_rejects_non_loopback_with_only_api_token() {
        let addr: SocketAddr = "10.0.0.10:9501".parse().unwrap();
        assert!(validate_bind_auth_state(&addr, true, false, false).is_err());
    }

    #[test]
    fn validate_bind_auth_rejects_non_loopback_with_only_dashboard_password() {
        let addr: SocketAddr = "10.0.0.10:9501".parse().unwrap();
        assert!(validate_bind_auth_state(&addr, false, true, false).is_err());
    }

    #[test]
    fn validate_bind_auth_allows_non_loopback_with_both_credentials() {
        let addr: SocketAddr = "10.0.0.10:9501".parse().unwrap();
        assert!(validate_bind_auth_state(&addr, true, true, false).is_ok());
    }

    #[test]
    fn validate_bind_auth_counts_named_tokens_as_api_auth() {
        let addr: SocketAddr = "10.0.0.10:9501".parse().unwrap();
        let auth = AuthConfig::default();
        let dash = DashConfig {
            pass: Some("pw".into()),
            ..Default::default()
        };
        assert!(validate_bind_auth(&addr, &auth, &dash, true).is_ok());
        assert!(validate_bind_auth(&addr, &auth, &dash, false).is_err());
    }

    #[test]
    fn validate_bind_auth_allows_explicit_override() {
        let addr: SocketAddr = "0.0.0.0:9501".parse().unwrap();
        assert!(validate_bind_auth_state(&addr, false, false, true).is_ok());
    }
}
