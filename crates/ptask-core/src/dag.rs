//! Task dependency-graph queries.
//!
//! Phase-2 implementation focuses on the single killer query — "what's
//! ready right now?" — i.e. pending tasks whose every dependency has
//! `status='done'` (or which have no deps at all). Cycle detection,
//! critical-path scoring, and downstream-fanout analysis are deferred to
//! later phases; the `petgraph` workspace dep is in place for those.
//!
//! Schema reference: the existing Python `tasks` table stores deps as a
//! JSON array of UUIDs in `depends_on` (default `'[]'`). Reverse edges
//! live in `blocks_tasks`. We only read `depends_on` here — `blocks_tasks`
//! is the operator-facing "who's blocked by me" view, not the prerequisite
//! relation.

use crate::error::Result;
use crate::storage::Db;
use crate::tasks::Task;

/// Return active tasks ready to start: every `depends_on` link resolves to
/// a done task, or the task has no dependency links. Snoozed tasks don't
/// compete. Order matches `tasks::list_with_filter` (priority_score DESC,
/// priority DESC, created_at DESC).
pub fn next_ready(db: &Db, limit: usize) -> Result<Vec<Task>> {
    let conn = db.get()?;

    // Active candidates (snoozed tasks deliberately don't compete) with an
    // unmet-dependency count from task_links (schema v2 replaced the JSON
    // depends_on blobs — which were empty for every task in prod anyway).
    let mut stmt = conn.prepare(
        "SELECT t.id, t.pt_id, t.title, t.description, t.priority, t.status_v2 AS status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning,
                t.kind, t.deliverable,
                (SELECT COUNT(*) FROM task_links l JOIN tasks d ON d.id = l.to_uuid
                 WHERE l.from_uuid = t.id AND l.kind = 'depends_on'
                   AND d.status_v2 != 'done') AS unmet
         FROM tasks t
         WHERE t.status_v2 IN ('triage','backlog','todo','in_progress')
         ORDER BY t.priority_score DESC,
                  t.priority DESC,
                  t.created_at DESC",
    )?;

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

/// Diagnostic: list `(task, missing_deps[])` for pending tasks whose deps
/// are unmet. Useful for `pt next --explain` (deferred).
#[allow(dead_code)]
pub fn pending_with_missing_deps(db: &Db) -> Result<Vec<(Task, Vec<String>)>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT t.id, t.pt_id, t.title, t.description, t.priority, t.status_v2 AS status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning,
                t.kind, t.deliverable
         FROM tasks t
         WHERE t.status_v2 IN ('triage','backlog','todo','in_progress')
           AND EXISTS (SELECT 1 FROM task_links l JOIN tasks d ON d.id = l.to_uuid
                       WHERE l.from_uuid = t.id AND l.kind = 'depends_on'
                         AND d.status_v2 != 'done')",
    )?;
    let tasks: Vec<Task> = stmt
        .query_map([], |r| {
            Ok(Task {
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
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    drop(stmt);

    let mut out = Vec::new();
    for task in tasks {
        let mut miss = conn.prepare(
            "SELECT l.to_uuid FROM task_links l JOIN tasks d ON d.id = l.to_uuid
             WHERE l.from_uuid = ?1 AND l.kind = 'depends_on' AND d.status_v2 != 'done'",
        )?;
        let missing: Vec<String> = miss
            .query_map([&task.id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        out.push((task, missing));
    }
    Ok(out)
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
    fn ordering_priority_score_first() {
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
    fn pending_with_missing_deps_surfaces_blockers() {
        let (_dir, db) = fresh_db();
        let blocker =
            crate::tasks::create(&db, NewTask::minimal("blocker"), &EventCtx::test()).unwrap();
        let downstream =
            crate::tasks::create(&db, NewTask::minimal("downstream"), &EventCtx::test()).unwrap();
        set_deps(&db, &downstream.id, std::slice::from_ref(&blocker.id));
        let pending = pending_with_missing_deps(&db).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0.title, "downstream");
        assert_eq!(pending[0].1, vec![blocker.id]);
    }
}
