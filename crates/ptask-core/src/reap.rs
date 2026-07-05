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

/// One task the reaper dismissed (or would dismiss, in dry-run).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Reaped {
    pub uuid: String,
    pub pt_id: Option<String>,
    pub title: String,
    pub source_type: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ReapReport {
    pub dry_run: bool,
    pub incident_ttl_days: i64,
    pub distilled_ttl_days: i64,
    pub reaped: Vec<Reaped>,
    pub errors: usize,
}

/// Idle TTL for incident-sourced tasks. With close-on-recovery wired
/// (v2.6.0 `/capture/resolve`), an incident task idle this long means no
/// re-capture bumped it and no resolve arrived — the condition is gone.
pub const INCIDENT_TTL_DAYS: i64 = 7;
/// Idle TTL for distilled (LLM-extracted) tasks at priority <= 3.
pub const DISTILLED_TTL_DAYS: i64 = 30;

/// One reap pass. `dry_run` lists candidates without dismissing.
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
                      AND updated_at < strftime('%Y-%m-%dT%H:%M:%f', 'now', ?1) || '+00:00')
                  OR (source_type = 'distilled'
                      AND priority <= 3
                      AND updated_at < strftime('%Y-%m-%dT%H:%M:%f', 'now', ?2) || '+00:00')
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

    let mut errors = 0usize;
    if !dry_run {
        for c in &candidates {
            let ctx = ctx.with_uuid(format!("reap:{}:{}", c.uuid, &c.updated_at));
            if let Err(e) = crate::tasks::dismiss(db, &c.uuid, &ctx) {
                tracing::warn!(target: "ptask::reap", uuid = %c.uuid, error = %e, "dismiss failed");
                errors += 1;
            }
        }
    }

    Ok(ReapReport {
        dry_run,
        incident_ttl_days: INCIDENT_TTL_DAYS,
        distilled_ttl_days: DISTILLED_TTL_DAYS,
        reaped: candidates,
        errors,
    })
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
}
