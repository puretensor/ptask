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
//!       // v1.8.0 — resolve by { task_uuid } or { pt_id }:
//!       { "type": "task_priority", "uuid": "...", "args": { "task_uuid": "...", "priority": 4 } },
//!       { "type": "task_edit",     "uuid": "...", "args": { "task_uuid": "...", "deadline": "2026-07-01" | null } },
//!       { "type": "task_reopen",   "uuid": "...", "args": { "task_uuid": "..." } },
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
use ptask_core::event_log::EventCtx;
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

fn sync_read_error(stage: &str, e: ptask_core::Error) -> axum::response::Response {
    warn!(target: "ptask::sync", error = %e, stage, "sync read failed");
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(serde_json::json!({"error": format!("{stage} failed")})),
    )
        .into_response()
}

/// Attribution for one /sync command: the authenticated client identity +
/// the command uuid as the idempotency key.
fn sync_ctx(actor: &str, cmd_uuid: &str) -> EventCtx {
    EventCtx::sync(actor, cmd_uuid)
}

async fn sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SyncReq>,
) -> impl IntoResponse {
    let identity = match crate::auth::authenticate(
        &state.db,
        &state.auth,
        &headers,
        ptask_core::tokens::Scope::Write,
    ) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
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
        match apply_command(&state, cmd, &identity.client_id) {
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
    // A DB error here must be a loud 500, not an empty task universe: a
    // client full-syncing against a briefly-erroring store would otherwise
    // read "no tasks" as truth and clear its local state.
    let new_cursor = match event_log::current_cursor(&state.db) {
        Ok(c) => c,
        Err(e) => return sync_read_error("cursor read", e),
    };
    let (delta_tasks, deleted_task_uuids): (Vec<tasks::Task>, Vec<String>) = if full_sync {
        match tasks::list_all(&state.db) {
            Ok(all) => (all, Vec::new()),
            Err(e) => return sync_read_error("full-sync list", e),
        }
    } else {
        let delta_uuids = match event_log::changed_task_uuids_since(&state.db, since) {
            Ok(v) => v,
            Err(e) => return sync_read_error("delta read", e),
        };
        let deleted = match event_log::deleted_task_uuids_since(&state.db, since) {
            Ok(v) => v,
            Err(e) => return sync_read_error("tombstone read", e),
        };
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
    actor: &str,
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
            let t =
                tasks::create_with_extensions(&state.db, new, ext, &sync_ctx(actor, &cmd.uuid))?;
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
            let outcome = tasks::mark_done(&state.db, &task, &sync_ctx(actor, &cmd.uuid))?;
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
        "task_priority" => {
            let task = resolve_task(state, &cmd.args)?;
            let priority = cmd
                .args
                .get("priority")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow::anyhow!("task_priority: args.priority required"))?;
            tasks::update_priority(&state.db, &task.id, priority, &sync_ctx(actor, &cmd.uuid))?;
            // Priority feeds the composite priority_score; rescore best-effort so
            // ordering / the dashboard reflect the change without waiting for the
            // scoring timer (parity with the local `pt priority`).
            if let Err(e) = ptask_core::scoring::run_once(&state.db, false) {
                warn!(target: "ptask::sync", error = %e, "post-mutation rescore failed");
            }
            Ok((
                Some(task.id.clone()),
                EventPayload {
                    event_type: "task.updated".into(),
                    payload: serde_json::json!({ "task_uuid": task.id, "priority": priority }),
                },
            ))
        }
        "task_edit" => {
            let task = resolve_task(state, &cmd.args)?;
            // `deadline` present as a string sets it; present as null clears it;
            // absent is an error (this command edits the deadline).
            if cmd.args.get("deadline").is_none() {
                return Err(anyhow::anyhow!(
                    "task_edit: args.deadline required (ISO string to set, null to clear)"
                ));
            }
            let new_deadline = cmd.args.get("deadline").and_then(Value::as_str);
            tasks::update_deadline(
                &state.db,
                &task.id,
                new_deadline,
                &sync_ctx(actor, &cmd.uuid),
            )?;
            if let Err(e) = ptask_core::scoring::run_once(&state.db, false) {
                warn!(target: "ptask::sync", error = %e, "post-mutation rescore failed");
            }
            Ok((
                Some(task.id.clone()),
                EventPayload {
                    event_type: "task.updated".into(),
                    payload: serde_json::json!({ "task_uuid": task.id, "deadline": new_deadline }),
                },
            ))
        }
        "task_reopen" => {
            let task = resolve_task(state, &cmd.args)?;
            tasks::reopen(&state.db, &task.id, &sync_ctx(actor, &cmd.uuid))?;
            if let Err(e) = ptask_core::scoring::run_once(&state.db, false) {
                warn!(target: "ptask::sync", error = %e, "post-mutation rescore failed");
            }
            Ok((
                Some(task.id.clone()),
                EventPayload {
                    event_type: "task.updated".into(),
                    payload: serde_json::json!({ "task_uuid": task.id, "status": "pending" }),
                },
            ))
        }
        "task_retext" => {
            let task = resolve_task(state, &cmd.args)?;
            let title = cmd.args.get("title").and_then(Value::as_str);
            let description = cmd.args.get("description").and_then(Value::as_str);
            if title.is_none() && description.is_none() {
                return Err(anyhow::anyhow!(
                    "task_retext: at least one of args.title / args.description required"
                ));
            }
            tasks::update_text(
                &state.db,
                &task.id,
                title,
                description,
                &sync_ctx(actor, &cmd.uuid),
            )?;
            Ok((
                Some(task.id.clone()),
                EventPayload {
                    event_type: "task.updated".into(),
                    payload: serde_json::json!({
                        "task_uuid": task.id, "title": title, "description": description
                    }),
                },
            ))
        }
        "task_dismiss" => {
            let task = resolve_task(state, &cmd.args)?;
            tasks::dismiss(&state.db, &task.id, &sync_ctx(actor, &cmd.uuid))?;
            Ok((
                Some(task.id.clone()),
                EventPayload {
                    event_type: "task.updated".into(),
                    payload: serde_json::json!({ "task_uuid": task.id, "status": "dismissed" }),
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
