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
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (id TEXT PRIMARY KEY, title TEXT, created_at TEXT NOT NULL);
                 CREATE TABLE raw_items (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    text TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    source_file TEXT NOT NULL,
                    source_date TEXT NOT NULL,
                    commitment_score REAL DEFAULT 0.0,
                    processed INTEGER DEFAULT 0,
                    created_at TEXT NOT NULL,
                    classification TEXT,
                    classification_confidence REAL DEFAULT 0.0,
                    classification_reasoning TEXT DEFAULT ''
                 );",
            )
            .unwrap();
        }
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
