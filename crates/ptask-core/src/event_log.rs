//! Append-only event log (`pt_event_log`).
//!
//! Records every state-changing operation that should propagate through
//! the sync API. The `id` column is the monotonic sync cursor. The `uuid`
//! column is the caller-supplied idempotency key (a `task_create` retried
//! with the same uuid returns `ok` without re-creating).

use crate::dates;
use crate::error::Result;
use crate::storage::Db;
use rusqlite::OptionalExtension;
use rusqlite::params;

/// Who performed a mutation, through which surface, and (optionally) under
/// which idempotency key. Required by every event-emitting mutation — the
/// compiler enforces attribution, which is what makes `pt log`, undo, the
/// activity feed, and per-agent audit possible. Before v1.17.0 HAL, the
/// operator CLI, puresentinel, the dashboard, and webhooks were
/// indistinguishable in the journal.
#[derive(Debug, Clone)]
pub struct EventCtx {
    /// Stable identity: "shell", a token client_id ("hal", "puresentinel",
    /// "dashboard", …), "accountability", "distill", "webhook:gitea".
    pub actor: String,
    /// Surface the mutation arrived through: "cli", "sync", "capture",
    /// "webhook", "accountability", "distill".
    pub source: String,
    /// Idempotency key. `Some` = caller-supplied (e.g. a /sync command
    /// uuid — replays return ok without re-applying). `None` = a generated
    /// `local:` uuid.
    pub event_uuid: Option<String>,
    /// What the keyed command was (verb + a hash of its canonical
    /// arguments), journaled as `payload.cmd` so a retry under the same key
    /// can be told apart from a different command that reused it.
    pub command: Option<CommandFingerprint>,
}

/// Identity of one keyed command: its verb and the SHA-256 of its canonical
/// arguments. Two commands under one idempotency key are the same command
/// only when both match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFingerprint {
    pub verb: String,
    pub args_sha256: String,
}

impl CommandFingerprint {
    /// `canonical_args` must be deterministic for the same command (sorted
    /// JSON, a fixed Debug rendering, ...).
    pub fn new(verb: &str, canonical_args: &str) -> Self {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(canonical_args.as_bytes());
        Self {
            verb: verb.to_string(),
            args_sha256: digest.iter().map(|b| format!("{b:02x}")).collect(),
        }
    }
}

impl EventCtx {
    /// A locally-initiated mutation (CLI/TUI) by `actor`.
    pub fn local(actor: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            source: "cli".into(),
            event_uuid: None,
            command: None,
        }
    }

    /// A /sync command from an authenticated client, keyed for idempotency.
    pub fn sync(client_id: impl Into<String>, cmd_uuid: impl Into<String>) -> Self {
        Self {
            actor: client_id.into(),
            source: "sync".into(),
            event_uuid: Some(cmd_uuid.into()),
            command: None,
        }
    }

    /// An internal engine acting on its own schedule (accountability,
    /// distill): actor and source are the engine name.
    pub fn system(name: &str) -> Self {
        Self {
            actor: name.into(),
            source: name.into(),
            event_uuid: None,
            command: None,
        }
    }

    /// A webhook-driven mutation, keyed by the delivery for idempotency.
    pub fn webhook(provider: &str, delivery_uuid: impl Into<String>) -> Self {
        Self {
            actor: format!("webhook:{provider}"),
            source: "webhook".into(),
            event_uuid: Some(delivery_uuid.into()),
            command: None,
        }
    }

    /// Test fixture identity.
    pub fn test() -> Self {
        Self {
            actor: "test".into(),
            source: "test".into(),
            event_uuid: None,
            command: None,
        }
    }

    /// Same identity, different idempotency key.
    pub fn with_uuid(&self, uuid: impl Into<String>) -> Self {
        Self {
            actor: self.actor.clone(),
            source: self.source.clone(),
            event_uuid: Some(uuid.into()),
            command: self.command.clone(),
        }
    }

    /// Same identity, carrying the keyed command's fingerprint.
    pub fn with_command(mut self, command: CommandFingerprint) -> Self {
        self.command = Some(command);
        self
    }
}

#[derive(Debug, Clone)]
pub struct LoggedEvent {
    pub id: i64,
    pub task_uuid: Option<String>,
    pub event_type: String,
    /// NULL for events from before actor attribution (V009).
    pub actor: Option<String>,
    /// The keyed command's fingerprint; `None` for events journaled before
    /// fingerprints, or without a key.
    pub command: Option<CommandFingerprint>,
}

/// True for a client-supplied idempotency key in the namespace the capture
/// fast lane reserves: its `task.created` events are keyed
/// `capture:<raw_items id>` and that key marks a task as capture-created
/// (what /capture/resolve may close). Case-insensitive, so no spelling of
/// the prefix is accepted from a client.
pub fn is_reserved_client_key(key: &str) -> bool {
    key.as_bytes()
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case(b"capture"))
}

/// Record an attributed event. Returns the new `pt_event_log.id`.
pub fn record(
    db: &Db,
    uuid: &str,
    task_uuid: Option<&str>,
    event_type: &str,
    payload: &serde_json::Value,
    ctx: &EventCtx,
) -> Result<i64> {
    let conn = db.get()?;
    record_in_conn(&conn, uuid, task_uuid, event_type, payload, ctx)
}

/// Record an attributed event on an existing connection — pass a
/// `Transaction` (it derefs to `Connection`) to make the event row atomic
/// with the mutation it describes. This is the primitive `tasks::*`
/// mutations use so a task change and its sync-visible event commit or
/// roll back together.
///
/// The actor lands both in the `actor` column (queryable: `pt log`,
/// activity feed) and inside the payload envelope (self-contained events
/// for downstream consumers). Existing payload keys are preserved.
pub fn record_in_conn(
    conn: &rusqlite::Connection,
    uuid: &str,
    task_uuid: Option<&str>,
    event_type: &str,
    payload: &serde_json::Value,
    ctx: &EventCtx,
) -> Result<i64> {
    let ts = dates::format_iso(&dates::now_in_operator_tz()?);
    let mut enveloped = payload.clone();
    if let Some(obj) = enveloped.as_object_mut() {
        obj.insert("actor".into(), serde_json::json!(ctx.actor));
        obj.insert("source".into(), serde_json::json!(ctx.source));
        if let Some(cmd) = &ctx.command {
            obj.insert(
                "cmd".into(),
                serde_json::json!({ "verb": cmd.verb, "args_sha256": cmd.args_sha256 }),
            );
        }
    }
    let payload_str = enveloped.to_string();
    conn.execute(
        "INSERT INTO pt_event_log (uuid, task_uuid, event_type, payload, ts, actor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![uuid, task_uuid, event_type, payload_str, ts, ctx.actor],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Fetch a recorded command/event by idempotency UUID.
pub fn get_by_uuid(db: &Db, uuid: &str) -> Result<Option<LoggedEvent>> {
    let conn = db.get()?;
    let found = conn
        .query_row(
            "SELECT id, task_uuid, event_type, actor,
                    json_extract(payload, '$.cmd.verb'),
                    json_extract(payload, '$.cmd.args_sha256')
             FROM pt_event_log WHERE uuid = ?1",
            [uuid],
            |r| {
                let verb: Option<String> = r.get(4)?;
                let args_sha256: Option<String> = r.get(5)?;
                Ok(LoggedEvent {
                    id: r.get(0)?,
                    task_uuid: r.get(1)?,
                    event_type: r.get(2)?,
                    actor: r.get(3)?,
                    command: verb
                        .zip(args_sha256)
                        .map(|(verb, args_sha256)| CommandFingerprint { verb, args_sha256 }),
                })
            },
        )
        .optional()?;
    Ok(found)
}

/// What a retried keyed command expects its earlier event to look like.
#[derive(Debug, Clone, Copy)]
pub struct ReplayCheck<'a> {
    /// The caller: a key journaled by another actor is never its replay.
    pub actor: &'a str,
    /// The task the command names, when it still resolves.
    pub task_uuid: Option<&'a str>,
    /// Event types the command writes; empty skips this check.
    pub event_types: &'a [&'a str],
    /// The command's fingerprint, compared with the journaled one.
    pub command: Option<&'a CommandFingerprint>,
}

/// Is `event` (already journaled under idempotency key `key`) a replay of
/// the command now being retried? Only when the same actor journaled it,
/// with the same command fingerprint (verb and canonical arguments) — or,
/// for an event from before fingerprints, one of the expected event types —
/// on the same task. Anything else is a key reused for a different command:
/// an error, never a silent "ok" that skips the new command.
pub fn verify_replay(key: &str, event: &LoggedEvent, expect: &ReplayCheck<'_>) -> Result<()> {
    let reused = |what: &str| {
        Err(crate::Error::Other(format!(
            "idempotency key {key:?} was already used {what}; nothing was applied — use a fresh key"
        )))
    };
    if event.actor.as_deref() != Some(expect.actor) {
        return reused("by another client");
    }
    if !expect.event_types.is_empty() && !expect.event_types.contains(&event.event_type.as_str()) {
        return reused(&format!("for a different command ({})", event.event_type));
    }
    if let (Some(journaled), Some(now)) = (&event.command, expect.command)
        && journaled != now
    {
        return reused(&format!(
            "for a different command or arguments ({})",
            journaled.verb
        ));
    }
    if let Some(expected) = expect.task_uuid
        && event.task_uuid.as_deref() != Some(expected)
    {
        return reused("for another task");
    }
    Ok(())
}

/// The replay check every keyed mutation runs first: `Ok(None)` when `key`
/// has not been used, `Ok(Some(event))` when this is a retry of the same
/// command (report success without re-applying), and an error when the key
/// was used for something else (see [`verify_replay`]).
pub fn check_replay(db: &Db, key: &str, expect: &ReplayCheck<'_>) -> Result<Option<LoggedEvent>> {
    let Some(event) = get_by_uuid(db, key)? else {
        return Ok(None);
    };
    verify_replay(key, &event, expect)?;
    Ok(Some(event))
}

/// Highest `id` currently in the log, or 0 if empty. The sync token.
pub fn current_cursor(db: &Db) -> Result<i64> {
    let conn = db.get()?;
    let n: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM pt_event_log", [], |r| {
        r.get(0)
    })?;
    Ok(n)
}

/// `task_uuid` values for events with `id > since`. Use to fetch deltas.
pub fn changed_task_uuids_since(db: &Db, since: i64) -> Result<Vec<String>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT DISTINCT task_uuid FROM pt_event_log
         WHERE id > ?1 AND task_uuid IS NOT NULL
         ORDER BY task_uuid",
    )?;
    let rows = stmt.query_map([since], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// One row of a task's attributed history, newest first.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistoryEvent {
    pub id: i64,
    pub ts: String,
    pub actor: Option<String>,
    pub event_type: String,
    pub payload: String,
}

/// Attributed history for one task, newest first. Powers `pt log PT-N` and
/// the cockpit activity feed. `actor` is NULL for pre-v1.17 events.
pub fn history_for_task(db: &Db, task_uuid: &str, limit: usize) -> Result<Vec<HistoryEvent>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, ts, actor, event_type, payload FROM pt_event_log
         WHERE task_uuid = ?1 ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![task_uuid, limit as i64], |r| {
        Ok(HistoryEvent {
            id: r.get(0)?,
            ts: r.get(1)?,
            actor: r.get(2)?,
            event_type: r.get(3)?,
            payload: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Newest `limit` events, plus every criteria event on the task so a
/// cockpit drawer can fold the checklist from the full trail, not only
/// the history window.
pub fn history_for_drawer(db: &Db, task_uuid: &str, limit: usize) -> Result<Vec<HistoryEvent>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(&format!(
        "SELECT id, ts, actor, event_type, payload FROM pt_event_log
          WHERE task_uuid = ?1
            AND (
              event_type IN ({})
              OR id IN (
                SELECT id FROM pt_event_log
                 WHERE task_uuid = ?1
                 ORDER BY id DESC
                 LIMIT ?2
              )
            )
          ORDER BY id DESC",
        crate::criteria::EVENTS
    ))?;
    let rows = stmt.query_map(params![task_uuid, limit as i64], |r| {
        Ok(HistoryEvent {
            id: r.get(0)?,
            ts: r.get(1)?,
            actor: r.get(2)?,
            event_type: r.get(3)?,
            payload: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Tombstones: `task_uuid` values with a `task.deleted` event after the
/// cursor. Delta clients drop these from their local state.
pub fn deleted_task_uuids_since(db: &Db, since: i64) -> Result<Vec<String>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT DISTINCT task_uuid FROM pt_event_log
         WHERE id > ?1 AND task_uuid IS NOT NULL AND event_type = 'task.deleted'
         ORDER BY task_uuid",
    )?;
    let rows = stmt.query_map([since], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        // V008 bootstraps the production-shape legacy schema — no stub.
        (dir, Db::open(&path).unwrap())
    }

    #[test]
    fn record_and_cursor_advance() {
        let (_dir, db) = fresh_db();
        assert_eq!(current_cursor(&db).unwrap(), 0);
        let id1 = record(
            &db,
            "u1",
            Some("t1"),
            "task.created",
            &serde_json::json!({}),
            &EventCtx::test(),
        )
        .unwrap();
        assert_eq!(id1, 1);
        let id2 = record(
            &db,
            "u2",
            Some("t2"),
            "task.created",
            &serde_json::json!({}),
            &EventCtx::test(),
        )
        .unwrap();
        assert_eq!(id2, 2);
        assert_eq!(current_cursor(&db).unwrap(), 2);
    }

    #[test]
    fn duplicate_uuid_errors() {
        let (_dir, db) = fresh_db();
        record(
            &db,
            "u1",
            None,
            "x",
            &serde_json::json!({}),
            &EventCtx::test(),
        )
        .unwrap();
        assert!(
            record(
                &db,
                "u1",
                None,
                "x",
                &serde_json::json!({}),
                &EventCtx::test()
            )
            .is_err()
        );
    }

    #[test]
    fn get_by_uuid_returns_recorded_task_uuid() {
        let (_dir, db) = fresh_db();
        record(
            &db,
            "u1",
            Some("task-1"),
            "task.created",
            &serde_json::json!({}),
            &EventCtx::test(),
        )
        .unwrap();
        let event = get_by_uuid(&db, "u1").unwrap().unwrap();
        assert_eq!(event.id, 1);
        assert_eq!(event.task_uuid.as_deref(), Some("task-1"));
        assert_eq!(event.event_type, "task.created");
    }

    #[test]
    fn check_replay_accepts_the_same_command_and_rejects_reuse() {
        let (_dir, db) = fresh_db();
        record(
            &db,
            "k1",
            Some("task-1"),
            "task.created",
            &serde_json::json!({}),
            &EventCtx::test(),
        )
        .unwrap();
        let expect = |task: &'static str, types: &'static [&'static str]| ReplayCheck {
            actor: "test",
            task_uuid: Some(task),
            event_types: types,
            command: None,
        };
        let created: &[&str] = &["task.created"];
        assert!(
            check_replay(&db, "unused", &expect("task-1", created))
                .unwrap()
                .is_none()
        );
        let replay = check_replay(&db, "k1", &expect("task-1", created)).unwrap();
        assert_eq!(replay.unwrap().actor.as_deref(), Some("test"));
        // Same key, different command, task or actor: an error.
        assert!(check_replay(&db, "k1", &expect("task-1", &["task.completed"])).is_err());
        assert!(check_replay(&db, "k1", &expect("task-2", created)).is_err());
        let other_actor = ReplayCheck {
            actor: "hal",
            ..expect("task-1", created)
        };
        assert!(check_replay(&db, "k1", &other_actor).is_err());

        // A fingerprinted event matches only the same command.
        let add = CommandFingerprint::new("add", r#"{"title":"x"}"#);
        record(
            &db,
            "k2",
            Some("task-3"),
            "task.created",
            &serde_json::json!({}),
            &EventCtx::test().with_uuid("k2").with_command(add.clone()),
        )
        .unwrap();
        let with = |cmd| ReplayCheck {
            command: Some(cmd),
            ..expect("task-3", created)
        };
        assert!(check_replay(&db, "k2", &with(&add)).unwrap().is_some());
        let other = CommandFingerprint::new("add", r#"{"title":"y"}"#);
        assert!(check_replay(&db, "k2", &with(&other)).is_err());
    }

    #[test]
    fn changed_uuids_filters_by_cursor() {
        let (_dir, db) = fresh_db();
        record(
            &db,
            "u1",
            Some("t1"),
            "x",
            &serde_json::json!({}),
            &EventCtx::test(),
        )
        .unwrap();
        record(
            &db,
            "u2",
            Some("t2"),
            "x",
            &serde_json::json!({}),
            &EventCtx::test(),
        )
        .unwrap();
        let after_1 = changed_task_uuids_since(&db, 1).unwrap();
        assert_eq!(after_1, vec!["t2"]);
        let after_0 = changed_task_uuids_since(&db, 0).unwrap();
        assert_eq!(after_0, vec!["t1", "t2"]);
    }
}
