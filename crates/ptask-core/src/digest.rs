//! Session-priming digest — the compact "what happened + what's next" an
//! agent loads at session start. Deterministic by design: the consumer IS a
//! model, so structured facts beat a second model's paraphrase (and can't
//! hallucinate or fail closed).

use crate::{Db, Result};

/// Counts + recently done/dismissed/created over `days`, plus the top of
/// the DAG-ready queue.
pub fn build(db: &Db, days: i64) -> Result<serde_json::Value> {
    let days = days.clamp(1, 60);
    let cutoff = format!("date('now','-{days} days')");
    let (done, dismissed, created): (Vec<serde_json::Value>, Vec<serde_json::Value>, i64) = db
        .with_conn(|c| {
            let grab = |c: &rusqlite::Connection, status: &str| {
                let mut stmt = c.prepare(&format!(
                    "SELECT pt_id, title FROM tasks
                     WHERE status_v2 = ?1 AND updated_at >= {cutoff}
                     ORDER BY updated_at DESC LIMIT 40"
                ))?;
                let rows = stmt
                    .query_map([status], |r| {
                        Ok(serde_json::json!({
                            "pt_id": r.get::<_, Option<String>>(0)?,
                            "title": r.get::<_, String>(1)?,
                        }))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok::<_, crate::Error>(rows)
            };
            let done = grab(c, "done")?;
            let dismissed = grab(c, "dismissed")?;
            let created: i64 = c.query_row(
                &format!("SELECT COUNT(*) FROM tasks WHERE created_at >= {cutoff}"),
                [],
                |r| r.get(0),
            )?;
            Ok((done, dismissed, created))
        })?;
    let ready = crate::dag::next_ready(db, 8)?
        .iter()
        .map(|t| serde_json::json!({"pt_id": t.pt_id, "title": t.title, "priority": t.priority}))
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "window_days": days,
        "done": done, "dismissed": dismissed,
        "created_count": created,
        "ready_queue": ready,
    }))
}
