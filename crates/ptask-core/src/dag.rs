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

use crate::error::{Error, Result};
use crate::storage::Db;
use crate::tasks::Task;
use std::collections::HashMap;

/// Return pending tasks ready to start: all `depends_on` UUIDs resolve to
/// rows with `status='done'`, or `depends_on` is empty. Order is the same
/// as `tasks::list_with_filter` (priority_score DESC, priority DESC,
/// deadline ASC NULLS LAST, created_at DESC).
pub fn next_ready(db: &Db, limit: usize) -> Result<Vec<Task>> {
    let conn = db.get()?;

    // Build a map of id -> status for every task, in one shot. This lets us
    // resolve dependencies without a per-task lookup.
    let mut status_by_id: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, status FROM tasks")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, status) = row?;
            status_by_id.insert(id, status);
        }
    }

    // Pull pending candidates with their depends_on payload.
    let mut stmt = conn.prepare(
        "SELECT t.id, x.pt_id, t.title, t.description, t.priority, t.status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning,
                COALESCE(t.depends_on, '[]') AS deps
         FROM tasks t
         LEFT JOIN pt_extensions x ON x.task_uuid = t.id
         WHERE t.status = 'pending'
         ORDER BY t.priority_score DESC,
                  t.priority DESC,
                  CASE WHEN t.deadline IS NULL THEN 1 ELSE 0 END,
                  t.deadline ASC,
                  t.created_at DESC",
    )?;

    let mut out: Vec<Task> = Vec::new();
    let rows = stmt.query_map([], |r| {
        let deps_str: String = r.get(11)?;
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
        };
        Ok((task, deps_str))
    })?;

    for entry in rows {
        let (task, deps_str) = entry?;
        let deps: Vec<String> = serde_json::from_str(&deps_str)
            .map_err(|e| Error::Other(format!("parse depends_on for {}: {}", task.id, e)))?;
        let all_done = deps
            .iter()
            .all(|d| status_by_id.get(d).map(|s| s == "done").unwrap_or(true));
        if all_done {
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
    let mut status_by_id: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT id, status FROM tasks")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, status) = row?;
            status_by_id.insert(id, status);
        }
    }

    let mut stmt = conn.prepare(
        "SELECT t.id, x.pt_id, t.title, t.description, t.priority, t.status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning,
                COALESCE(t.depends_on, '[]') AS deps
         FROM tasks t
         LEFT JOIN pt_extensions x ON x.task_uuid = t.id
         WHERE t.status = 'pending'",
    )?;
    let mut out: Vec<(Task, Vec<String>)> = Vec::new();
    let rows = stmt.query_map([], |r| {
        let deps_str: String = r.get(11)?;
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
        };
        Ok((task, deps_str))
    })?;
    for entry in rows {
        let (task, deps_str) = entry?;
        let deps: Vec<String> = serde_json::from_str(&deps_str)
            .map_err(|e| Error::Other(format!("parse depends_on for {}: {}", task.id, e)))?;
        let missing: Vec<String> = deps
            .into_iter()
            .filter(|d| status_by_id.get(d).map(|s| s != "done").unwrap_or(false))
            .collect();
        if !missing.is_empty() {
            out.push((task, missing));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::NewTask;
    use rusqlite::params;

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

    fn set_deps(db: &Db, task_id: &str, deps: &[String]) {
        let json = serde_json::to_string(deps).unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET depends_on=?1 WHERE id=?2",
                params![json, task_id],
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn task_with_no_deps_is_ready() {
        let (_dir, db) = fresh_db();
        crate::tasks::create(&db, NewTask::minimal("solo")).unwrap();
        let ready = next_ready(&db, 10).unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].title, "solo");
    }

    #[test]
    fn task_with_open_dep_is_not_ready() {
        let (_dir, db) = fresh_db();
        let a = crate::tasks::create(&db, NewTask::minimal("blocker")).unwrap();
        let b = crate::tasks::create(&db, NewTask::minimal("downstream")).unwrap();
        set_deps(&db, &b.id, std::slice::from_ref(&a.id));
        let ready = next_ready(&db, 10).unwrap();
        let titles: Vec<&str> = ready.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["blocker"]);
    }

    #[test]
    fn task_with_done_dep_becomes_ready() {
        let (_dir, db) = fresh_db();
        let a = crate::tasks::create(&db, NewTask::minimal("blocker")).unwrap();
        let b = crate::tasks::create(&db, NewTask::minimal("downstream")).unwrap();
        set_deps(&db, &b.id, std::slice::from_ref(&a.id));
        crate::tasks::mark_done(&db, &a).unwrap();
        let ready = next_ready(&db, 10).unwrap();
        let titles: Vec<&str> = ready.iter().map(|t| t.title.as_str()).collect();
        // Now only "downstream" remains pending and is ready.
        assert_eq!(titles, vec!["downstream"]);
    }

    #[test]
    fn ordering_priority_score_first() {
        let (_dir, db) = fresh_db();
        let lo = crate::tasks::create(&db, NewTask::minimal("low score")).unwrap();
        let hi = crate::tasks::create(&db, NewTask::minimal("high score")).unwrap();
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
    fn limit_truncates_result() {
        let (_dir, db) = fresh_db();
        for i in 0..5 {
            crate::tasks::create(&db, NewTask::minimal(format!("t{}", i))).unwrap();
        }
        let ready = next_ready(&db, 3).unwrap();
        assert_eq!(ready.len(), 3);
    }

    #[test]
    fn dep_pointing_at_missing_id_is_ignored() {
        // depends_on referencing a UUID that no longer exists is treated as
        // satisfied (the row was likely deleted or the reference is stale).
        let (_dir, db) = fresh_db();
        let t = crate::tasks::create(&db, NewTask::minimal("orphan dep")).unwrap();
        set_deps(&db, &t.id, &["nonexistent-uuid".to_string()]);
        let ready = next_ready(&db, 10).unwrap();
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn pending_with_missing_deps_surfaces_blockers() {
        let (_dir, db) = fresh_db();
        let blocker = crate::tasks::create(&db, NewTask::minimal("blocker")).unwrap();
        let downstream = crate::tasks::create(&db, NewTask::minimal("downstream")).unwrap();
        set_deps(&db, &downstream.id, std::slice::from_ref(&blocker.id));
        let pending = pending_with_missing_deps(&db).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0.title, "downstream");
        assert_eq!(pending[0].1, vec![blocker.id]);
    }
}
