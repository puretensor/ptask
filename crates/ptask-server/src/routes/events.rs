//! Machine-readable journal tail. Unlike the dashboard's invalidation stream,
//! this carries task references and a deliberately small, non-free-text payload.

use crate::AppState;
use axum::Router;
use axum::extract::{Query, State, rejection::QueryRejection};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use ptask_core::status::Status;
use ptask_core::tokens::Scope;
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/events", get(events))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsQuery {
    #[serde(default)]
    cursor: i64,
    #[serde(default = "default_limit")]
    limit: usize,
    snapshot: Option<i64>,
}

fn default_limit() -> usize {
    50
}

fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({"error": message, "items": [], "count": 0})),
    )
        .into_response()
}

/// Credentials are mandatory even in otherwise anonymous local-development mode.
async fn events(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<EventsQuery>, QueryRejection>,
) -> Response {
    crate::blocking::db_response(move || {
        // The parser `authenticate` itself uses, so this gate and the token
        // lookup agree on what counts as a presented credential.
        if crate::auth::presented_token(&headers).is_none() {
            return error(StatusCode::UNAUTHORIZED, "events require a machine read token");
        }
        match crate::auth::authenticate(&state.db, &state.auth, &headers, Scope::Read) {
            Ok(_) => {}
            Err(response) => return response,
        }
        let q = match query {
            Ok(Query(q))
                if q.cursor >= 0
                    && (1..=200).contains(&q.limit)
                    && q.snapshot.is_none_or(|s| s >= q.cursor) => q,
            _ => return error(StatusCode::BAD_REQUEST,
                "valid parameters: cursor >= 0, limit 1..200, optional snapshot >= cursor; unknown parameters are rejected"),
        };
        let result = state.db.with_conn(|conn| {
            // One read transaction fixes the denominator and high-water mark.
            let tx = conn.unchecked_transaction()?;
            let (oldest, current): (i64, i64) = tx.query_row(
                "SELECT COALESCE(MIN(id),0), COALESCE(MAX(id),0) FROM pt_event_log", [],
                |r| Ok((r.get(0)?, r.get(1)?)))?;
            let snapshot = q.snapshot.unwrap_or(current);
            // Parameters already hold snapshot >= cursor. Anything past MAX(id)
            // means the journal moved backwards (a restore), so resync.
            if q.cursor > current || snapshot > current {
                return Ok(None);
            }
            let total: i64 = tx.query_row(
                "SELECT COUNT(*) FROM pt_event_log WHERE id > ?1 AND id <= ?2",
                params![q.cursor, snapshot], |r| r.get(0))?;
            let mut stmt = tx.prepare(
                "SELECT id,uuid,task_uuid,event_type,actor,ts,payload FROM pt_event_log
                 WHERE id > ?1 AND id <= ?2 ORDER BY id LIMIT ?3")?;
            let rows = stmt.query_map(params![q.cursor, snapshot, q.limit as i64], |r| {
                let payload: String = r.get(6)?;
                let raw: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
                let (safe, redacted) = safe_payload(&raw);
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "event_id": r.get::<_, String>(1)?,
                    "task_id": r.get::<_, Option<String>>(2)?,
                    "event_type": r.get::<_, String>(3)?,
                    "actor": r.get::<_, Option<String>>(4)?,
                    "observed_at": r.get::<_, String>(5)?, "source": "ptask",
                    "payload_redacted": redacted,
                    "payload": safe,
                }))
            })?;
            let items = rows.collect::<std::result::Result<Vec<_>, _>>()?;
            let next = items.last().and_then(|v| v["id"].as_i64()).unwrap_or(q.cursor);
            let resync = oldest > 1 && q.cursor < oldest - 1;
            Ok(Some(json!({
                "items": items, "count": items.len(), "total": total,
                "cursor": q.cursor, "next_cursor": next, "snapshot_cursor": snapshot,
                "oldest_cursor": oldest, "has_more": next < snapshot && total > items.len() as i64,
                "resync": resync,
                "retention": "journal retained by server; resync when history before oldest_cursor is missing",
                "help": ["Pass snapshot=snapshot_cursor on later pages; restart without snapshot to tail new events.",
                         "On resync, refresh authoritative task state; missing history cannot be reconstructed."],
            })))
        });
        match result {
            Ok(Some(value)) => Json(value).into_response(),
            Ok(None) => (StatusCode::CONFLICT, Json(json!({
                "error": "cursor or snapshot is ahead of the current journal; restart at cursor=0",
                "resync": true, "items": [], "count": 0,
            }))).into_response(),
            Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "event journal unavailable"),
        }
    }).await
}

/// Keys that are not event content. `event_log::record_in_conn` adds `actor`,
/// `source` and (for keyed commands) `cmd` to every stored payload, and the task
/// references duplicate the row's `task_id`. Dropping them does not make a row
/// redacted; otherwise every real event would carry `payload_redacted:true`.
const ENVELOPE_KEYS: [&str; 5] = ["actor", "source", "cmd", "task_uuid", "pt_id"];

/// Never export descriptions, prompts, notes, command arguments or arbitrary nested
/// values. The authorized consumer can explicitly fetch task details when needed.
///
/// Returns the exported payload and whether any content key was withheld (a key
/// outside the allowlist, or an allowlisted key whose value is not exportable).
/// A payload that is not a JSON object counts as withheld.
fn safe_payload(raw: &Value) -> (serde_json::Map<String, Value>, bool) {
    let mut result = serde_json::Map::new();
    let Some(obj) = raw.as_object() else {
        return (result, true);
    };
    let mut redacted = false;
    for (key, value) in obj {
        if ENVELOPE_KEYS.contains(&key.as_str()) {
            continue;
        }
        match safe_value(key, value) {
            Some(safe) => {
                result.insert(key.clone(), safe);
            }
            None => redacted = true,
        }
    }
    (result, redacted)
}

fn safe_value(key: &str, value: &Value) -> Option<Value> {
    match key {
        "status" | "old_status" | "new_status" | "from" | "to" => {
            value.as_str().and_then(export_status).map(Value::from)
        }
        "priority" | "old_priority" | "new_priority" => value
            .as_i64()
            .filter(|n| (1..=5).contains(n))
            .map(Value::from),
        _ => None,
    }
}

/// The v2 status vocabulary, normalised through the core model, plus the two
/// legacy-column values with no v2 spelling (`Status::legacy`). Only these
/// fixed words are emitted, so no caller-supplied text passes through.
fn export_status(s: &str) -> Option<&'static str> {
    if let Ok(status) = Status::parse(s) {
        return Some(status.as_str());
    }
    match s {
        "pending" => Some("pending"),
        "delayed" => Some("delayed"),
        _ => None,
    }
}
