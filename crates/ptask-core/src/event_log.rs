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

#[derive(Debug, Clone)]
pub struct LoggedEvent {
    pub id: i64,
    pub task_uuid: Option<String>,
    pub event_type: String,
}

/// Record an event. Returns the new `pt_event_log.id`.
pub fn record(
    db: &Db,
    uuid: &str,
    task_uuid: Option<&str>,
    event_type: &str,
    payload: &serde_json::Value,
) -> Result<i64> {
    let conn = db.get()?;
    record_in_conn(&conn, uuid, task_uuid, event_type, payload)
}

/// Record an event on an existing connection — pass a `Transaction` (it
/// derefs to `Connection`) to make the event row atomic with the mutation
/// it describes. This is the primitive `tasks::*` mutations use so a task
/// change and its sync-visible event commit or roll back together.
pub fn record_in_conn(
    conn: &rusqlite::Connection,
    uuid: &str,
    task_uuid: Option<&str>,
    event_type: &str,
    payload: &serde_json::Value,
) -> Result<i64> {
    let ts = dates::format_iso(&dates::now_in_operator_tz()?);
    let payload_str = payload.to_string();
    conn.execute(
        "INSERT INTO pt_event_log (uuid, task_uuid, event_type, payload, ts)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![uuid, task_uuid, event_type, payload_str, ts],
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
        )
        .unwrap();
        assert_eq!(id1, 1);
        let id2 = record(
            &db,
            "u2",
            Some("t2"),
            "task.created",
            &serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(id2, 2);
        assert_eq!(current_cursor(&db).unwrap(), 2);
    }

    #[test]
    fn duplicate_uuid_errors() {
        let (_dir, db) = fresh_db();
        record(&db, "u1", None, "x", &serde_json::json!({})).unwrap();
        assert!(record(&db, "u1", None, "x", &serde_json::json!({})).is_err());
    }

    #[test]
    fn exists_detects_recorded_uuid() {
        let (_dir, db) = fresh_db();
        assert!(!exists(&db, "u1").unwrap());
        record(&db, "u1", None, "x", &serde_json::json!({})).unwrap();
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
        record(&db, "u1", Some("t1"), "x", &serde_json::json!({})).unwrap();
        record(&db, "u2", Some("t2"), "x", &serde_json::json!({})).unwrap();
        let after_1 = changed_task_uuids_since(&db, 1).unwrap();
        assert_eq!(after_1, vec!["t2"]);
        let after_0 = changed_task_uuids_since(&db, 0).unwrap();
        assert_eq!(after_0, vec!["t1", "t2"]);
    }
}
