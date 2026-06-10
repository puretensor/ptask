//! POST /sync — Todoist-style sync API.
//!
//! Request:
//!   {
//!     "sync_token": "<opaque integer string>",   // "*" or absent = full sync
//!     "resource_types": ["tasks"],               // optional; currently advisory
//!     "commands": [
//!       { "type": "task_create",
//!         "uuid": "<idempotency-key>",
//!         "temp_id": "<client-side>",
//!         "args": { "text": "<quick-add input>" } },
//!       { "type": "task_done",
//!         "uuid": "<idempotency-key>",
//!         "args": { "pt_id": "PT-42" } | { "task_uuid": "<uuid>" } },
//!     ]
//!   }
//!
//! Response:
//!   {
//!     "sync_token": "<new opaque>",
//!     "resources": { "tasks": [<Task>, ...] },
//!     "sync_status": { "<command-uuid>": "ok" | { "error": "..." } },
//!     "temp_id_mapping": { "<temp_id>": "<real task uuid>" }
//!   }

use crate::AppState;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use ptask_core::event_log;
use ptask_core::tasks::{self, DoneOutcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use tracing::warn;

pub fn router() -> Router<AppState> {
    Router::new().route("/sync", post(sync))
}

#[derive(Debug, Deserialize)]
pub struct SyncReq {
    #[serde(default)]
    pub sync_token: Option<String>,
    /// Advisory only in v0.3.4 — accepted to match the Todoist shape so
    /// future client code doesn't have to be rewritten when we honour it.
    #[serde(default)]
    #[allow(dead_code)]
    pub resource_types: Vec<String>,
    #[serde(default)]
    pub commands: Vec<Command>,
}

#[derive(Debug, Deserialize)]
pub struct Command {
    #[serde(rename = "type")]
    pub kind: String,
    pub uuid: String,
    #[serde(default)]
    pub temp_id: Option<String>,
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Serialize)]
pub struct SyncResp {
    pub sync_token: String,
    pub resources: Resources,
    pub sync_status: BTreeMap<String, Value>,
    pub temp_id_mapping: BTreeMap<String, String>,
    /// Tombstones: task uuids deleted since the client's cursor. Empty on
    /// full sync (the full task set replaces client state wholesale).
    #[serde(default)]
    pub deleted_task_uuids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Resources {
    pub tasks: Vec<tasks::Task>,
}

async fn sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SyncReq>,
) -> impl IntoResponse {
    if let Some(resp) = crate::auth::require_write_token(&headers) {
        return resp;
    }
    let mut status: BTreeMap<String, Value> = BTreeMap::new();
    let mut temp_map: BTreeMap<String, String> = BTreeMap::new();

    // Apply commands sequentially. Each command's `uuid` is its idempotency
    // key — replays return "ok" without re-executing.
    for cmd in &req.commands {
        // A failed idempotency lookup must NOT fall through to apply: if the
        // command was already executed, re-applying double-creates. Surface
        // the error and let the client retry the whole command instead.
        let prior = match event_log::get_by_uuid(&state.db, &cmd.uuid) {
            Ok(p) => p,
            Err(e) => {
                warn!(target: "ptask::sync", error = %e, uuid = %cmd.uuid, "idempotency lookup failed");
                status.insert(
                    cmd.uuid.clone(),
                    serde_json::json!({ "error": format!("idempotency lookup failed: {e}") }),
                );
                continue;
            }
        };
        if let Some(event) = prior {
            replay_temp_mapping(cmd, &event, &mut temp_map);
            status.insert(cmd.uuid.clone(), Value::String("ok".into()));
            continue;
        }
        // The mutation itself records the event row in its own transaction
        // (atomic, keyed on cmd.uuid) — no post-hoc event_log::record here.
        match apply_command(&state, cmd) {
            Ok((task_uuid, payload)) => {
                if let (Some(temp), Some(tu)) = (cmd.temp_id.as_ref(), task_uuid.as_ref()) {
                    temp_map.insert(temp.clone(), tu.clone());
                }
                // Outbound webhook fan-out (env-driven; no-op if unconfigured).
                crate::webhooks::dispatch(
                    &state,
                    &payload.event_type,
                    task_uuid.as_deref(),
                    &payload.payload,
                )
                .await;
                status.insert(cmd.uuid.clone(), Value::String("ok".into()));
            }
            Err(e) => {
                status.insert(
                    cmd.uuid.clone(),
                    serde_json::json!({ "error": format!("{}", e) }),
                );
            }
        }
    }

    // Delta: tasks touched since the supplied cursor. Full-sync sentinel "*"
    // or a missing/zero token returns everything.
    let (full_sync, since): (bool, i64) = match req.sync_token.as_deref() {
        None | Some("*") | Some("") => (true, 0),
        Some(s) => {
            let parsed = s.parse().unwrap_or(0);
            (parsed <= 0, parsed)
        }
    };
    // Snapshot the cursor BEFORE reading the delta. Events committed by a
    // concurrent writer between these two reads are then re-delivered on the
    // next sync (at-least-once) instead of being skipped forever, which is
    // what the previous read-delta-then-cursor order did (the token advanced
    // past events this response never contained).
    let new_cursor = event_log::current_cursor(&state.db).unwrap_or(0);
    let (delta_tasks, deleted_task_uuids): (Vec<tasks::Task>, Vec<String>) = if full_sync {
        (tasks::list_all(&state.db).unwrap_or_default(), Vec::new())
    } else {
        let delta_uuids = event_log::changed_task_uuids_since(&state.db, since).unwrap_or_default();
        let deleted = event_log::deleted_task_uuids_since(&state.db, since).unwrap_or_default();
        let mut rows = Vec::new();
        for u in &delta_uuids {
            // Deleted tasks legitimately fail the row fetch — they're
            // reported through the tombstone list instead.
            if let Ok(t) = task_by_uuid(&state.db, u) {
                rows.push(t);
            }
        }
        (rows, deleted)
    };

    (
        StatusCode::OK,
        Json(SyncResp {
            sync_token: new_cursor.to_string(),
            resources: Resources { tasks: delta_tasks },
            sync_status: status,
            temp_id_mapping: temp_map,
            deleted_task_uuids,
        }),
    )
        .into_response()
}

struct EventPayload {
    event_type: String,
    payload: Value,
}

fn replay_temp_mapping(
    cmd: &Command,
    event: &event_log::LoggedEvent,
    temp_map: &mut BTreeMap<String, String>,
) {
    if cmd.kind == "task_create"
        && let (Some(temp_id), Some(task_uuid)) = (cmd.temp_id.as_ref(), event.task_uuid.as_ref())
    {
        temp_map.insert(temp_id.clone(), task_uuid.clone());
    }
}

/// Apply one command. Returns the (task_uuid, event_payload) so the caller
/// can record into pt_event_log.
fn apply_command(
    state: &AppState,
    cmd: &Command,
) -> Result<(Option<String>, EventPayload), anyhow::Error> {
    match cmd.kind.as_str() {
        "task_create" => {
            let text = cmd
                .args
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("task_create: args.text required"))?;
            let q = ptask_core::quickadd::parse(text)?;
            let new = ptask_core::NewTask {
                title: q.title.clone(),
                description: q.description.clone(),
                priority: q.priority.unwrap_or(2),
                deadline: q.deadline.clone(),
                source_type: cmd
                    .args
                    .get("source_type")
                    .and_then(Value::as_str)
                    .unwrap_or("sync")
                    .into(),
                ai_confidence: 1.0,
                ai_reasoning: String::new(),
            };
            let ext = ptask_core::Extensions {
                labels: q.labels.clone(),
                project: q.project.clone(),
                duration_min: q.duration_min,
                planned_at: None,
                energy: None,
                recurrence: q.recurrence.clone(),
            };
            let t = tasks::create_with_extensions_with_event(&state.db, new, ext, Some(&cmd.uuid))?;
            let payload = serde_json::to_value(&t)?;
            Ok((
                Some(t.id.clone()),
                EventPayload {
                    event_type: "task.created".into(),
                    payload,
                },
            ))
        }
        "task_done" => {
            let task = resolve_task(state, &cmd.args)?;
            let outcome = tasks::mark_done_with_event(&state.db, &task, Some(&cmd.uuid))?;
            let (event_type, payload) = match outcome {
                DoneOutcome::Completed => (
                    "task.completed".to_string(),
                    serde_json::json!({"task_uuid": task.id, "pt_id": task.pt_id}),
                ),
                DoneOutcome::Advanced { next_deadline } => (
                    "task.recurrence_advanced".to_string(),
                    serde_json::json!({
                        "task_uuid": task.id,
                        "pt_id": task.pt_id,
                        "next_deadline": next_deadline
                    }),
                ),
            };
            Ok((
                Some(task.id),
                EventPayload {
                    event_type,
                    payload,
                },
            ))
        }
        other => Err(anyhow::anyhow!("unsupported command type: {:?}", other)),
    }
}

/// Resolve a command's args to a Task. Accepts `{task_uuid}` or `{pt_id}`.
fn resolve_task(state: &AppState, args: &Value) -> Result<tasks::Task, anyhow::Error> {
    if let Some(s) = args.get("task_uuid").and_then(Value::as_str) {
        return task_by_uuid(&state.db, s);
    }
    if let Some(s) = args.get("pt_id").and_then(Value::as_str) {
        let t = tasks::resolve(&state.db, s)?;
        return Ok(t);
    }
    Err(anyhow::anyhow!("expected args.task_uuid or args.pt_id"))
}

/// Direct fetch by UUID (no PT-N indirection).
fn task_by_uuid(db: &ptask_core::Db, uuid: &str) -> Result<tasks::Task, anyhow::Error> {
    let conn = db.get()?;
    let row = conn.query_row(
        "SELECT t.id, x.pt_id, t.title, t.description, t.priority, t.status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning
         FROM tasks t LEFT JOIN pt_extensions x ON x.task_uuid = t.id
         WHERE t.id = ?1",
        [uuid],
        |r| {
            Ok(tasks::Task {
                id: r.get(0)?,
                pt_id: r.get(1)?,
                title: r.get(2)?,
                description: r.get(3).unwrap_or_default(),
                priority: r.get(4)?,
                status: r.get(5)?,
                created_at: r.get(6)?,
                updated_at: r.get(7)?,
                deadline: r.get(8)?,
                source_type: r.get(9)?,
                ai_reasoning: r.get(10).unwrap_or_default(),
            })
        },
    )?;
    Ok(row)
}
