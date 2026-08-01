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

/// What one command did, decided entirely inside the blocking pool so the
/// async handler only has to fold the result and fan out the webhook.
enum CommandOutcome {
    /// The idempotency lookup itself failed — surface, never fall through.
    LookupFailed(String),
    /// Already executed; carries the replayed temp_id → task_uuid mapping.
    Replayed(Option<(String, String)>),
    Applied {
        task_uuid: Option<String>,
        temp: Option<(String, String)>,
        payload: EventPayload,
    },
    Failed(String),
}

/// Idempotency lookup + mutation for one command. Pure blocking SQLite.
fn apply_one(state: &AppState, cmd: &Command, actor: &str) -> CommandOutcome {
    // A failed idempotency lookup must NOT fall through to apply: if the
    // command was already executed, re-applying double-creates. Surface
    // the error and let the client retry the whole command instead.
    let prior = match event_log::get_by_uuid(&state.db, &cmd.uuid) {
        Ok(p) => p,
        Err(e) => {
            warn!(target: "ptask::sync", error = %e, uuid = %cmd.uuid, "idempotency lookup failed");
            return CommandOutcome::LookupFailed(format!("idempotency lookup failed: {e}"));
        }
    };
    if let Some(event) = prior {
        return CommandOutcome::Replayed(replay_temp_mapping(cmd, &event));
    }
    // The mutation itself records the event row in its own transaction
    // (atomic, keyed on cmd.uuid) — no post-hoc event_log::record here.
    match apply_command(state, cmd, actor) {
        Ok((task_uuid, payload)) => {
            let temp = match (cmd.temp_id.as_ref(), task_uuid.as_ref()) {
                (Some(temp), Some(tu)) => Some((temp.clone(), tu.clone())),
                _ => None,
            };
            CommandOutcome::Applied {
                task_uuid,
                temp,
                payload,
            }
        }
        Err(e) => CommandOutcome::Failed(format!("{}", e)),
    }
}

/// `(cursor, delta tasks, tombstones)`, or the stage that failed and why.
type DeltaRead =
    std::result::Result<(i64, Vec<tasks::Task>, Vec<String>), (&'static str, ptask_core::Error)>;

/// The whole read half of a /sync response. Pure blocking SQLite.
fn read_delta(state: &AppState, full_sync: bool, since: i64) -> DeltaRead {
    // Snapshot the cursor BEFORE reading the delta. Events committed by a
    // concurrent writer between these two reads are then re-delivered on the
    // next sync (at-least-once) instead of being skipped forever, which is
    // what the previous read-delta-then-cursor order did (the token advanced
    // past events this response never contained).
    // A DB error here must be a loud 500, not an empty task universe: a
    // client full-syncing against a briefly-erroring store would otherwise
    // read "no tasks" as truth and clear its local state.
    let new_cursor = event_log::current_cursor(&state.db).map_err(|e| ("cursor read", e))?;
    if full_sync {
        let all = tasks::list_all(&state.db).map_err(|e| ("full-sync list", e))?;
        return Ok((new_cursor, all, Vec::new()));
    }
    let delta_uuids =
        event_log::changed_task_uuids_since(&state.db, since).map_err(|e| ("delta read", e))?;
    let deleted =
        event_log::deleted_task_uuids_since(&state.db, since).map_err(|e| ("tombstone read", e))?;
    let mut rows = Vec::new();
    for u in &delta_uuids {
        // Deleted tasks legitimately fail the row fetch — they're
        // reported through the tombstone list instead.
        if let Ok(t) = task_by_uuid(&state.db, u) {
            rows.push(t);
        }
    }
    Ok((new_cursor, rows, deleted))
}

/// The auth token lookup is itself a SQLite read, so it belongs on the
/// blocking pool with the rest of the handler's database work.
#[allow(clippy::result_large_err)] // the Err IS the ready-made 401 Response
fn authenticate_writer(
    state: &AppState,
    headers: &HeaderMap,
) -> std::result::Result<ptask_core::tokens::Identity, axum::response::Response> {
    crate::auth::authenticate(
        &state.db,
        &state.auth,
        headers,
        ptask_core::tokens::Scope::Write,
    )
}

fn sync_task_aborted(stage: &str) -> axum::response::Response {
    warn!(target: "ptask::sync", stage, "sync blocking task aborted");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"error": format!("{stage} failed")})),
    )
        .into_response()
}

/// Every SQLite leg of /sync runs on tokio's blocking pool. This handler is
/// the fleet's heaviest writer — each command takes the single write lock and
/// then rescores — so leaving it on an async worker let a burst of >8 clients
/// park every worker for up to the 30s pool/busy timeout.
#[allow(clippy::result_large_err)] // the auth Err IS the ready-made 401 Response
async fn sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SyncReq>,
) -> impl IntoResponse {
    let auth_state = state.clone();
    let identity =
        match crate::blocking::db_value(move || authenticate_writer(&auth_state, &headers)).await {
            Ok(Ok(id)) => id,
            Ok(Err(resp)) => return resp,
            Err(_) => return sync_task_aborted("authentication"),
        };
    let mut status: BTreeMap<String, Value> = BTreeMap::new();
    let mut temp_map: BTreeMap<String, String> = BTreeMap::new();

    // Apply commands sequentially. Each command's `uuid` is its idempotency
    // key — replays return "ok" without re-executing.
    for cmd in req.commands {
        let cmd_uuid = cmd.uuid.clone();
        let cmd_state = state.clone();
        let actor = identity.client_id.clone();
        let outcome = match crate::blocking::db_value(move || apply_one(&cmd_state, &cmd, &actor))
            .await
        {
            Ok(o) => o,
            Err(e) => {
                warn!(target: "ptask::sync", error = %e, uuid = %cmd_uuid, "sync command task aborted");
                status.insert(
                    cmd_uuid,
                    serde_json::json!({ "error": "command task aborted" }),
                );
                continue;
            }
        };
        match outcome {
            CommandOutcome::LookupFailed(e) | CommandOutcome::Failed(e) => {
                status.insert(cmd_uuid, serde_json::json!({ "error": e }));
            }
            CommandOutcome::Replayed(temp) => {
                if let Some((temp_id, task_uuid)) = temp {
                    temp_map.insert(temp_id, task_uuid);
                }
                status.insert(cmd_uuid, Value::String("ok".into()));
            }
            CommandOutcome::Applied {
                task_uuid,
                temp,
                payload,
            } => {
                if let Some((temp_id, tu)) = temp {
                    temp_map.insert(temp_id, tu);
                }
                // Outbound webhook fan-out (env-driven; no-op if unconfigured).
                crate::webhooks::dispatch(
                    &state,
                    &payload.event_type,
                    task_uuid.as_deref(),
                    &payload.payload,
                )
                .await;
                status.insert(cmd_uuid, Value::String("ok".into()));
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
    let read_state = state.clone();
    let (new_cursor, delta_tasks, deleted_task_uuids) =
        match crate::blocking::db_value(move || read_delta(&read_state, full_sync, since)).await {
            Ok(Ok(v)) => v,
            Ok(Err((stage, e))) => return sync_read_error(stage, e),
            Err(_) => return sync_task_aborted("delta read"),
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

fn replay_temp_mapping(cmd: &Command, event: &event_log::LoggedEvent) -> Option<(String, String)> {
    if cmd.kind == "task_create"
        && let (Some(temp_id), Some(task_uuid)) = (cmd.temp_id.as_ref(), event.task_uuid.as_ref())
    {
        return Some((temp_id.clone(), task_uuid.clone()));
    }
    None
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
                due_at: q.due.clone(),
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
        "task_start" => {
            let task = resolve_task(state, &cmd.args)?;
            tasks::start(&state.db, &task.id, &sync_ctx(actor, &cmd.uuid))?;
            Ok((
                Some(task.id.clone()),
                EventPayload {
                    event_type: "task.updated".into(),
                    payload: serde_json::json!({ "task_uuid": task.id, "status": "in_progress" }),
                },
            ))
        }
        "task_snooze" => {
            let task = resolve_task(state, &cmd.args)?;
            let until = cmd
                .args
                .get("until")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ptask_core::Error::Other("task_snooze needs args.until".into()))?;
            tasks::snooze(&state.db, &task.id, until, &sync_ctx(actor, &cmd.uuid))?;
            Ok((
                Some(task.id.clone()),
                EventPayload {
                    event_type: "task.updated".into(),
                    payload: serde_json::json!({
                        "task_uuid": task.id, "status": "snoozed", "snoozed_until": until
                    }),
                },
            ))
        }
        "task_depend" => {
            let task = resolve_task(state, &cmd.args)?;
            let on = cmd
                .args
                .get("on")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ptask_core::Error::Other("task_depend needs args.on".into()))?;
            let on_task = resolve_query(state, on)?;
            let clear = cmd
                .args
                .get("clear")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if clear {
                tasks::remove_dependency(
                    &state.db,
                    &task.id,
                    &on_task.id,
                    &sync_ctx(actor, &cmd.uuid),
                )?;
            } else {
                tasks::add_dependency(
                    &state.db,
                    &task.id,
                    &on_task.id,
                    &sync_ctx(actor, &cmd.uuid),
                )?;
            }
            let key = if clear {
                "depends_on_removed"
            } else {
                "depends_on_added"
            };
            let mut payload = serde_json::json!({ "task_uuid": task.id });
            payload[key] = serde_json::json!(on_task.id);
            Ok((
                Some(task.id.clone()),
                EventPayload {
                    event_type: "task.updated".into(),
                    payload,
                },
            ))
        }
        "task_delete" => {
            let task = resolve_task(state, &cmd.args)?;
            tasks::delete_task(&state.db, &task.id, &sync_ctx(actor, &cmd.uuid))?;
            Ok((
                Some(task.id.clone()),
                EventPayload {
                    event_type: "task.deleted".into(),
                    payload: serde_json::json!({ "task_uuid": task.id, "pt_id": task.pt_id }),
                },
            ))
        }
        other => Err(anyhow::anyhow!("unsupported command type: {:?}", other)),
    }
}

/// Resolve a command's args to a Task. Accepts `{task_uuid}` or `{pt_id}`.
/// Resolve a bare query string (PT-N / integer / title substring),
/// including terminal tasks — dependency targets are often already done.
fn resolve_query(state: &AppState, query: &str) -> Result<tasks::Task, anyhow::Error> {
    tasks::resolve_for_lookup(&state.db, query, true).map_err(|e| anyhow::anyhow!("{e}"))
}

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
        "SELECT t.id, t.pt_id, t.title, t.description, t.priority, t.status_v2 AS status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning
         FROM tasks t
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use std::time::{Duration, Instant};

    /// Regression (#39.2): /sync is the heaviest writer on the fleet and ran
    /// every SQLite leg — auth lookup, per-command mutation + rescore, delta
    /// read — inline on an async worker. A caller that had to wait for a
    /// pooled connection parked that worker for the whole wait.
    #[tokio::test]
    async fn sync_does_not_park_the_async_executor() {
        const HOLD: Duration = Duration::from_millis(600);
        // Db::open's pool is max_size(8).
        const POOL_SIZE: usize = 8;

        let dir = tempfile::tempdir().unwrap();
        let db = ptask_core::Db::open(dir.path().join("t.db")).unwrap();
        let state = AppState::new(
            db.clone(),
            ptask_core::config::AuthConfig::default(),
            ptask_core::config::WebhookConfig::default(),
        );

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let hold_db = db.clone();
        let holder = std::thread::spawn(move || {
            let held: Vec<_> = (0..POOL_SIZE).map(|_| hold_db.get().unwrap()).collect();
            ready_tx.send(()).unwrap();
            std::thread::sleep(HOLD);
            drop(held);
        });
        ready_rx.recv().unwrap();

        let started = Instant::now();
        let handler = sync(
            State(state),
            HeaderMap::new(),
            Json(SyncReq {
                sync_token: None,
                resource_types: Vec::new(),
                commands: Vec::new(),
            }),
        );
        let timer = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            started.elapsed()
        };
        let (_resp, timer_elapsed) = tokio::join!(handler, timer);
        holder.join().unwrap();

        assert!(
            timer_elapsed < HOLD / 2,
            "unrelated runtime work was starved for {timer_elapsed:?} while /sync \
             waited for a connection"
        );
    }
}
