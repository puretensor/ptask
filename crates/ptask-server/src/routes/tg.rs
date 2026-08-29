//! POST /tg/callback — execute a Telegram inline-button tap.
//!
//! The accountability nudges carry a Done / Snooze 3d / Dismiss inline
//! keyboard (v2.2.0) — the triage loop's missing actuator. The bot is
//! @hal_nexus_bot, whose `getUpdates` stream is owned by nexus; a second
//! poller would 409 and steal conversational updates, so nexus forwards
//! `pt*:` callback queries here and answers the tap with our result.
//!
//! Idempotent per tap: the Telegram callback id becomes the journal event
//! uuid, so a retried forward of the same tap is a no-op.

use crate::AppState;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use ptask_core::event_log::EventCtx;
use ptask_core::tokens::Scope;
use serde::Deserialize;

pub fn router() -> Router<AppState> {
    Router::new().route("/tg/callback", post(callback))
}

#[derive(Debug, Deserialize)]
pub struct CallbackReq {
    /// Raw callback_data: `ptdone:<uuid>` | `ptsnooze:<uuid>` | `ptdismiss:<uuid>`.
    pub data: String,
    /// Telegram callback query id — the idempotency key for this tap.
    pub callback_id: String,
}

fn err(status: StatusCode, msg: &str) -> axum::response::Response {
    (status, Json(serde_json::json!({"error": msg}))).into_response()
}

async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CallbackReq>,
) -> impl IntoResponse {
    crate::blocking::db_response(move || callback_blocking(state, headers, req)).await
}

fn callback_blocking(
    state: AppState,
    headers: HeaderMap,
    req: CallbackReq,
) -> axum::response::Response {
    if let Err(resp) = crate::auth::authenticate(&state.db, &state.auth, &headers, Scope::Write) {
        return resp;
    }
    let Some((verb, uuid)) = req.data.split_once(':') else {
        return err(StatusCode::BAD_REQUEST, "malformed callback data");
    };
    if !matches!(verb, "ptdone" | "ptsnooze" | "ptdismiss") {
        return err(StatusCode::BAD_REQUEST, "unknown callback verb");
    }
    if req.callback_id.is_empty() {
        return err(StatusCode::BAD_REQUEST, "callback_id must be non-empty");
    }

    let event_uuid = format!("tg-cb:{}", req.callback_id);
    let already: bool = match state.db.with_conn(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM pt_event_log WHERE uuid = ?1",
            [&event_uuid],
            |r| r.get::<_, i64>(0),
        )?)
    }) {
        Ok(n) => n > 0,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{}", e)),
    };

    let task = match ptask_core::tasks::resolve_for_lookup(&state.db, uuid, true) {
        Ok(t) => t,
        Err(_) => return err(StatusCode::NOT_FOUND, "task not found"),
    };
    let pt_id = task.pt_id.clone().unwrap_or_else(|| task.id.clone());

    if already {
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "ok": true, "duplicate": true, "verb": verb, "pt_id": pt_id,
            })),
        )
            .into_response();
    }

    // The operator tapped the button; Telegram is the acting surface.
    let ctx = EventCtx {
        actor: "telegram".into(),
        source: "tg-callback".into(),
        event_uuid: Some(event_uuid),
    };
    let result = match verb {
        "ptdone" => {
            ptask_core::tasks::mark_done(&state.db, &task, &ctx).map(|outcome| match outcome {
                ptask_core::tasks::DoneOutcome::Completed => "done".to_string(),
                ptask_core::tasks::DoneOutcome::Advanced { next_deadline } => {
                    format!("advanced:{next_deadline}")
                }
            })
        }
        "ptdismiss" => {
            ptask_core::tasks::dismiss(&state.db, &task.id, &ctx).map(|_| "dismissed".to_string())
        }
        "ptsnooze" => match snooze_until_3d() {
            Ok(until) => ptask_core::tasks::snooze(&state.db, &task.id, &until, &ctx)
                .map(|_| "snoozed".to_string()),
            Err(e) => Err(e),
        },
        _ => unreachable!(),
    };
    match result {
        Ok(outcome) => {
            tracing::info!(
                target: "ptask::tg",
                verb, pt_id = %pt_id, outcome = %outcome,
                "inline-button action executed"
            );
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "ok": true, "verb": verb, "pt_id": pt_id,
                    "outcome": outcome,
                    "title": task.title.chars().take(80).collect::<String>(),
                })),
            )
                .into_response()
        }
        Err(e) => err(StatusCode::UNPROCESSABLE_ENTITY, &format!("{}", e)),
    }
}

/// Now + 3 days, RFC3339 UTC — matches the "Snooze 3d" button label.
fn snooze_until_3d() -> ptask_core::Result<String> {
    let now = ptask_core::dates::now_in_operator_tz()?;
    let until = now
        .checked_add(ptask_core::jiff::Span::new().days(3))
        .map_err(|e| ptask_core::Error::Other(format!("snooze date math: {}", e)))?;
    Ok(ptask_core::dates::format_iso(
        &until.with_time_zone(ptask_core::jiff::tz::TimeZone::UTC),
    ))
}
