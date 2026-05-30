//! POST /email — inbound email capture.
//!
//! Accepts a raw RFC 822 message body (Content-Type: `message/rfc822` or
//! `text/plain`). Mail-parser extracts Subject + body; the text is dropped
//! into `raw_items` as a capture with `source='email'`. The Python distill
//! pipeline picks it up downstream (until v0.9).
//!
//! For provider-shaped JSON envelopes (Mailgun, Postmark, SendGrid) deploy
//! a tiny forwarder upstream that hands us the raw `.eml` — keeps this
//! endpoint provider-agnostic.

use crate::AppState;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use mail_parser::MessageParser;
use serde::Serialize;

pub fn router() -> Router<AppState> {
    Router::new().route("/email", post(email))
}

#[derive(Debug, Serialize)]
pub struct EmailResp {
    pub id: i64,
    pub subject: String,
    pub source_file: String,
}

async fn email(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Some(resp) = crate::auth::require_write_token(&headers) {
        return resp;
    }
    let Some(msg) = MessageParser::default().parse(&body[..]) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "could not parse RFC 822 message"})),
        )
            .into_response();
    };
    let subject = msg.subject().unwrap_or("(no subject)").to_string();
    let body_text = msg.body_text(0).map(|s| s.to_string()).unwrap_or_default();
    let message_id = msg.message_id().unwrap_or("none").to_string();

    // Compose the raw_items text: subject + blank line + body. Distill's
    // speech-act classifier handles the rest. Keep it short — anything too
    // long gets truncated by the LLM downstream anyway.
    let text = if body_text.is_empty() {
        subject.clone()
    } else {
        format!("{}\n\n{}", subject, body_text)
    };
    let source_file = format!("email:{}", message_id);

    match ptask_core::raw_items::insert(&state.db, &text, "email", &source_file) {
        Ok(r) => (
            StatusCode::CREATED,
            Json(EmailResp {
                id: r.id,
                subject,
                source_file: r.source_file,
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(target: "ptask::email", error = %e, "insert failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{}", e)})),
            )
                .into_response()
        }
    }
}
