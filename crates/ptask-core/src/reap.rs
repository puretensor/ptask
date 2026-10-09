//! Staleness reaper (v2.6.0) — bounded, reversible garbage collection for
//! MACHINE-GENERATED tasks only.
//!
//! Scoring v2 makes stale tasks louder (urgency and neglect both grow with
//! age), so an un-closed machine capture climbs the ranking forever. The
//! reaper bounds that: machine-sourced tasks that have sat untouched past
//! their class TTL are dismissed (soft close — `pt reopen` reverses, and
//! every action lands in `pt_event_log` with actor attribution).
//!
//! Hard exclusions, by policy (Quiet Cockpit program):
//!   - anything human-authored (only `incident` and `distilled` source
//!     types are ever touched)
//!   - sev>=4 incidents (priority 5)
//!   - anything claimed / in progress / blocked / snoozed (status_v2 gate)
//!   - anything already in triage review (`triage_reason` set)

use crate::event_log::EventCtx;
use crate::{Db, Result};
use rusqlite::OptionalExtension;

/// One task the reaper dismissed (or would dismiss, in dry-run).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Reaped {
    pub uuid: String,
    pub pt_id: Option<String>,
    pub title: String,
    pub source_type: String,
    pub updated_at: String,
}

/// A candidate whose dismiss failed, with the reason.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReapFailure {
    #[serde(flatten)]
    pub task: Reaped,
    pub error: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ReapReport {
    pub dry_run: bool,
    pub incident_ttl_days: i64,
    pub distilled_ttl_days: i64,
    /// Dismissed (or, in dry-run, would be dismissed).
    pub reaped: Vec<Reaped>,
    /// Candidates whose dismiss failed; `errors == failed.len()`.
    pub failed: Vec<ReapFailure>,
    pub errors: usize,
}

/// Idle TTL for incident-sourced tasks. With close-on-recovery wired
/// (v2.6.0 `/capture/resolve`), an incident task idle this long means no
/// re-capture bumped it and no resolve arrived — the condition is gone.
pub const INCIDENT_TTL_DAYS: i64 = 7;
/// Idle TTL for distilled (LLM-extracted) tasks at priority <= 3.
pub const DISTILLED_TTL_DAYS: i64 = 30;

/// One reap pass. `dry_run` lists candidates without dismissing.
///
/// The `julianday(updated_at)` guards below stay fail-*closed* on purpose:
/// unlike the read paths, reaping auto-dismisses, so an unparseable timestamp
/// must exclude a task from reaping rather than silently discard it.
pub fn run(db: &Db, dry_run: bool, ctx: &EventCtx) -> Result<ReapReport> {
    let candidates: Vec<Reaped> = {
        let conn = db.get()?;
        let mut stmt = conn.prepare(
            "SELECT id, pt_id, title, source_type, updated_at FROM tasks
             WHERE status_v2 IN ('triage','backlog','todo')
               AND triage_reason IS NULL
               AND (
                     (source_type = 'incident'
                      AND priority <= 4
                      AND julianday(updated_at) < julianday('now', ?1))
                  OR (source_type = 'distilled'
                      AND priority <= 3
                      AND julianday(updated_at) < julianday('now', ?2))
               )
             ORDER BY updated_at ASC",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                format!("-{} days", INCIDENT_TTL_DAYS),
                format!("-{} days", DISTILLED_TTL_DAYS)
            ],
            |r| {
                Ok(Reaped {
                    uuid: r.get(0)?,
                    pt_id: r.get(1)?,
                    title: r.get(2)?,
                    source_type: r.get(3)?,
                    updated_at: r.get(4)?,
                })
            },
        )?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    let mut failed = Vec::new();
    let reaped = if dry_run {
        candidates
    } else {
        let mut reaped = Vec::with_capacity(candidates.len());
        for c in candidates {
            match reap_one(db, &c, ctx) {
                Ok(true) => reaped.push(c),
                Ok(false) => tracing::info!(
                    target: "ptask::reap", uuid = %c.uuid,
                    "changed since the candidate scan; not reaped"
                ),
                Err(e) => {
                    tracing::warn!(target: "ptask::reap", uuid = %c.uuid, error = %e, "dismiss failed");
                    failed.push(ReapFailure {
                        task: c,
                        error: e.to_string(),
                    });
                }
            }
        }
        reaped
    };

    Ok(ReapReport {
        dry_run,
        incident_ttl_days: INCIDENT_TTL_DAYS,
        distilled_ttl_days: DISTILLED_TTL_DAYS,
        reaped,
        errors: failed.len(),
        failed,
    })
}

/// Dismiss one candidate from the scan, re-checking the reap rule under the
/// write lock: still untriaged in triage/backlog/todo, and untouched since
/// the scan (`updated_at` as read). Between the scan and here a task can be
/// claimed, started, completed, snoozed, triaged or refreshed; a generic
/// dismiss would discard that (a completion turned into "dismissed").
/// Returns false, writing nothing, when the task no longer qualifies.
fn reap_one(db: &Db, c: &Reaped, ctx: &EventCtx) -> Result<bool> {
    let ctx = ctx.with_uuid(format!("reap:{}:{}", c.uuid, c.updated_at));
    let mut conn = db.get()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let status: Option<String> = tx
        .query_row(
            "SELECT status FROM tasks
             WHERE id = ?1 AND updated_at = ?2
               AND status_v2 IN ('triage','backlog','todo')
               AND triage_reason IS NULL",
            rusqlite::params![c.uuid, c.updated_at],
            |r| r.get(0),
        )
        .optional()?;
    let Some(status) = status else {
        return Ok(false);
    };
    // The reason is the evidence: a reaped task says why it went.
    let ttl = if c.source_type == "incident" {
        INCIDENT_TTL_DAYS
    } else {
        DISTILLED_TTL_DAYS
    };
    let note = format!(
        "reaped: {} task untouched for over {ttl} days (last touched {}); `pt reopen` restores it",
        c.source_type, c.updated_at
    );
    crate::tasks::dismiss_in_tx(&tx, &c.uuid, &status, Some(&note), &ctx)?;
    tx.commit()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Extensions, NewTask};

    fn mk(db: &Db, title: &str, source: &str, priority: i64) -> crate::Task {
        let new = NewTask {
            title: title.into(),
            description: String::new(),
            priority,
            deadline: None,
            source_type: source.into(),
            ai_confidence: 1.0,
            ai_reasoning: String::new(),
        };
        crate::tasks::create_with_extensions(db, new, Extensions::default(), &EventCtx::test())
            .unwrap()
    }

    fn age(db: &Db, uuid: &str, days: i64) {
        let conn = db.get().unwrap();
        conn.execute(
            "UPDATE tasks SET updated_at = strftime('%Y-%m-%dT%H:%M:%f','now', ?1) || '+00:00' WHERE id = ?2",
            rusqlite::params![format!("-{} days", days), uuid],
        )
        .unwrap();
    }

    #[test]
    fn reaps_stale_machine_tasks_only() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("t.db")).unwrap();

        let stale_incident = mk(&db, "old incident", "incident", 4);
        age(&db, &stale_incident.id, 10);
        let fresh_incident = mk(&db, "fresh incident", "incident", 4);
        let sev4_incident = mk(&db, "critical incident", "incident", 5);
        age(&db, &sev4_incident.id, 10);
        let stale_distilled = mk(&db, "old distilled", "distilled", 3);
        age(&db, &stale_distilled.id, 40);
        let young_distilled = mk(&db, "recent distilled", "distilled", 3);
        age(&db, &young_distilled.id, 10);
        let stale_human = mk(&db, "old manual task", "manual", 3);
        age(&db, &stale_human.id, 90);

        // Dry run: two candidates, nothing dismissed.
        let dry = run(&db, true, &EventCtx::test()).unwrap();
        let ids: Vec<&str> = dry.reaped.iter().map(|r| r.uuid.as_str()).collect();
        assert_eq!(dry.reaped.len(), 2, "{:?}", dry.reaped);
        assert!(ids.contains(&stale_incident.id.as_str()));
        assert!(ids.contains(&stale_distilled.id.as_str()));
        assert!(!ids.contains(&fresh_incident.id.as_str()));
        let conn = db.get().unwrap();
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE status_v2 NOT IN ('done','dismissed')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 6);

        // Real run dismisses exactly those two.
        let real = run(&db, false, &EventCtx::test()).unwrap();
        assert_eq!(real.reaped.len(), 2);
        assert_eq!(real.errors, 0);
        let dismissed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE status_v2 = 'dismissed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dismissed, 2);
        // Each reaped task says why it went.
        let notes = crate::notes::list(&db, &stale_distilled.id, 10).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].kind, "dismissed");
        assert!(
            notes[0]
                .text
                .starts_with("reaped: distilled task untouched for over 30 days"),
            "{}",
            notes[0].text
        );
        // Reversible: reopen brings one back.
        crate::tasks::reopen(&db, &stale_incident.id, &EventCtx::test()).unwrap();
        let back: String = conn
            .query_row(
                "SELECT status_v2 FROM tasks WHERE id = ?1",
                [&stale_incident.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(back, "dismissed");
    }

    #[test]
    fn snoozed_and_in_progress_are_untouchable() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("t.db")).unwrap();
        let t = mk(&db, "claimed incident", "incident", 4);
        age(&db, &t.id, 30);
        crate::tasks::start(&db, &t.id, &EventCtx::test()).unwrap();
        // start() bumps updated_at; re-age to prove the status gate holds.
        age(&db, &t.id, 30);
        let dry = run(&db, true, &EventCtx::test()).unwrap();
        assert!(dry.reaped.is_empty(), "{:?}", dry.reaped);
    }

    #[test]
    fn a_task_touched_after_the_snapshot_is_not_reaped() {
        // Regression (CORE-5): candidates come from an autocommit SELECT and
        // the generic dismiss only refused an already-dismissed task, so a
        // task claimed, started, completed, snoozed, triaged or refreshed in
        // between was dismissed anyway (a completion lost to "dismissed").
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("t.db")).unwrap();
        let ctx = EventCtx::test();
        let mut cases = Vec::new();
        for touch in ["start", "done", "snooze", "triage", "refresh"] {
            let t = mk(&db, &format!("stale incident ({touch})"), "incident", 4);
            age(&db, &t.id, 30);
            cases.push((touch, t));
        }
        let snapshot = run(&db, true, &ctx).unwrap().reaped;
        assert_eq!(snapshot.len(), cases.len());

        for (touch, t) in &cases {
            match *touch {
                "start" => {
                    crate::tasks::start(&db, &t.id, &ctx).unwrap();
                }
                "done" => {
                    crate::tasks::mark_done(&db, t, &ctx).unwrap();
                }
                "snooze" => crate::tasks::snooze(&db, &t.id, "2099-01-01", &ctx).unwrap(),
                "triage" => {
                    db.get()
                        .unwrap()
                        .execute(
                            "UPDATE tasks SET triage_reason='needs a human' WHERE id=?1",
                            [&t.id],
                        )
                        .unwrap();
                }
                "refresh" => crate::tasks::update_priority(&db, &t.id, 3, &ctx).unwrap(),
                _ => unreachable!(),
            }
        }
        for c in &snapshot {
            assert!(!reap_one(&db, c, &ctx).unwrap(), "{} was reaped", c.title);
        }
        let conn = db.get().unwrap();
        for (touch, t) in &cases {
            let status: String = conn
                .query_row("SELECT status_v2 FROM tasks WHERE id=?1", [&t.id], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_ne!(status, "dismissed", "{touch}");
        }
    }

    #[test]
    fn reaper_compares_mixed_offset_updated_at_as_instants() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("t.db")).unwrap();
        let actually_fresh = mk(&db, "fresh across offset", "incident", 4);
        let actually_stale = mk(&db, "stale across offset", "incident", 4);
        db.with_conn(|c| {
            // Incident cutoff is now-7d. This is cutoff+1h represented at
            // -05:00; its wall-clock hour sorts before the UTC cutoff.
            c.execute(
                "UPDATE tasks SET updated_at =
                    strftime('%Y-%m-%dT%H:%M:%f','now','-7 days','-4 hours') || '-05:00'
                  WHERE id=?1",
                [&actually_fresh.id],
            )?;
            // This is cutoff-1h represented at +05:00; its wall-clock hour
            // sorts after the UTC cutoff.
            c.execute(
                "UPDATE tasks SET updated_at =
                    strftime('%Y-%m-%dT%H:%M:%f','now','-7 days','+4 hours') || '+05:00'
                  WHERE id=?1",
                [&actually_stale.id],
            )?;
            Ok(())
        })
        .unwrap();

        let report = run(&db, true, &EventCtx::test()).unwrap();
        let ids: Vec<&str> = report.reaped.iter().map(|r| r.uuid.as_str()).collect();
        assert!(!ids.contains(&actually_fresh.id.as_str()));
        assert!(ids.contains(&actually_stale.id.as_str()));
    }
}
