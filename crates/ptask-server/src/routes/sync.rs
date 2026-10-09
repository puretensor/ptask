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

/// Attribution for one /sync command: the authenticated client identity,
/// the command uuid scoped to that client as the idempotency key, and the
/// command's fingerprint (kind + canonical args + temp_id).
fn sync_ctx(actor: &str, cmd: &Command) -> EventCtx {
    EventCtx::sync(actor, sync_event_uuid(actor, &cmd.uuid)).with_command(command_fingerprint(cmd))
}

/// What makes two commands under one uuid the same command. serde_json's
/// map is ordered by key, so the rendering is canonical.
fn command_fingerprint(cmd: &Command) -> event_log::CommandFingerprint {
    let canonical = serde_json::json!({ "args": cmd.args, "temp_id": cmd.temp_id });
    event_log::CommandFingerprint::new(&cmd.kind, &canonical.to_string())
}

/// The journal key for a client's command uuid. Client uuids shared one
/// namespace with every other client and with server-derived keys
/// (`capture:<id>`, `tg-cb:<id>`, `git:…:close`), so two clients that
/// picked the same uuid had the second command swallowed as a "replay".
/// The actor is length-prefixed: client ids may contain ':', and
/// `sync:{actor}:{uuid}` let "hal:x" + "c1" collide with "hal" + "x:c1".
fn sync_event_uuid(actor: &str, cmd_uuid: &str) -> String {
    format!("sync:{}:{actor}:{cmd_uuid}", actor.len())
}

/// Keys journaled before the current format, newest first: the v3.36.0
/// `sync:{actor}:{uuid}` and the raw client uuid.
fn legacy_event_uuids(actor: &str, cmd_uuid: &str) -> [String; 2] {
    [format!("sync:{actor}:{cmd_uuid}"), cmd_uuid.to_string()]
}

/// The journal event types each command kind writes; a prior event under the
/// command's key is a replay only if it is one of these.
fn command_event_types(kind: &str) -> &'static [&'static str] {
    match kind {
        "task_create" => &["task.created"],
        "task_done" => &["task.completed", "task.recurrence_advanced"],
        "task_priority" | "task_edit" | "task_reopen" | "task_retext" | "task_dismiss"
        | "task_start" | "task_snooze" | "task_depend" => &["task.updated"],
        "task_delete" => &["task.deleted"],
        "task_note" => &["task.noted"],
        _ => &[],
    }
}

/// What the journal already holds for this command's key.
enum Prior {
    None,
    /// Already executed: the event it recorded.
    Replay(event_log::LoggedEvent),
    /// The key was used for a different command or task.
    Conflict(String),
}

fn prior_command(state: &AppState, cmd: &Command, actor: &str) -> ptask_core::Result<Prior> {
    // Only this client's own events count. Older key formats were
    // ambiguous (or unscoped), so an event under one of them that another
    // client journaled is simply not this command's.
    let mut event = None;
    let keys = std::iter::once(sync_event_uuid(actor, &cmd.uuid))
        .chain(legacy_event_uuids(actor, &cmd.uuid));
    for key in keys {
        if let Some(found) = event_log::get_by_uuid(&state.db, &key)?
            && found.actor.as_deref() == Some(actor)
        {
            event = Some(found);
            break;
        }
    }
    let Some(event) = event else {
        return Ok(Prior::None);
    };
    // The task the command names, when it still resolves (a replayed
    // task_delete names a task that is gone: the event type decides).
    let target = match (
        cmd.args.get("task_uuid").and_then(Value::as_str),
        cmd.args.get("pt_id").and_then(Value::as_str),
    ) {
        (Some(uuid), _) => Some(uuid.to_string()),
        (None, Some(pt)) => tasks::resolve(&state.db, pt).ok().map(|t| t.id),
        (None, None) => None,
    };
    Ok(
        match event_log::verify_replay(
            &cmd.uuid,
            &event,
            &event_log::ReplayCheck {
                actor,
                task_uuid: target.as_deref(),
                event_types: command_event_types(&cmd.kind),
                command: Some(&command_fingerprint(cmd)),
            },
        ) {
            Ok(()) => Prior::Replay(event),
            Err(e) => Prior::Conflict(e.to_string()),
        },
    )
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
    // The capture lane's namespace: its keys mark capture-created tasks,
    // and the legacy raw-uuid replay lookup below would read them too.
    if event_log::is_reserved_client_key(&cmd.uuid) {
        return CommandOutcome::Failed(
            "command uuid: the capture prefix is reserved for the capture lane".into(),
        );
    }
    // A failed idempotency lookup must NOT fall through to apply: if the
    // command was already executed, re-applying double-creates. Surface
    // the error and let the client retry the whole command instead.
    match prior_command(state, cmd, actor) {
        Ok(Prior::None) => {}
        Ok(Prior::Replay(event)) => {
            return CommandOutcome::Replayed(replay_temp_mapping(cmd, &event));
        }
        Ok(Prior::Conflict(e)) => return CommandOutcome::Failed(e),
        Err(e) => {
            warn!(target: "ptask::sync", error = %e, uuid = %cmd.uuid, "idempotency lookup failed");
            return CommandOutcome::LookupFailed(format!("idempotency lookup failed: {e}"));
        }
    }
    // The mutation itself records the event row in its own transaction
    // (atomic, keyed on the scoped command uuid) — no post-hoc record here.
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
        // A concurrent request with the same command uuid can pass the
        // lookup too and commit first; this attempt then rolled back on
        // UNIQUE(pt_event_log.uuid). It was applied: answer as the replay.
        Err(e) => match prior_command(state, cmd, actor) {
            Ok(Prior::Replay(event)) => CommandOutcome::Replayed(replay_temp_mapping(cmd, &event)),
            _ => CommandOutcome::Failed(format!("{}", e)),
        },
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
        // Missing rows are delivered as tombstones. Any other read failure
        // must abort the response so its cursor cannot hide an unread update.
        match task_by_uuid(&state.db, u) {
            Ok(t) => rows.push(t),
            Err(ptask_core::Error::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => {}
            Err(e) => return Err(("delta task read", e)),
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

/// Upper bound on commands per /sync request (413 beyond).
pub const MAX_SYNC_COMMANDS: usize = 200;

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
    // Each command takes the single SQLite write lock in turn; axum's 2 MB body
    // limit alone permits thousands per request.
    if req.commands.len() > MAX_SYNC_COMMANDS {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": format!("too many commands: {} > {}", req.commands.len(), MAX_SYNC_COMMANDS)
            })),
        )
            .into_response();
    }
    let mut status: BTreeMap<String, Value> = BTreeMap::new();
    let mut temp_map: BTreeMap<String, String> = BTreeMap::new();
    // Priority, deadline and reopen feed the composite score. Rescore once
    // after the batch (parity with local `pt priority`/`edit`/`reopen`), not
    // once per command: each pass rewrites every active row under the write
    // lock, and a 200-command batch paid for 200 of them.
    let mut needs_rescore = false;
    // Outbound webhook fan-out (env-driven; no-op if unconfigured).
    let outbox = crate::webhooks::Outbox::start(&state);

    // Apply commands sequentially. Each command's `uuid` is its idempotency
    // key — replays return "ok" without re-executing.
    for cmd in req.commands {
        let cmd_uuid = cmd.uuid.clone();
        let rescores = matches!(
            cmd.kind.as_str(),
            "task_priority" | "task_edit" | "task_reopen"
        );
        let cmd_state = state.clone();
        let actor = identity.client_id.clone();
        let cmd_outbox = outbox.clone();
        // Enqueue the webhook inside the commit-order lock, so concurrent
        // requests' events reach subscribers in the order they committed.
        let outcome = match crate::blocking::db_value(move || {
            crate::webhooks::commit_ordered(&cmd_outbox, || {
                let outcome = apply_one(&cmd_state, &cmd, &actor);
                if let CommandOutcome::Applied {
                    task_uuid, payload, ..
                } = &outcome
                {
                    cmd_outbox.send(crate::webhooks::OutboundEvent {
                        event_type: payload.event_type.clone(),
                        task_uuid: task_uuid.clone(),
                        payload: payload.payload.clone(),
                        // The journal key, so the envelope carries the
                        // committed row's ts and event_id.
                        event_uuid: Some(sync_event_uuid(&actor, &cmd.uuid)),
                    });
                }
                outcome
            })
        })
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
            CommandOutcome::Applied { temp, .. } => {
                if let Some((temp_id, tu)) = temp {
                    temp_map.insert(temp_id, tu);
                }
                needs_rescore |= rescores;
                status.insert(cmd_uuid, Value::String("ok".into()));
            }
        }
    }

    if needs_rescore {
        let db = state.db.clone();
        match crate::blocking::db_value(move || ptask_core::scoring::run_once(&db, false)).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => warn!(target: "ptask::sync", error = %e, "post-mutation rescore failed"),
            Err(e) => warn!(target: "ptask::sync", error = %e, "post-mutation rescore aborted"),
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
            let source_type = cmd
                .args
                .get("source_type")
                .and_then(Value::as_str)
                .unwrap_or("sync");
            let (new, ext) = ptask_core::quickadd::parse(text)?.task_parts(source_type);
            let t = tasks::create_with_extensions(&state.db, new, ext, &sync_ctx(actor, cmd))?;
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
            let expected = match cmd.args.get("expected_deadline") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.as_str()),
                Some(_) => {
                    return Err(anyhow::anyhow!(
                        "task_done: args.expected_deadline must be a string"
                    ));
                }
            };
            let note = note_arg(&cmd.args, "task_done")?;
            let task = tasks::expect_deadline(resolve_task(state, &cmd.args)?, expected)?;
            let outcome = tasks::mark_done_noted(&state.db, &task, note, &sync_ctx(actor, cmd))?;
            let (event_type, mut payload) = match outcome {
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
            if let Some(n) = note {
                payload["note"] = serde_json::json!(n.trim());
            }
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
            tasks::update_priority(&state.db, &task.id, priority, &sync_ctx(actor, cmd))?;
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
            let new_deadline = match cmd.args.get("deadline") {
                Some(Value::String(deadline)) => Some(deadline.as_str()),
                Some(Value::Null) => None,
                _ => {
                    return Err(anyhow::anyhow!(
                        "task_edit: args.deadline required (ISO string to set, null to clear)"
                    ));
                }
            };
            tasks::update_deadline(&state.db, &task.id, new_deadline, &sync_ctx(actor, cmd))?;
            // Echo what was stored (normalised), not the raw input.
            let stored = tasks::resolve_for_lookup(&state.db, &task.id, true)?.deadline;
            Ok((
                Some(task.id.clone()),
                EventPayload {
                    event_type: "task.updated".into(),
                    payload: serde_json::json!({ "task_uuid": task.id, "deadline": stored }),
                },
            ))
        }
        "task_reopen" => {
            let task = resolve_task(state, &cmd.args)?;
            tasks::reopen(&state.db, &task.id, &sync_ctx(actor, cmd))?;
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
                &sync_ctx(actor, cmd),
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
            let note = note_arg(&cmd.args, "task_dismiss")?;
            let task = resolve_task(state, &cmd.args)?;
            tasks::dismiss_noted(&state.db, &task.id, note, &sync_ctx(actor, cmd))?;
            let mut payload = serde_json::json!({ "task_uuid": task.id, "status": "dismissed" });
            if let Some(n) = note {
                payload["note"] = serde_json::json!(n.trim());
            }
            Ok((
                Some(task.id.clone()),
                EventPayload {
                    event_type: "task.updated".into(),
                    payload,
                },
            ))
        }
        "task_note" => {
            let text = match cmd.args.get("text") {
                Some(Value::String(s)) => s.as_str(),
                _ => return Err(anyhow::anyhow!("task_note: args.text must be a string")),
            };
            let task = resolve_task(state, &cmd.args)?;
            let note = ptask_core::notes::add(&state.db, &task.id, text, &sync_ctx(actor, cmd))?;
            Ok((
                Some(task.id.clone()),
                EventPayload {
                    event_type: "task.noted".into(),
                    payload: serde_json::json!({
                        "task_uuid": task.id, "pt_id": task.pt_id, "note": note.text
                    }),
                },
            ))
        }
        "task_start" => {
            let task = resolve_task(state, &cmd.args)?;
            let _ = tasks::start(&state.db, &task.id, &sync_ctx(actor, cmd))?;
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
            tasks::snooze(&state.db, &task.id, until, &sync_ctx(actor, cmd))?;
            // Echo what was stored (normalised), not the raw input.
            let until: Option<String> = state.db.with_conn(|c| {
                Ok(c.query_row(
                    "SELECT snoozed_until FROM tasks WHERE id=?1",
                    [&task.id],
                    |r| r.get(0),
                )?)
            })?;
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
                tasks::remove_dependency(&state.db, &task.id, &on_task.id, &sync_ctx(actor, cmd))?;
            } else {
                tasks::add_dependency(&state.db, &task.id, &on_task.id, &sync_ctx(actor, cmd))?;
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
            tasks::delete_task(&state.db, &task.id, &sync_ctx(actor, cmd))?;
            Ok((
                Some(task.id.clone()),
                EventPayload {
                    event_type: "task.deleted".into(),
                    payload: serde_json::json!({
                        "task_uuid": task.id,
                        "pt_id": task.pt_id,
                        "status": task.status,
                    }),
                },
            ))
        }
        other => Err(anyhow::anyhow!("unsupported command type: {:?}", other)),
    }
}

/// The optional `note` arg of a closing command: absent or null is no note,
/// a string is the evidence (validated by the core), anything else an error.
fn note_arg<'a>(args: &'a Value, kind: &str) -> Result<Option<&'a str>, anyhow::Error> {
    match args.get("note") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(anyhow::anyhow!("{kind}: args.note must be a string")),
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
        return Ok(task_by_uuid(&state.db, s)?);
    }
    if let Some(s) = args.get("pt_id").and_then(Value::as_str) {
        let t = tasks::resolve(&state.db, s)?;
        return Ok(t);
    }
    Err(anyhow::anyhow!("expected args.task_uuid or args.pt_id"))
}

/// Direct fetch by UUID (no PT-N indirection).
fn task_by_uuid(db: &ptask_core::Db, uuid: &str) -> ptask_core::Result<tasks::Task> {
    let conn = db.get()?;
    let row = conn.query_row(
        "SELECT t.id, t.pt_id, t.title, t.description, t.priority, t.status_v2 AS status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning,
                t.kind, t.deliverable
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
                kind: r.get(11).unwrap_or_else(|_| "ship".to_string()),
                deliverable: r.get(12).unwrap_or_default(),
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

    fn test_state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let db = ptask_core::Db::open(dir.path().join("sync.db")).unwrap();
        (
            dir,
            AppState::new(db, Default::default(), Default::default()),
        )
    }

    #[tokio::test]
    async fn sync_rejects_non_string_deadlines_without_mutation() {
        let (_dir, state) = test_state();
        let mut new = tasks::NewTask::minimal("retain my deadline");
        new.deadline = Some("2099-01-01".into());
        let task = tasks::create(&state.db, new, &EventCtx::test()).unwrap();
        let cursor = event_log::current_cursor(&state.db).unwrap();
        for value in [
            serde_json::json!(false),
            serde_json::json!(7),
            serde_json::json!(1.5),
            serde_json::json!([]),
            serde_json::json!({}),
        ] {
            let response = sync(
                State(state.clone()),
                HeaderMap::new(),
                Json(SyncReq {
                    sync_token: Some(cursor.to_string()),
                    resource_types: vec![],
                    commands: vec![Command {
                        kind: "task_edit".into(),
                        uuid: "invalid-deadline".into(),
                        temp_id: None,
                        args: serde_json::json!({"task_uuid": task.id, "deadline": value}),
                    }],
                }),
            )
            .await
            .into_response();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert!(
                body["sync_status"]["invalid-deadline"]["error"].is_string(),
                "{body}"
            );
            assert_eq!(
                task_by_uuid(&state.db, &task.id).unwrap().deadline,
                task.deadline
            );
            assert_eq!(event_log::current_cursor(&state.db).unwrap(), cursor);
        }
        // Rejected command IDs remain retryable with a valid value.
        for deadline in [serde_json::json!("2099-02-01"), Value::Null] {
            let cmd = Command {
                kind: "task_edit".into(),
                uuid: if deadline.is_null() {
                    "clear-deadline"
                } else {
                    "invalid-deadline"
                }
                .into(),
                temp_id: None,
                args: serde_json::json!({"task_uuid": task.id, "deadline": deadline}),
            };
            apply_command(&state, &cmd, "test").unwrap();
            assert_eq!(
                task_by_uuid(&state.db, &task.id)
                    .unwrap()
                    .deadline
                    .as_deref(),
                deadline.as_str()
            );
        }
        let cmd = Command {
            kind: "task_edit".into(),
            uuid: "missing-deadline".into(),
            temp_id: None,
            args: serde_json::json!({"task_uuid": task.id}),
        };
        assert!(apply_command(&state, &cmd, "test").is_err());
    }

    fn create_cmd(uuid: &str, temp_id: &str, text: &str) -> Command {
        Command {
            kind: "task_create".into(),
            uuid: uuid.into(),
            temp_id: Some(temp_id.into()),
            args: serde_json::json!({ "text": text }),
        }
    }

    fn outcome_ok(outcome: &CommandOutcome) -> Result<Option<(String, String)>, String> {
        match outcome {
            CommandOutcome::Applied { temp, .. } | CommandOutcome::Replayed(temp) => {
                Ok(temp.clone())
            }
            CommandOutcome::Failed(e) | CommandOutcome::LookupFailed(e) => Err(e.clone()),
        }
    }

    fn task_count(state: &AppState) -> i64 {
        state
            .db
            .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?))
            .unwrap()
    }

    #[test]
    fn task_done_with_expected_deadline_completes_one_occurrence_once() {
        // Regression (round 2, item 4ii): /sync resolved the task when each
        // command ran, so [task_done d1, task_done d2] for the same
        // occurrence advanced it twice. expected_deadline pins the
        // occurrence the client saw.
        let (_dir, state) = test_state();
        let mut new = tasks::NewTask::minimal("daily");
        new.deadline = Some("2099-01-01".into());
        let ext = ptask_core::Extensions {
            recurrence: Some(ptask_core::recurrence::parse("every day").unwrap()),
            ..Default::default()
        };
        let t = tasks::create_with_extensions(&state.db, new, ext, &EventCtx::test()).unwrap();
        let done = |uuid: &str| Command {
            kind: "task_done".into(),
            uuid: uuid.into(),
            temp_id: None,
            args: serde_json::json!({ "task_uuid": t.id, "expected_deadline": "2099-01-01" }),
        };
        assert!(outcome_ok(&apply_one(&state, &done("d1"), "hal")).is_ok());
        let second = outcome_ok(&apply_one(&state, &done("d2"), "hal"));
        assert!(second.is_err(), "second completion of the same occurrence");
        let after = task_by_uuid(&state.db, &t.id).unwrap();
        assert_eq!(after.deadline.as_deref(), Some("2099-01-02"));

        // Without it, behaviour is unchanged: the current occurrence.
        let plain = Command {
            kind: "task_done".into(),
            uuid: "d3".into(),
            temp_id: None,
            args: serde_json::json!({ "task_uuid": t.id }),
        };
        assert!(outcome_ok(&apply_one(&state, &plain, "hal")).is_ok());
        let after = task_by_uuid(&state.db, &t.id).unwrap();
        assert_eq!(after.deadline.as_deref(), Some("2099-01-03"));
    }

    #[test]
    fn a_command_uuid_reused_with_different_arguments_is_an_error() {
        // Regression (round 2, 9a): every task.updated kind looked alike, and
        // arguments were never compared.
        let (_dir, state) = test_state();
        let created = apply_one(&state, &create_cmd("c-1", "t-1", "write report"), "hal");
        let (_, task_uuid) = outcome_ok(&created).unwrap().unwrap();
        let on_task = |kind: &str| Command {
            kind: kind.into(),
            uuid: "c-2".into(),
            temp_id: None,
            args: serde_json::json!({ "task_uuid": task_uuid }),
        };
        assert!(outcome_ok(&apply_one(&state, &on_task("task_dismiss"), "hal")).is_ok());
        assert!(outcome_ok(&apply_one(&state, &on_task("task_reopen"), "hal")).is_err());
        assert_eq!(
            task_by_uuid(&state.db, &task_uuid).unwrap().status,
            "dismissed"
        );
        // The same command is still a replay.
        assert!(matches!(
            apply_one(&state, &on_task("task_dismiss"), "hal"),
            CommandOutcome::Replayed(_)
        ));

        // task_create: same uuid, different text or temp_id.
        for other in [
            create_cmd("c-1", "t-1", "something else"),
            create_cmd("c-1", "t-9", "write report"),
        ] {
            assert!(outcome_ok(&apply_one(&state, &other, "hal")).is_err());
        }
        assert_eq!(task_count(&state), 1);
    }

    #[test]
    fn a_client_id_containing_a_colon_cannot_alias_another_clients_uuid() {
        // Regression (round 2, 9b): the scoped key `sync:{actor}:{uuid}` was
        // ambiguous — token "hal:x" sending uuid c1 and token "hal" sending
        // uuid x:c1 shared one key, and hal got hal:x's task back.
        let (_dir, state) = test_state();
        let theirs = apply_one(&state, &create_cmd("c1", "t", "hal:x's task"), "hal:x");
        let (_, their_uuid) = outcome_ok(&theirs).unwrap().unwrap();
        let mine = apply_one(&state, &create_cmd("x:c1", "t", "hal's task"), "hal");
        assert!(matches!(mine, CommandOutcome::Applied { .. }), "aliased");
        let (_, my_uuid) = outcome_ok(&mine).unwrap().unwrap();
        assert_ne!(my_uuid, their_uuid);
        assert_eq!(task_count(&state), 2);
    }

    #[test]
    fn edit_and_snooze_payloads_echo_the_stored_value() {
        // Round 2 (cosmetic): the outbound payload echoed the raw input
        // ("+0100") rather than what was stored.
        let (_dir, state) = test_state();
        let created = apply_one(&state, &create_cmd("c-1", "t-1", "report"), "hal");
        let (_, task_uuid) = outcome_ok(&created).unwrap().unwrap();
        let raw = "2099-12-10T09:00:00+0100";
        for (kind, field, args) in [
            (
                "task_edit",
                "deadline",
                serde_json::json!({ "deadline": raw }),
            ),
            (
                "task_snooze",
                "snoozed_until",
                serde_json::json!({ "until": raw }),
            ),
        ] {
            let mut args = args;
            args["task_uuid"] = serde_json::json!(task_uuid);
            let cmd = Command {
                kind: kind.into(),
                uuid: format!("{kind}-1"),
                temp_id: None,
                args,
            };
            let CommandOutcome::Applied { payload, .. } = apply_one(&state, &cmd, "hal") else {
                panic!("{kind} not applied");
            };
            assert_eq!(
                payload.payload[field], "2099-12-10T08:00:00+00:00",
                "{kind}: {}",
                payload.payload
            );
        }
    }

    #[test]
    fn concurrent_requests_with_one_command_uuid_both_answer_ok() {
        // Regression (SRV-8): both requests passed the idempotency lookup,
        // the loser's insert hit UNIQUE(pt_event_log.uuid) and it answered
        // {"error": "sqlite: UNIQUE constraint failed"} with no temp_id
        // mapping, although the command had been applied.
        let (_dir, state) = test_state();
        // Hold the write lock so both requests read "not applied yet" and
        // queue on it, then release them together.
        let mut writer = rusqlite::Connection::open(state.db.path()).unwrap();
        let tx = writer
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        tx.execute(
            "UPDATE pt_counters SET value = value WHERE name='pt_id'",
            [],
        )
        .unwrap();
        let racers: Vec<_> = (0..2)
            .map(|_| {
                let state = state.clone();
                std::thread::spawn(move || {
                    apply_one(&state, &create_cmd("same-cmd", "tmp-1", "race me"), "hal")
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(300));
        tx.commit().unwrap();
        let mappings: Vec<_> = racers
            .into_iter()
            .map(|h| outcome_ok(&h.join().unwrap()).expect("both answer ok"))
            .collect();
        assert_eq!(task_count(&state), 1);
        assert!(mappings[0].is_some(), "temp_id mapping missing");
        assert_eq!(mappings[0], mappings[1], "both map tmp-1 to the one task");
    }

    #[test]
    fn a_command_uuid_reused_for_another_command_is_an_error() {
        // Regression (CORE-7): replay only checked that the uuid existed, so
        // any reused uuid answered "ok" whatever the command or task.
        let (_dir, state) = test_state();
        let created = apply_one(&state, &create_cmd("c-1", "t-1", "write report"), "hal");
        let (_, task_uuid) = outcome_ok(&created).unwrap().unwrap();
        let done = Command {
            kind: "task_done".into(),
            uuid: "c-1".into(),
            temp_id: None,
            args: serde_json::json!({ "task_uuid": task_uuid }),
        };
        assert!(outcome_ok(&apply_one(&state, &done, "hal")).is_err());
        assert_eq!(task_by_uuid(&state.db, &task_uuid).unwrap().status, "todo");

        // The same command replayed is still a replay, with its mapping.
        let again = apply_one(&state, &create_cmd("c-1", "t-1", "write report"), "hal");
        assert_eq!(
            outcome_ok(&again).unwrap(),
            Some(("t-1".to_string(), task_uuid))
        );
        assert_eq!(task_count(&state), 1);
    }

    #[test]
    fn command_uuids_are_scoped_to_their_client() {
        // Two clients that pick the same uuid are different commands; the
        // second used to be swallowed as the first one's replay. A client
        // uuid also cannot collide with server-derived keys (tg-cb:...).
        let (_dir, state) = test_state();
        for client in ["hal", "puresentinel"] {
            let out = apply_one(&state, &create_cmd("retry-1", "t", client), client);
            assert!(matches!(out, CommandOutcome::Applied { .. }), "{client}");
        }
        assert_eq!(task_count(&state), 2);

        // A command journaled under the raw uuid before keys were scoped
        // still replays for the client that sent it, and only for it.
        let legacy = tasks::create(
            &state.db,
            tasks::NewTask::minimal("legacy"),
            &EventCtx::sync("hal", "pre-scope"),
        )
        .unwrap();
        let replay = apply_one(&state, &create_cmd("pre-scope", "t", "legacy"), "hal");
        assert_eq!(
            outcome_ok(&replay).unwrap(),
            Some(("t".to_string(), legacy.id))
        );
        let other = apply_one(
            &state,
            &create_cmd("pre-scope", "t", "theirs"),
            "puresentinel",
        );
        assert!(matches!(other, CommandOutcome::Applied { .. }));
        assert_eq!(task_count(&state), 4);
    }

    #[tokio::test]
    async fn sync_delta_read_failure_does_not_advance_client_cursor() {
        let (_dir, state) = test_state();
        tasks::create(
            &state.db,
            tasks::NewTask::minimal("first"),
            &EventCtx::test(),
        )
        .unwrap();
        let cursor = event_log::current_cursor(&state.db).unwrap();
        let second = tasks::create(
            &state.db,
            tasks::NewTask::minimal("second"),
            &EventCtx::test(),
        )
        .unwrap();
        state
            .db
            .with_conn(|c| {
                c.execute_batch("ALTER TABLE tasks RENAME TO tasks_unavailable")?;
                Ok(())
            })
            .unwrap();
        let response = sync(
            State(state.clone()),
            HeaderMap::new(),
            Json(SyncReq {
                sync_token: Some(cursor.to_string()),
                resource_types: vec![],
                commands: vec![],
            }),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        state
            .db
            .with_conn(|c| {
                c.execute_batch("ALTER TABLE tasks_unavailable RENAME TO tasks")?;
                Ok(())
            })
            .unwrap();
        let (_, rows, deleted) = read_delta(&state, false, cursor).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, second.id);
        assert!(deleted.is_empty());
        tasks::delete_task(&state.db, &second.id, &EventCtx::test()).unwrap();
        let (_, rows, deleted) = read_delta(&state, false, cursor).unwrap();
        assert!(rows.is_empty());
        assert_eq!(deleted, vec![second.id]);
    }

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

    #[test]
    fn task_note_and_closing_notes_apply_once_and_replay() {
        let (_dir, state) = test_state();
        let task = tasks::create(
            &state.db,
            tasks::NewTask::minimal("fix dns"),
            &EventCtx::test(),
        )
        .unwrap();
        let cmd = |kind: &str, uuid: &str, args: Value| Command {
            kind: kind.into(),
            uuid: uuid.into(),
            temp_id: None,
            args,
        };
        let note = cmd(
            "task_note",
            "n-1",
            serde_json::json!({"task_uuid": task.id, "text": "resolver flapped at 03:10"}),
        );
        assert!(outcome_ok(&apply_one(&state, &note, "hal")).is_ok());
        // The retry replays: still one note.
        assert!(outcome_ok(&apply_one(&state, &note, "hal")).is_ok());
        assert_eq!(
            ptask_core::notes::list(&state.db, &task.id, 50)
                .unwrap()
                .len(),
            1
        );
        // The same key for other text is a different command.
        let reused = cmd(
            "task_note",
            "n-1",
            serde_json::json!({"task_uuid": task.id, "text": "something else"}),
        );
        let _ = apply_one(&state, &reused, "hal");
        assert_eq!(
            ptask_core::notes::list(&state.db, &task.id, 50)
                .unwrap()
                .len(),
            1
        );
        // Malformed args are refused without a write.
        for bad in [
            serde_json::json!({"task_uuid": task.id}),
            serde_json::json!({"task_uuid": task.id, "text": 7}),
            serde_json::json!({"task_uuid": task.id, "text": "  "}),
        ] {
            assert!(
                outcome_ok(&apply_one(&state, &cmd("task_note", "n-bad", bad), "hal")).is_err()
            );
        }
        assert!(
            outcome_ok(&apply_one(
                &state,
                &cmd(
                    "task_done",
                    "d-bad",
                    serde_json::json!({"task_uuid": task.id, "note": 1})
                ),
                "hal"
            ))
            .is_err()
        );
        let done = cmd(
            "task_done",
            "d-1",
            serde_json::json!({"task_uuid": task.id, "note": "unbound restarted; dig ok"}),
        );
        assert!(outcome_ok(&apply_one(&state, &done, "hal")).is_ok());
        let notes = ptask_core::notes::list(&state.db, &task.id, 50).unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(
            (
                notes[1].kind.as_str(),
                notes[1].text.as_str(),
                notes[1].actor.as_deref()
            ),
            ("done", "unbound restarted; dig ok", Some("hal"))
        );
    }
}
