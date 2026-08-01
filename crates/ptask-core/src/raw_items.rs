//! `raw_items` — staging table for incoming captures.
//!
//! The native Rust distill pipeline owns this table end-to-end.
//! pTask writes new captures here via `pt serve`'s `/capture` endpoint
//! and the Telegram bot. Distillation picks them up, classifies, dedups,
//! and consolidates into `tasks`.

use crate::dates;
use crate::error::Result;
use crate::storage::Db;
use rusqlite::params;
use serde::Serialize;

/// How many isolated distill failures a row may accumulate before it is
/// quarantined — served no more, so one unprocessable capture can never wedge
/// the pipeline. Kept here because both the fetch query and the pipeline's
/// bookkeeping have to agree on it.
pub const MAX_DISTILL_ATTEMPTS: i64 = 3;

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
    /// Isolated distill failures charged to this row (V013).
    pub distill_attempts: i64,
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
        distill_attempts: 0,
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

/// Mark one inbox row consumed (fast-lane or distill). Idempotent.
pub fn mark_processed(db: &Db, id: i64) -> Result<()> {
    let conn = db.get()?;
    conn.execute("UPDATE raw_items SET processed = 1 WHERE id = ?1", [id])?;
    Ok(())
}

/// Fetch a batch of unconsumed inbox rows, oldest first.
///
/// Quarantined rows (`distill_attempts >= MAX_DISTILL_ATTEMPTS`) are skipped:
/// they have each failed in isolation on a run where the provider was
/// demonstrably working, so re-serving them only re-wedges the head of the
/// queue. They stay in the table with their `distill_error` for triage.
pub fn fetch_unprocessed(db: &Db, limit: usize) -> Result<Vec<RawItem>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, text, source_type, source_file, source_date,
                commitment_score, processed, created_at, distill_attempts
         FROM raw_items
         WHERE processed = 0 AND distill_attempts < ?1
         ORDER BY id ASC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![MAX_DISTILL_ATTEMPTS, limit as i64], |r| {
        Ok(RawItem {
            id: r.get(0)?,
            text: r.get(1)?,
            source_type: r.get(2)?,
            source_file: r.get(3)?,
            source_date: r.get(4)?,
            commitment_score: r.get(5)?,
            processed: r.get::<_, i64>(6)? != 0,
            created_at: r.get(7)?,
            distill_attempts: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Charge one isolated distill failure to a row and record why. Returns the
/// row's new attempt count so the caller can tell when it crossed into
/// quarantine.
pub fn record_distill_failure(db: &Db, id: i64, error: &str) -> Result<i64> {
    let conn = db.get()?;
    conn.execute(
        "UPDATE raw_items
            SET distill_attempts = distill_attempts + 1, distill_error = ?1
          WHERE id = ?2",
        params![error, id],
    )?;
    let attempts: i64 = conn.query_row(
        "SELECT distill_attempts FROM raw_items WHERE id = ?1",
        [id],
        |r| r.get(0),
    )?;
    Ok(attempts)
}

/// Rows parked out of the distill queue after repeated isolated failures.
/// Surfaced as a gauge so a poison capture is loud instead of invisible.
pub fn quarantined_count(db: &Db) -> Result<i64> {
    let conn = db.get()?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM raw_items
          WHERE processed = 0 AND distill_attempts >= ?1",
        [MAX_DISTILL_ATTEMPTS],
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

    /// Regression (#37.3): `fetch_unprocessed` served every `processed = 0`
    /// row oldest-first, so a capture the distiller could never handle came
    /// back at the head of the queue on every run and blocked everything
    /// behind it. Once its attempts are exhausted it must stop being served.
    #[test]
    fn a_quarantined_row_stops_being_served_but_stays_countable() {
        let (_dir, db) = fresh_db();
        let poison = insert(&db, "unclassifiable", "http", "src").unwrap();
        let good = insert(&db, "a real commitment", "http", "src").unwrap();

        for expected in 1..=MAX_DISTILL_ATTEMPTS {
            assert_eq!(
                record_distill_failure(&db, poison.id, "no text part in response").unwrap(),
                expected
            );
        }

        let served: Vec<i64> = fetch_unprocessed(&db, 10)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(served, vec![good.id], "the poison row is no longer served");
        assert_eq!(quarantined_count(&db).unwrap(), 1);
        // Still in the table with its reason — parked, not deleted.
        assert_eq!(unprocessed_count(&db).unwrap(), 2);
        let reason: String = db
            .get()
            .unwrap()
            .query_row(
                "SELECT distill_error FROM raw_items WHERE id = ?1",
                [poison.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reason, "no text part in response");
    }

    #[test]
    fn a_row_below_the_ceiling_is_still_served() {
        let (_dir, db) = fresh_db();
        let item = insert(&db, "transiently awkward", "http", "src").unwrap();
        record_distill_failure(&db, item.id, "one bad run").unwrap();
        let served = fetch_unprocessed(&db, 10).unwrap();
        assert_eq!(served.len(), 1);
        assert_eq!(served[0].distill_attempts, 1);
        assert_eq!(quarantined_count(&db).unwrap(), 0);
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
