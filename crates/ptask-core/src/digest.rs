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
            // Keyed on when the task was closed, not on updated_at: linking
            // an old done task to a goal (or any later touch) bumps
            // updated_at and must not make it "recently done". A terminal
            // task's latest status_change interaction is the one that closed
            // it (reopening changes the status); rows with none fall back to
            // updated_at.
            let grab = |c: &rusqlite::Connection, status: &str| {
                let mut stmt = c.prepare(&format!(
                    "SELECT pt_id, title, id FROM (
                         SELECT t.id, t.pt_id, t.title,
                                COALESCE((SELECT MAX(i.ts) FROM interactions i
                                          WHERE i.task_id = t.id
                                            AND i.action = 'status_change'),
                                         t.updated_at) AS closed_at
                         FROM tasks t WHERE t.status_v2 = ?1)
                     WHERE julianday(closed_at) >= julianday({cutoff})
                     ORDER BY julianday(closed_at) DESC LIMIT 40"
                ))?;
                let rows = stmt
                    .query_map([status], |r| {
                        Ok((
                            r.get::<_, Option<String>>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                // The closing evidence, when the close carried a note: the
                // next session learns how the work was finished, not only
                // that it was.
                let mut out = Vec::with_capacity(rows.len());
                for (pt_id, title, id) in rows {
                    let mut v = serde_json::json!({ "pt_id": pt_id, "title": title });
                    if let Some(note) = crate::notes::closure_note_in_conn(c, &id)? {
                        v["note"] = serde_json::json!(crate::notes::preview(&note));
                    }
                    out.push(v);
                }
                Ok::<_, crate::Error>(out)
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
    // Work a dead agent left behind: leases that ran out, still in progress.
    // A session starting up sees them before it claims something new.
    let expired_claims = crate::claims::expired(db)?
        .iter()
        .map(|c| {
            serde_json::json!({
                "pt_id": c.pt_id, "title": c.title,
                "holder": c.holder, "expired_at": c.expired_at,
            })
        })
        .collect::<Vec<_>>();
    // Who opened and who closed work in the same window as created_count
    // (UTC midnight N days ago), not a rolling now−N×24h window.
    let since: String = db.with_conn(|c| {
        Ok(c.query_row(
            &format!("SELECT {cutoff} || 'T00:00:00.000+00:00'"),
            [],
            |r| r.get(0),
        )?)
    })?;
    let flux = crate::flux::by_actor_since(db, &since, days * 24 * 60)?;
    Ok(serde_json::json!({
        "window_days": days,
        "done": done, "dismissed": dismissed,
        "created_count": created,
        "ready_queue": ready,
        "expired_claims": expired_claims,
        "flux_by_actor": flux.actors,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::EventCtx;

    fn titles(v: &serde_json::Value, key: &str) -> Vec<String> {
        v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["title"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn recently_done_is_keyed_on_completion_not_last_touch() {
        // MCP-16: "done" filtered on updated_at, so linking an old done task
        // to a goal (which bumps updated_at) put it back in the digest.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("d.db")).unwrap();
        let ctx = EventCtx::test();
        let old =
            crate::tasks::create(&db, crate::NewTask::minimal("closed long ago"), &ctx).unwrap();
        let fresh =
            crate::tasks::create(&db, crate::NewTask::minimal("closed today"), &ctx).unwrap();
        crate::tasks::mark_done(&db, &old, &ctx).unwrap();
        crate::tasks::mark_done(&db, &fresh, &ctx).unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE interactions SET ts = strftime('%Y-%m-%dT%H:%M:%S', 'now', '-30 days') || '+00:00'
                 WHERE task_id = ?1",
                [&old.id],
            )?;
            c.execute(
                "UPDATE tasks SET updated_at = strftime('%Y-%m-%dT%H:%M:%S', 'now', '-30 days') || '+00:00'
                 WHERE id = ?1",
                [&old.id],
            )?;
            Ok(())
        })
        .unwrap();
        assert_eq!(titles(&build(&db, 7).unwrap(), "done"), ["closed today"]);

        let goal = crate::goals::add(&db, "a goal", None, None, &ctx).unwrap();
        crate::goals::link(&db, &old.id, &goal.g_id(), &ctx).unwrap();
        assert_eq!(
            titles(&build(&db, 7).unwrap(), "done"),
            ["closed today"],
            "a goal link is not a completion"
        );
        assert_eq!(titles(&build(&db, 60).unwrap(), "done").len(), 2);
    }

    #[test]
    fn recently_closed_tasks_carry_their_closing_note() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("d.db")).unwrap();
        let ctx = EventCtx::test();
        let a = crate::tasks::create(&db, crate::NewTask::minimal("with evidence"), &ctx).unwrap();
        let b = crate::tasks::create(&db, crate::NewTask::minimal("bare close"), &ctx).unwrap();
        let c = crate::tasks::create(&db, crate::NewTask::minimal("won't do"), &ctx).unwrap();
        crate::tasks::mark_done_noted(&db, &a, Some("CI green on abc123"), &ctx).unwrap();
        crate::tasks::mark_done(&db, &b, &ctx).unwrap();
        crate::tasks::dismiss_noted(&db, &c.id, Some("superseded by PT-9"), &ctx).unwrap();
        let v = build(&db, 7).unwrap();
        let note_of = |key: &str, title: &str| {
            v[key]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["title"] == title)
                .unwrap()
                .get("note")
                .cloned()
        };
        assert_eq!(
            note_of("done", "with evidence"),
            Some(serde_json::json!("CI green on abc123"))
        );
        assert_eq!(note_of("done", "bare close"), None);
        assert_eq!(
            note_of("dismissed", "won't do"),
            Some(serde_json::json!("superseded by PT-9"))
        );
    }
}
