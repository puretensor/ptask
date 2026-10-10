//! Task dependency-graph queries.
//!
//! The one query here is "what's ready right now?": open tasks with no
//! `depends_on` edge (in `task_links`, since V010) to a prerequisite that is
//! still open. A done or dismissed prerequisite counts as satisfied. Edge
//! writes and the cycle check live in `tasks::add_dependency`.

use crate::error::Result;
use crate::storage::Db;
use crate::tasks::Task;

/// Return active tasks ready to start: every `depends_on` link resolves to
/// a done task, or the task has no dependency links. Snoozed tasks don't
/// compete. Order matches `tasks::list_with_filter` — severity first, with the
/// composite score breaking ties inside a band (see `crate::ordering`).
pub fn next_ready(db: &Db, limit: usize) -> Result<Vec<Task>> {
    let conn = db.get()?;

    // Active candidates (snoozed tasks deliberately don't compete) with an
    // unmet-dependency count from task_links (schema v2 replaced the JSON
    // depends_on blobs — which were empty for every task in prod anyway).
    let mut stmt = conn.prepare(&format!(
        "SELECT t.id, t.pt_id, t.title, t.description, t.priority, t.status_v2 AS status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning,
                t.kind, t.deliverable,
                (SELECT COUNT(*) FROM task_links l JOIN tasks d ON d.id = l.to_uuid
                 WHERE l.from_uuid = t.id AND l.kind = 'depends_on'
                   AND d.status_v2 NOT IN ('done','dismissed')) AS unmet
         FROM tasks t
         WHERE t.status_v2 IN ('triage','backlog','todo','in_progress')
         ORDER BY {}",
        crate::ordering::SortKey::default().sql()
    ))?;

    let mut out: Vec<Task> = Vec::new();
    let rows = stmt.query_map([], |r| {
        let unmet: i64 = r.get(13)?;
        let task = Task {
            id: r.get(0)?,
            pt_id: r.get(1)?,
            title: r.get(2)?,
            description: r.get(3).unwrap_or_default(),
            priority: r.get(4)?,
            status: r.get(5)?,
            created_at: r.get(6)?,
            updated_at: r.get(7)?,
            deadline: r.get(8)?,
            source_type: r.get(9)?,
            ai_reasoning: r.get(10).unwrap_or_default(),
            kind: r.get(11).unwrap_or_else(|_| "ship".to_string()),
            deliverable: r.get(12).unwrap_or_default(),
        };
        Ok((task, unmet))
    })?;

    for entry in rows {
        let (task, unmet) = entry?;
        if unmet == 0 {
            out.push(task);
            if out.len() >= limit {
                break;
            }
        }
    }

    Ok(out)
}

/// Close-and-continue, part one: the dependents of `closed_uuid` that are
/// ready now, i.e. open (triage/backlog/todo/in_progress) with no
/// prerequisite left open. Called after a close, it names the work that
/// close released, in the ready queue's order.
pub fn unblocked_by(db: &Db, closed_uuid: &str) -> Result<Vec<Task>> {
    let dependents: std::collections::HashSet<String> = {
        let conn = db.get()?;
        let mut stmt = conn.prepare(
            "SELECT from_uuid FROM task_links WHERE to_uuid = ?1 AND kind = 'depends_on'",
        )?;
        stmt.query_map([closed_uuid], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    if dependents.is_empty() {
        return Ok(Vec::new());
    }
    Ok(next_ready(db, usize::MAX)?
        .into_iter()
        .filter(|t| dependents.contains(&t.id))
        .collect())
}

/// Close-and-continue, part two: claim the next ready task for `ctx`'s
/// actor, in `pt next` order, skipping tasks already in progress and any
/// uuid in `skip` (tasks this same call just closed or advanced — a
/// recurring task that rolled forward is ready again and must not be
/// claimed back). The take is [`crate::claims::claim`]: holder, optional
/// `lease_minutes`, instance token. When another claimer wins a candidate
/// between the read and the claim, the next one is tried; `None` when
/// nothing ready is claimable. The returned task is re-read after the
/// claim so callers see it in progress.
pub fn claim_next(
    db: &Db,
    ctx: &crate::event_log::EventCtx,
    skip: &[String],
    lease_minutes: Option<i64>,
) -> Result<Option<(Task, crate::claims::Claim)>> {
    for t in next_ready(db, 50)? {
        if skip.iter().any(|id| id == &t.id) {
            continue;
        }
        if !matches!(t.status.as_str(), "triage" | "backlog" | "todo") {
            continue;
        }
        match crate::claims::claim(db, &t.id, lease_minutes, ctx) {
            Ok(claim) => {
                let claimed =
                    crate::tasks::resolve_for_lookup(db, &t.id, true).unwrap_or_else(|_| Task {
                        status: "in_progress".into(),
                        ..t
                    });
                return Ok(Some((claimed, claim)));
            }
            // Lost the race (or it moved on): the next candidate.
            Err(crate::Error::Other(msg)) if msg.contains("not claimable") => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::EventCtx;
    use crate::tasks::NewTask;

    fn fresh_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (
                    id               TEXT PRIMARY KEY,
                    title            TEXT NOT NULL,
                    description      TEXT DEFAULT '',
                    priority         INTEGER DEFAULT 2,
                    status           TEXT DEFAULT 'pending',
                    created_at       TEXT NOT NULL,
                    updated_at       TEXT NOT NULL,
                    deadline         TEXT,
                    source_type      TEXT DEFAULT 'manual',
                    source_files     TEXT DEFAULT '[]',
                    ai_confidence    REAL DEFAULT 1.0,
                    ai_reasoning     TEXT DEFAULT '',
                    depends_on       TEXT DEFAULT '[]',
                    blocks_tasks     TEXT DEFAULT '[]',
                    escalation_level INTEGER DEFAULT 0,
                    dismissal_count  INTEGER DEFAULT 0,
                    last_reminded    TEXT,
                    next_reminder    TEXT,
                    priority_score   REAL DEFAULT 0.0,
                    score_urgency    REAL DEFAULT 0.0,
                    score_dependency REAL DEFAULT 0.0,
                    score_neglect    REAL DEFAULT 0.0,
                    subtasks         TEXT DEFAULT '[]',
                    task_type        TEXT DEFAULT 'operational',
                    cluster_keywords TEXT DEFAULT '[]'
                );
                CREATE TABLE interactions (
                    id      INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    action  TEXT NOT NULL,
                    ts      TEXT NOT NULL,
                    details TEXT DEFAULT ''
                );",
            )
            .unwrap();
        }
        (dir, Db::open(&path).unwrap())
    }

    fn set_deps(db: &Db, task_uuid: &str, deps: &[String]) {
        for d in deps {
            crate::tasks::add_dependency(db, task_uuid, d, &EventCtx::test()).unwrap();
        }
    }

    #[test]
    fn task_with_no_deps_is_ready() {
        let (_dir, db) = fresh_db();
        crate::tasks::create(&db, NewTask::minimal("solo"), &EventCtx::test()).unwrap();
        let ready = next_ready(&db, 10).unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].title, "solo");
    }

    #[test]
    fn task_with_open_dep_is_not_ready() {
        let (_dir, db) = fresh_db();
        let a = crate::tasks::create(&db, NewTask::minimal("blocker"), &EventCtx::test()).unwrap();
        let b =
            crate::tasks::create(&db, NewTask::minimal("downstream"), &EventCtx::test()).unwrap();
        set_deps(&db, &b.id, std::slice::from_ref(&a.id));
        let ready = next_ready(&db, 10).unwrap();
        let titles: Vec<&str> = ready.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["blocker"]);
    }

    #[test]
    fn task_with_done_dep_becomes_ready() {
        let (_dir, db) = fresh_db();
        let a = crate::tasks::create(&db, NewTask::minimal("blocker"), &EventCtx::test()).unwrap();
        let b =
            crate::tasks::create(&db, NewTask::minimal("downstream"), &EventCtx::test()).unwrap();
        set_deps(&db, &b.id, std::slice::from_ref(&a.id));
        crate::tasks::mark_done(&db, &a, &EventCtx::test()).unwrap();
        let ready = next_ready(&db, 10).unwrap();
        let titles: Vec<&str> = ready.iter().map(|t| t.title.as_str()).collect();
        // Now only "downstream" remains pending and is ready.
        assert_eq!(titles, vec!["downstream"]);
    }

    #[test]
    fn ordering_score_breaks_ties_within_a_severity_band() {
        let (_dir, db) = fresh_db();
        let lo =
            crate::tasks::create(&db, NewTask::minimal("low score"), &EventCtx::test()).unwrap();
        let hi =
            crate::tasks::create(&db, NewTask::minimal("high score"), &EventCtx::test()).unwrap();
        db.with_conn(|c| {
            c.execute("UPDATE tasks SET priority_score=9.0 WHERE id=?1", [&hi.id])?;
            c.execute("UPDATE tasks SET priority_score=1.0 WHERE id=?1", [&lo.id])?;
            Ok(())
        })
        .unwrap();
        // Both tasks sit at the default priority, so the composite score is
        // the tiebreaker. Severity itself is asserted in
        // `tasks::tests::ready_tasks_rank_by_severity_too`.
        let ready = next_ready(&db, 10).unwrap();
        assert_eq!(ready[0].title, "high score");
    }

    #[test]
    fn ordering_matches_task_list_without_deadline_sort() {
        let (_dir, db) = fresh_db();
        let older_due =
            crate::tasks::create(&db, NewTask::minimal("older due"), &EventCtx::test()).unwrap();
        let newer_no_due =
            crate::tasks::create(&db, NewTask::minimal("newer no due"), &EventCtx::test()).unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks
                 SET created_at='2026-01-01T00:00:00+00:00',
                     deadline='2026-01-02'
                 WHERE id=?1",
                [&older_due.id],
            )?;
            c.execute(
                "UPDATE tasks
                 SET created_at='2026-01-03T00:00:00+00:00',
                     deadline=NULL
                 WHERE id=?1",
                [&newer_no_due.id],
            )?;
            Ok(())
        })
        .unwrap();

        let ready = next_ready(&db, 10).unwrap();
        let listed = crate::tasks::list_with_filter(&db, None, Some("pending"), None, 10).unwrap();
        assert_eq!(ready[0].title, "newer no due");
        assert_eq!(
            ready.iter().map(|t| &t.id).collect::<Vec<_>>(),
            listed.iter().map(|t| &t.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn limit_truncates_result() {
        let (_dir, db) = fresh_db();
        for i in 0..5 {
            crate::tasks::create(&db, NewTask::minimal(format!("t{}", i)), &EventCtx::test())
                .unwrap();
        }
        let ready = next_ready(&db, 3).unwrap();
        assert_eq!(ready.len(), 3);
    }

    #[test]
    fn dep_pointing_at_missing_id_is_ignored() {
        // depends_on referencing a UUID that no longer exists is treated as
        // satisfied (the row was likely deleted or the reference is stale).
        let (_dir, db) = fresh_db();
        let t =
            crate::tasks::create(&db, NewTask::minimal("orphan dep"), &EventCtx::test()).unwrap();
        // Raw insert: add_dependency validates both ends, but stale edges can
        // exist from legacy JSON backfill or a later hard delete.
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO task_links (from_uuid, to_uuid, kind, created_at)
                 VALUES (?1, 'nonexistent-uuid', 'depends_on', '2026-01-01T00:00:00+00:00')",
                [&t.id],
            )?;
            Ok(())
        })
        .unwrap();
        let ready = next_ready(&db, 10).unwrap();
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn a_close_names_what_it_unblocked_and_claim_next_takes_the_top() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("c.db")).unwrap();
        let ctx = EventCtx::test();
        let mk = |t: &str| crate::tasks::create(&db, NewTask::minimal(t), &ctx).unwrap();
        let base = mk("provision the VLAN");
        let other = mk("second prerequisite");
        let solo = mk("waits on base only");
        let both = mk("waits on base and other");
        crate::tasks::add_dependency(&db, &solo.id, &base.id, &ctx).unwrap();
        crate::tasks::add_dependency(&db, &both.id, &base.id, &ctx).unwrap();
        crate::tasks::add_dependency(&db, &both.id, &other.id, &ctx).unwrap();
        crate::tasks::update_priority(&db, &solo.id, 4, &ctx).unwrap();

        crate::tasks::mark_done(&db, &base, &ctx).unwrap();
        let freed: Vec<String> = unblocked_by(&db, &base.id)
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect();
        assert_eq!(freed, ["waits on base only"], "both still waits on other");
        assert!(
            unblocked_by(&db, &solo.id).unwrap().is_empty(),
            "no dependents"
        );

        let hal = EventCtx::local("hal");
        let none: &[String] = &[];
        let (got, claim) = claim_next(&db, &hal, none, None).unwrap().unwrap();
        assert_eq!(
            got.title, "waits on base only",
            "highest priority ready first"
        );
        assert_eq!(got.status, "in_progress", "returned after the claim");
        assert_eq!(claim.by, "hal");
        assert!(claim.expires_at.is_none());
        // In progress now: the next call skips it and takes the next ready.
        let (got, _) = claim_next(&db, &hal, none, None).unwrap().unwrap();
        assert_eq!(got.title, "second prerequisite");
        crate::tasks::mark_done(&db, &other, &ctx).unwrap();
        let (got, _) = claim_next(&db, &hal, none, None).unwrap().unwrap();
        assert_eq!(got.title, "waits on base and other");
        assert!(
            claim_next(&db, &hal, none, None).unwrap().is_none(),
            "nothing claimable left"
        );
    }

    #[test]
    fn claim_next_takes_an_owner_and_an_optional_lease() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("c.db")).unwrap();
        let ctx = EventCtx::test();
        crate::tasks::create(&db, NewTask::minimal("leased"), &ctx).unwrap();
        crate::tasks::create(&db, NewTask::minimal("owned"), &ctx).unwrap();
        let hal = EventCtx::local("hal");
        let none: &[String] = &[];
        let (got, claim) = claim_next(&db, &hal, none, Some(30)).unwrap().unwrap();
        assert_eq!(got.status, "in_progress");
        assert_eq!(claim.by, "hal");
        assert!(claim.expires_at.is_some() && !claim.token.is_empty());
        let stored = crate::claims::get(&db, &got.id).unwrap().unwrap();
        assert_eq!(stored.by, "hal");
        assert!(stored.expires_at.is_some());
        let (_, claim) = claim_next(&db, &hal, none, None).unwrap().unwrap();
        assert_eq!(claim.by, "hal");
        assert!(claim.expires_at.is_none());
    }

    #[test]
    fn claim_next_does_not_take_a_task_the_caller_asked_to_skip() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("c.db")).unwrap();
        let ctx = EventCtx::test();
        let first = crate::tasks::create(&db, NewTask::minimal("just advanced"), &ctx).unwrap();
        crate::tasks::update_priority(&db, &first.id, 4, &ctx).unwrap();
        crate::tasks::create(&db, NewTask::minimal("other ready"), &ctx).unwrap();
        let hal = EventCtx::local("hal");
        let (got, _) = claim_next(&db, &hal, std::slice::from_ref(&first.id), None)
            .unwrap()
            .unwrap();
        assert_eq!(got.title, "other ready");
        assert_eq!(got.status, "in_progress");
        let status: String = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT status_v2 FROM tasks WHERE id=?1",
                    [&first.id],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(status, "todo");
    }
}
