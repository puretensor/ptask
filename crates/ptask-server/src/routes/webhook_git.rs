//! Inbound git webhooks (Gitea + GitHub).
//!
//! Both providers send the same shape: a push event with `commits[]` each
//! carrying a `message`. We HMAC-verify the raw body bytes against the
//! configured secret, parse the JSON, then run every commit message
//! through `ptask_core::magic_words::parse` and route Closes/Fixes
//! directives through `tasks::mark_done`.
//!
//! Config (env-driven):
//!   PTASK_GITEA_WEBHOOK_SECRET    shared secret for Gitea (`X-Gitea-Signature`)
//!   PTASK_GITHUB_WEBHOOK_SECRET   shared secret for GitHub (`X-Hub-Signature-256`)
//!
//! Every event (verified or not) gets logged to pt_webhook_log with the
//! `signature_ok` flag set accordingly.

use crate::AppState;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use hmac::{Hmac, Mac};
use ptask_core::magic_words;
use ptask_core::tasks::{self, DoneOutcome};
use ptask_core::webhook_log::{Direction, record as log_webhook};
use serde::Deserialize;
use sha2::Sha256;
use tracing::{info, warn};

type HmacSha256 = Hmac<Sha256>;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/webhook/gitea", post(gitea))
        .route("/webhook/github", post(github))
}

#[derive(Debug, Deserialize)]
struct PushEvent {
    #[serde(default)]
    pub commits: Vec<PushCommit>,
    /// PR-style payloads put the title elsewhere; v0.5.x scans commits only.
    #[serde(default, rename = "ref")]
    #[allow(dead_code)]
    pub git_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PushCommit {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub id: String,
}

async fn gitea(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let sig = headers
        .get("X-Gitea-Signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let secret = std::env::var("PTASK_GITEA_WEBHOOK_SECRET").unwrap_or_default();
    handle("gitea", &state, &body, &secret, sig).await
}

async fn github(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // GitHub uses `sha256=<hex>` prefix.
    let sig_raw = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let sig = sig_raw.strip_prefix("sha256=").unwrap_or(sig_raw);
    let secret = std::env::var("PTASK_GITHUB_WEBHOOK_SECRET").unwrap_or_default();
    handle("github", &state, &body, &secret, sig).await
}

async fn handle(
    source: &str,
    state: &AppState,
    body: &[u8],
    secret: &str,
    signature_hex: &str,
) -> axum::response::Response {
    let signature_ok = verify_hmac(body, secret, signature_hex);
    let envelope_json: serde_json::Value =
        serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
    let _ = log_webhook(
        &state.db,
        Direction::In,
        source,
        &envelope_json,
        signature_ok,
    );

    if !signature_ok {
        warn!(target: "ptask::webhook", source, "signature verification failed");
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid signature"})),
        )
            .into_response();
    }

    let event: PushEvent = match serde_json::from_slice(body) {
        Ok(e) => e,
        Err(e) => {
            warn!(target: "ptask::webhook", source, error = %e, "push event parse failed");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("parse: {}", e)})),
            )
                .into_response();
        }
    };

    let mut closed: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for commit in &event.commits {
        let directives = magic_words::parse(&commit.message);
        if directives.is_empty() {
            continue;
        }
        for pt_id in magic_words::pt_ids_to_close(&directives) {
            match tasks::resolve(&state.db, &pt_id) {
                Ok(t) => match tasks::mark_done(&state.db, &t) {
                    Ok(DoneOutcome::Completed) => {
                        closed.push(format!("{}=done", pt_id));
                    }
                    Ok(DoneOutcome::Advanced { next_deadline }) => {
                        closed.push(format!("{}=advanced→{}", pt_id, next_deadline));
                    }
                    Err(e) => errors.push(format!("{}: {}", pt_id, e)),
                },
                Err(e) => errors.push(format!("{}: {}", pt_id, e)),
            }
        }
        info!(
            target: "ptask::webhook",
            source,
            commit = %commit.id.chars().take(12).collect::<String>(),
            directives = directives.len(),
            "processed commit"
        );
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "closed": closed,
            "errors": errors,
        })),
    )
        .into_response()
}

/// HMAC-SHA256(body, secret) compared in constant time to `signature_hex`.
/// Empty secret → reject all signatures (we don't want a misconfigured
/// install to silently accept everything).
fn verify_hmac(body: &[u8], secret: &str, signature_hex: &str) -> bool {
    if secret.is_empty() || signature_hex.is_empty() {
        return false;
    }
    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let want = match hex::decode(signature_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };
    mac.verify_slice(&want).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_rejects_empty_secret() {
        assert!(!verify_hmac(b"x", "", "deadbeef"));
    }

    #[test]
    fn verify_rejects_empty_signature() {
        assert!(!verify_hmac(b"x", "secret", ""));
    }

    #[test]
    fn verify_accepts_known_answer() {
        // HMAC-SHA256("hello world", "secret-key") — same vector used in
        // ptask-server::webhooks::tests::webhook_sign_is_stable_hex.
        let sig = "095d5a21fe6d0646db223fdf3de6436bb8dfb2fab0b51677ecf6441fcf5f2a67";
        assert!(verify_hmac(b"hello world", "secret-key", sig));
    }

    #[test]
    fn verify_rejects_tampered_body() {
        let sig = "095d5a21fe6d0646db223fdf3de6436bb8dfb2fab0b51677ecf6441fcf5f2a67";
        assert!(!verify_hmac(b"hello WORLD", "secret-key", sig));
    }
}
