//! Exact-text temporal deduplication.
//!
//! New in v0.8.5 — the Python pipeline's "temporal" stage was actually a
//! 7-day-windowed *semantic* dedup. This module is a cheaper cousin that
//! runs *before* the LLM-classified stage: an exact hash of normalised
//! source text, keyed on `(source_type, text_hash)`, recorded into
//! `pt_event_log` as `temporal_dedup.seen` rows. If we ingested the same
//! raw line from the same source in the last 7 days we skip it without
//! paying for an embedding or a HAL call.
//!
//! Normalisation collapses whitespace and lowercases the input so that
//! `"  Buy bread\n"` and `"buy bread"` hash identically — same content
//! shouldn't double-bill the pipeline just because the trim differs.
//!
//! Idempotency is provided by the `pt_event_log.uuid` column. The uuid we
//! store is itself `sha256(source_type || ":" || text_hash || ":" || day)`
//! so re-recording the same key within one day is a no-op on duplicate
//! UUIDs (event_log has a UNIQUE constraint).

use crate::Db;
use anyhow::{Result, anyhow};
use rusqlite::params;
use serde_json::json;
use sha2::{Digest, Sha256};

pub const DEFAULT_WINDOW_DAYS: i64 = 7;
const EVENT_TYPE: &str = "temporal_dedup.seen";

/// Lowercase + whitespace-collapse. ASCII whitespace only is fine here —
/// we're hashing identifiers, not displaying text.
pub fn normalise_text(s: &str) -> String {
    s.split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// 64-char hex SHA-256 of the normalised text.
pub fn text_hash(s: &str) -> String {
    let norm = normalise_text(s);
    let digest = Sha256::digest(norm.as_bytes());
    hex::encode(digest)
}

/// Composite key — `{source_type}:{hash}`.
pub fn dedup_key(source_type: &str, text: &str) -> String {
    format!("{source_type}:{}", text_hash(text))
}

/// Idempotency key for the event_log row. Includes today's UTC date so a
/// re-run on a different day doesn't collide with yesterday's seen event.
fn event_uuid(source_type: &str, text: &str) -> String {
    let day = jiff::Zoned::now()
        .with_time_zone(jiff::tz::TimeZone::UTC)
        .strftime("%Y-%m-%d")
        .to_string();
    let key = format!("{}:{}", dedup_key(source_type, text), day);
    hex::encode(Sha256::digest(key.as_bytes()))
}

/// Returns `true` if we've ingested this (source_type, normalised text) in
/// the last `window_days`, looking at `pt_event_log` rows with
/// `event_type = 'temporal_dedup.seen'`.
pub fn is_temporal_duplicate(
    db: &Db,
    source_type: &str,
    text: &str,
    window_days: i64,
) -> Result<bool> {
    let key = dedup_key(source_type, text);
    let conn = db.get()?;
    let cutoff = format!("-{} days", window_days);
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pt_event_log
         WHERE event_type = ?1
           AND ts >= datetime('now', ?2)
           AND json_extract(payload, '$.key') = ?3",
        params![EVENT_TYPE, cutoff, key],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Record one `temporal_dedup.seen` event for `(source_type, text)`.
pub fn record_seen(db: &Db, source_type: &str, text: &str) -> Result<()> {
    let key = dedup_key(source_type, text);
    let uuid = event_uuid(source_type, text);
    let payload = json!({
        "key": key,
        "source_type": source_type,
        "text_hash": text_hash(text),
    });
    // Best-effort: the UNIQUE constraint on pt_event_log.uuid means a same-day
    // re-record is silently absorbed.
    let conn = db.get()?;
    let res = conn.execute(
        "INSERT OR IGNORE INTO pt_event_log (uuid, task_uuid, event_type, payload, ts)
         VALUES (?1, NULL, ?2, ?3, datetime('now'))",
        params![uuid, EVENT_TYPE, payload.to_string()],
    );
    match res {
        Ok(_) => Ok(()),
        Err(e) => Err(anyhow!("record_seen: {e}")),
    }
}

/// Convenience: if `(source_type, text)` is a dup within `window_days` →
/// return `true` without writing anything. Otherwise → record + return `false`.
pub fn check_and_record(db: &Db, source_type: &str, text: &str, window_days: i64) -> Result<bool> {
    if is_temporal_duplicate(db, source_type, text, window_days)? {
        return Ok(true);
    }
    record_seen(db, source_type, text)?;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptask_core::Db;

    fn fresh_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        // V008 bootstraps the production-shape legacy schema — no stub.
        (dir, Db::open(&path).unwrap())
    }

    #[test]
    fn normalise_collapses_whitespace_and_lowercases() {
        assert_eq!(normalise_text("  Buy   Bread  "), "buy bread");
        assert_eq!(normalise_text("HELLO\nworld"), "hello world");
        assert_eq!(normalise_text("a  b\t\tc"), "a b c");
    }

    #[test]
    fn text_hash_stable_across_normalisation() {
        let a = text_hash("Buy bread");
        let b = text_hash("  buy   BREAD  ");
        assert_eq!(a, b, "normalisation differences should hash identical");
    }

    #[test]
    fn text_hash_distinct_for_different_content() {
        let a = text_hash("buy bread");
        let b = text_hash("buy milk");
        assert_ne!(a, b);
    }

    #[test]
    fn dedup_key_includes_source_type() {
        let a = dedup_key("voice_kb", "buy bread");
        let b = dedup_key("email", "buy bread");
        assert_ne!(a, b, "same text from different sources keys differently");
    }

    #[test]
    fn check_and_record_first_call_records_second_call_dup() {
        let (_d, db) = fresh_db();
        let dup1 = check_and_record(&db, "voice_kb", "buy bread", 7).unwrap();
        assert!(!dup1, "first call should not be flagged as duplicate");
        let dup2 = check_and_record(&db, "voice_kb", "BUY BREAD", 7).unwrap();
        assert!(
            dup2,
            "second call (with case-normalised input) should be dup"
        );
    }

    #[test]
    fn check_and_record_different_source_independent() {
        let (_d, db) = fresh_db();
        check_and_record(&db, "voice_kb", "buy bread", 7).unwrap();
        let dup = check_and_record(&db, "email", "buy bread", 7).unwrap();
        assert!(!dup, "different source_type should not collide");
    }

    #[test]
    fn is_temporal_duplicate_negative_when_unseen() {
        let (_d, db) = fresh_db();
        assert!(!is_temporal_duplicate(&db, "voice_kb", "buy bread", 7).unwrap());
    }

    #[test]
    fn window_filter_excludes_old_events() {
        let (_d, db) = fresh_db();
        record_seen(&db, "voice_kb", "buy bread").unwrap();
        // Backdate the row to 10 days ago.
        db.with_conn(|c| {
            c.execute(
                "UPDATE pt_event_log SET ts = datetime('now', '-10 days')
                 WHERE event_type = 'temporal_dedup.seen'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        // With a 7-day window, the 10-day-old event should be invisible.
        assert!(!is_temporal_duplicate(&db, "voice_kb", "buy bread", 7).unwrap());
        // With a 14-day window, it should resurface.
        assert!(is_temporal_duplicate(&db, "voice_kb", "buy bread", 14).unwrap());
    }
}
