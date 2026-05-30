//! Optional application-level auth for mutating HTTP routes.
//!
//! `pt serve` still defaults to unauthenticated localhost/Tailscale operation
//! for backwards compatibility. Set `PTASK_API_TOKEN` to require callers of
//! `/sync`, `/capture`, and `/email` to send either:
//!   - `Authorization: Bearer <token>`
//!   - `X-PTask-Token: <token>`

use axum::Json;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

const API_TOKEN_ENV: &str = "PTASK_API_TOKEN";
const TOKEN_HEADER: &str = "x-ptask-token";

pub fn require_write_token(headers: &HeaderMap) -> Option<Response> {
    let expected = configured_token()?;
    match presented_token(headers) {
        Some(token) if token == expected => None,
        _ => Some(
            (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "missing or invalid API token"})),
            )
                .into_response(),
        ),
    }
}

fn configured_token() -> Option<String> {
    std::env::var(API_TOKEN_ENV)
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
