//! PT-N identifier minting and lookup.
//!
//! Every task in `tasks` gets a side-table row in `pt_extensions` with a
//! human-readable `pt_id` of the form `PT-<n>`. The counter persists in
//! `pt_counters` and is monotonically increasing per database.

use crate::error::{Error, Result};
use crate::storage::Db;
use rusqlite::params;
use tracing::{debug, info};

/// Format a counter integer as the canonical `PT-N` string.
pub fn format_pt_id(n: i64) -> String {
    format!("PT-{}", n)
}

/// Read the current `pt_id` counter. 0 means none minted.
pub fn current_counter(conn: &rusqlite::Connection) -> Result<i64> {
    let n: i64 = conn.query_row(
        "SELECT value FROM pt_counters WHERE name='pt_id'",
        [],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Mint a fresh PT-N. Advances the counter and inserts an extension row.
/// Caller supplies the parent `task_uuid`; row must already exist in `tasks`.
pub fn mint_for(conn: &mut rusqlite::Connection, task_uuid: &str) -> Result<String> {
    let tx = conn.transaction()?;
    let n: i64 = tx.query_row(
        "UPDATE pt_counters SET value = value + 1 WHERE name='pt_id' RETURNING value",
        [],
        |r| r.get(0),
    )?;
    let pt_id = format_pt_id(n);
    tx.execute(
        "INSERT INTO pt_extensions (task_uuid, pt_id, created_by_pt) VALUES (?1, ?2, 1)",
        params![task_uuid, pt_id],
    )?;
    tx.commit()?;
    Ok(pt_id)
}

/// Resolve a PT-N string to the underlying `tasks.id` UUID.
pub fn lookup_uuid(conn: &rusqlite::Connection, pt_id: &str) -> Result<String> {
    let uuid: Option<String> = conn
        .query_row(
            "SELECT task_uuid FROM pt_extensions WHERE pt_id = ?1",
            [pt_id],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    uuid.ok_or_else(|| Error::PtIdNotFound(pt_id.to_string()))
}

/// Resolve a `tasks.id` UUID to its PT-N, if any.
pub fn lookup_pt_id(conn: &rusqlite::Connection, task_uuid: &str) -> Result<Option<String>> {
    let pt_id: Option<String> = conn
        .query_row(
            "SELECT pt_id FROM pt_extensions WHERE task_uuid = ?1",
            [task_uuid],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    Ok(pt_id)
}

/// One-shot backfill: iterate existing rows in `tasks` (ordered by created_at)
/// and mint PT-N for any without an extension row. Idempotent — re-running is a
/// no-op once all rows are minted. Returns the number of new IDs minted.
pub fn backfill_all(db: &Db) -> Result<usize> {
    let mut conn = db.get()?;
    let tx = conn.transaction()?;

    // Tasks lacking a pt_extensions row, in creation order.
    let mut stmt = tx.prepare(
        "SELECT t.id FROM tasks t
         LEFT JOIN pt_extensions x ON x.task_uuid = t.id
         WHERE x.task_uuid IS NULL
         ORDER BY t.created_at ASC, t.id ASC",
    )?;
    let pending: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);

    let mut minted = 0usize;
    let mut current: i64 = tx.query_row(
        "SELECT value FROM pt_counters WHERE name='pt_id'",
        [],
        |r| r.get(0),
    )?;

    for uuid in pending {
        current += 1;
        let pt_id = format_pt_id(current);
        tx.execute(
            "INSERT INTO pt_extensions (task_uuid, pt_id, created_by_pt) VALUES (?1, ?2, 0)",
            params![uuid, pt_id],
        )?;
        debug!(target: "ptask::pt_id", pt_id = %pt_id, task = %uuid, "minted");
        minted += 1;
    }

    tx.execute(
        "UPDATE pt_counters SET value = ?1 WHERE name='pt_id'",
        params![current],
    )?;
    tx.commit()?;

    if minted > 0 {
        info!(target: "ptask::pt_id", count = minted, "backfill complete");
    }
    Ok(minted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Db;

    fn setup_db_with_tasks(tasks: &[(&str, &str, &str)]) -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (
                    id         TEXT PRIMARY KEY,
                    title      TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );",
            )
            .unwrap();
            for (id, title, ts) in tasks {
                conn.execute(
                    "INSERT INTO tasks (id, title, created_at) VALUES (?1, ?2, ?3)",
                    params![id, title, ts],
                )
                .unwrap();
            }
        }
        let db = Db::open(&path).unwrap();
        (dir, db)
    }

    #[test]
    fn backfill_assigns_sequential_pt_ids_in_created_at_order() {
        let (_dir, db) = setup_db_with_tasks(&[
            ("uuid-c", "third", "2026-01-03T00:00:00Z"),
            ("uuid-a", "first", "2026-01-01T00:00:00Z"),
            ("uuid-b", "second", "2026-01-02T00:00:00Z"),
        ]);
        let minted = backfill_all(&db).unwrap();
        assert_eq!(minted, 3);

        db.with_conn(|c| {
            assert_eq!(lookup_pt_id(c, "uuid-a").unwrap(), Some("PT-1".into()));
            assert_eq!(lookup_pt_id(c, "uuid-b").unwrap(), Some("PT-2".into()));
            assert_eq!(lookup_pt_id(c, "uuid-c").unwrap(), Some("PT-3".into()));
            assert_eq!(current_counter(c).unwrap(), 3);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn backfill_is_idempotent() {
        let (_dir, db) = setup_db_with_tasks(&[
            ("uuid-a", "first", "2026-01-01T00:00:00Z"),
            ("uuid-b", "second", "2026-01-02T00:00:00Z"),
        ]);
        assert_eq!(backfill_all(&db).unwrap(), 2);
        assert_eq!(backfill_all(&db).unwrap(), 0); // nothing new to mint
        db.with_conn(|c| {
            assert_eq!(current_counter(c).unwrap(), 2);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn mint_for_advances_counter() {
        let (_dir, db) = setup_db_with_tasks(&[("uuid-a", "first", "2026-01-01T00:00:00Z")]);
        backfill_all(&db).unwrap();

        let mut conn = db.get().unwrap();
        // Add a fresh row to `tasks` then mint.
        conn.execute(
            "INSERT INTO tasks (id, title, created_at) VALUES ('uuid-new', 'new', '2026-02-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let pt_id = mint_for(&mut conn, "uuid-new").unwrap();
        assert_eq!(pt_id, "PT-2");

        let uuid = lookup_uuid(&conn, "PT-2").unwrap();
        assert_eq!(uuid, "uuid-new");
    }

    #[test]
    fn lookup_uuid_missing_returns_error() {
        let (_dir, db) = setup_db_with_tasks(&[]);
        let conn = db.get().unwrap();
        let err = lookup_uuid(&conn, "PT-999").unwrap_err();
        assert!(matches!(err, Error::PtIdNotFound(_)));
    }
}
