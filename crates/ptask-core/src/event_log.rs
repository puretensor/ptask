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
}

impl EventCtx {
    /// A locally-initiated mutation (CLI/TUI) by `actor`.
    pub fn local(actor: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            source: "cli".into(),
            event_uuid: None,
        }
    }

    /// A /sync command from an authenticated client, keyed for idempotency.
    pub fn sync(client_id: impl Into<String>, cmd_uuid: impl Into<String>) -> Self {
        Self {
            actor: client_id.into(),
            source: "sync".into(),
            event_uuid: Some(cmd_uuid.into()),
        }
    }

    /// An internal engine acting on its own schedule (accountability,
    /// distill): actor and source are the engine name.
    pub fn system(name: &str) -> Self {
        Self {
            actor: name.into(),
            source: name.into(),
            event_uuid: None,
        }
    }

    /// A webhook-driven mutation, keyed by the delivery for idempotency.
    pub fn webhook(provider: &str, delivery_uuid: impl Into<String>) -> Self {
        Self {
            actor: format!("webhook:{provider}"),
            source: "webhook".into(),
            event_uuid: Some(delivery_uuid.into()),
        }
    }

    /// Test fixture identity.
    pub fn test() -> Self {
        Self {
            actor: "test".into(),
            source: "test".into(),
            event_uuid: None,
        }
    }

    /// Same identity, different idempotency key.
    pub fn with_uuid(&self, uuid: impl Into<String>) -> Self {
        Self {
            actor: self.actor.clone(),
            source: self.source.clone(),
            event_uuid: Some(uuid.into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoggedEvent {
    pub id: i64,
    pub task_uuid: Option<String>,
    pub event_type: String,
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
    }
    let payload_str = enveloped.to_string();
    conn.execute(
        "INSERT INTO pt_event_log (uuid, task_uuid, event_type, payload, ts, actor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![uuid, task_uuid, event_type, payload_str, ts, ctx.actor],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Check whether a command UUID has been processed already.
/// Used by the sync API for idempotent retries.
pub fn exists(db: &Db, uuid: &str) -> Result<bool> {
    let conn = db.get()?;
    let found: Option<i64> = conn
        .query_row("SELECT id FROM pt_event_log WHERE uuid = ?1", [uuid], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(found.is_some())
}

/// Fetch a recorded command/event by idempotency UUID.
pub fn get_by_uuid(db: &Db, uuid: &str) -> Result<Option<LoggedEvent>> {
    let conn = db.get()?;
    let found = conn
        .query_row(
            "SELECT id, task_uuid, event_type FROM pt_event_log WHERE uuid = ?1",
            [uuid],
            |r| {
                Ok(LoggedEvent {
                    id: r.get(0)?,
                    task_uuid: r.get(1)?,
                    event_type: r.get(2)?,
                })
            },
        )
        .optional()?;
    Ok(found)
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
#[derive(Debug, Clone)]
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
    fn exists_detects_recorded_uuid() {
        let (_dir, db) = fresh_db();
        assert!(!exists(&db, "u1").unwrap());
        record(
            &db,
            "u1",
            None,
            "x",
            &serde_json::json!({}),
            &EventCtx::test(),
        )
        .unwrap();
        assert!(exists(&db, "u1").unwrap());
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
