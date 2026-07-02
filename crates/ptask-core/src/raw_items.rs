//! `raw_items` — staging table for incoming captures.
//!
//! The Python distill pipeline owns this table end-to-end (until v0.9).
//! pTask writes new captures here via `pt serve`'s `/capture` endpoint
//! and the Telegram bot. Distillation picks them up, classifies, dedups,
//! and consolidates into `tasks`.

use crate::dates;
use crate::error::Result;
use crate::storage::Db;
use rusqlite::params;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RawItem {
    pub id: i64,
    pub text: String,
    pub source_type: String,
    pub source_file: String,
    pub source_date: String,
    pub commitment_score: f64,
    pub processed: bool,
    pub created_at: String,
}

/// Insert a captured item. `source_file` is a logical breadcrumb
/// (e.g. `http://capture`, `telegram:msg/123`, `email:Message-Id/...`).
pub fn insert(db: &Db, text: &str, source_type: &str, source_file: &str) -> Result<RawItem> {
    let now = dates::format_iso(&dates::now_in_operator_tz()?);
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO raw_items
            (text, source_type, source_file, source_date,
             commitment_score, processed, created_at)
         VALUES (?1, ?2, ?3, ?4, 0.0, 0, ?5)",
        params![text, source_type, source_file, now, now],
    )?;
    let id = conn.last_insert_rowid();
    Ok(RawItem {
        id,
        text: text.to_string(),
        source_type: source_type.to_string(),
        source_file: source_file.to_string(),
        source_date: now.clone(),
        commitment_score: 0.0,
        processed: false,
        created_at: now,
    })
}

/// Count of unprocessed raw items. Used by the TUI inbox badge (later phase).
pub fn unprocessed_count(db: &Db) -> Result<i64> {
    let conn = db.get()?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM raw_items WHERE processed = 0",
        [],
        |r| r.get(0),
    )?;
    Ok(n)
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
    fn insert_assigns_id_and_seeds_defaults() {
        let (_dir, db) = fresh_db();
        let r = insert(&db, "buy bread", "http", "http://capture").unwrap();
        assert!(r.id > 0);
        assert_eq!(r.text, "buy bread");
        assert_eq!(r.source_type, "http");
        assert!(!r.processed);
    }

    #[test]
    fn unprocessed_count_reflects_inserts() {
        let (_dir, db) = fresh_db();
        assert_eq!(unprocessed_count(&db).unwrap(), 0);
        insert(&db, "x", "http", "src").unwrap();
        insert(&db, "y", "http", "src").unwrap();
        assert_eq!(unprocessed_count(&db).unwrap(), 2);
    }
}

/// Mark one inbox row consumed (fast-lane or distill). Idempotent.
pub fn mark_processed(db: &Db, id: i64) -> Result<()> {
    let conn = db.get()?;
    conn.execute("UPDATE raw_items SET processed = 1 WHERE id = ?1", [id])?;
    Ok(())
}

/// Fetch a batch of unconsumed inbox rows, oldest first.
pub fn fetch_unprocessed(db: &Db, limit: usize) -> Result<Vec<RawItem>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, text, source_type, source_file, source_date,
                commitment_score, processed, created_at
         FROM raw_items WHERE processed = 0 ORDER BY id ASC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| {
        Ok(RawItem {
            id: r.get(0)?,
            text: r.get(1)?,
            source_type: r.get(2)?,
            source_file: r.get(3)?,
            source_date: r.get(4)?,
            commitment_score: r.get(5)?,
            processed: r.get::<_, i64>(6)? != 0,
            created_at: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
