//! `pt_webhook_log` — audit of inbound + outbound webhook envelopes.

use crate::dates;
use crate::error::Result;
use crate::storage::Db;
use rusqlite::params;

/// Direction of a logged webhook.
#[derive(Debug, Copy, Clone)]
pub enum Direction {
    In,
    Out,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Direction::In => "in",
            Direction::Out => "out",
        }
    }
}

/// Record a webhook envelope. Returns the new row id.
pub fn record(
    db: &Db,
    direction: Direction,
    source: &str,
    payload: &serde_json::Value,
    signature_ok: bool,
) -> Result<i64> {
    let ts = dates::format_iso(&dates::now_in_operator_tz()?);
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO pt_webhook_log (direction, source, payload, signature_ok, ts)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            direction.as_str(),
            source,
            payload.to_string(),
            if signature_ok { 1 } else { 0 },
            ts
        ],
    )?;
    Ok(conn.last_insert_rowid())
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
    fn records_direction_source_and_signature_ok() {
        let (_dir, db) = fresh_db();
        let id = record(
            &db,
            Direction::Out,
            "https://example.test/hook",
            &serde_json::json!({"event": "task.created"}),
            true,
        )
        .unwrap();
        assert!(id > 0);
        db.with_conn(|c| {
            let (dir, source, ok): (String, String, i64) = c
                .query_row(
                    "SELECT direction, source, signature_ok FROM pt_webhook_log WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(dir, "out");
            assert_eq!(source, "https://example.test/hook");
            assert_eq!(ok, 1);
            Ok(())
        })
        .unwrap();
    }
}
