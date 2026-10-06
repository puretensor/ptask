//! One-shot v2 data converters that need Rust (PT-N minting, attributed
//! events) and therefore can't live in the SQL migrations.
//!
//! Invoked from `pt backfill`. Idempotent: guarded by a `pt_counters` flag.

use crate::error::Result;
use crate::event_log::EventCtx;
use crate::storage::Db;
use rusqlite::params;
use tracing::info;

const FLAG: &str = "v2_converted";

/// Promote the JSON `subtasks` blobs of NON-terminal parents into real child
/// task rows (own PT-N, `parent_uuid`, a `subtask_of` link, attributed
/// `task.created` events). Terminal parents keep their JSON as history —
/// promoting 2,700+ steps of already-done work would be archaeology, not
/// utility. Returns the number of children created; 0 on re-runs.
///
/// One IMMEDIATE transaction covers the flag check, every child and the
/// flag: a failure part-way rolls everything back, so a retry starts clean
/// instead of re-creating the children committed before the failure, and a
/// concurrent run waits and then sees the flag.
pub fn promote_subtasks_once(db: &Db) -> Result<usize> {
    let mut conn = db.get()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let done: i64 = tx.query_row(
        "SELECT COUNT(*) FROM pt_counters WHERE name = ?1",
        [FLAG],
        |r| r.get(0),
    )?;
    if done > 0 {
        return Ok(0);
    }

    let ctx = EventCtx::system("migration");
    let parents: Vec<(String, Option<String>, i64, String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT id, pt_id, priority, status_v2, COALESCE(subtasks,'[]')
             FROM tasks
             WHERE status_v2 NOT IN ('done','dismissed')
               AND json_valid(COALESCE(subtasks,'[]'))
               AND json_array_length(COALESCE(subtasks,'[]')) > 0",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    let mut created = 0usize;
    for (parent_uuid, parent_pt, priority, _parent_status, subtasks_json) in &parents {
        let items: Vec<String> = serde_json::from_str(subtasks_json).unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        for item in items {
            let title = item.trim();
            if title.is_empty() || !seen.insert(title.to_string()) {
                continue;
            }
            let child_uuid = uuid::Uuid::new_v4().to_string();
            let now = crate::dates::format_iso(&crate::dates::now_in_operator_tz()?);
            let n: i64 = tx.query_row(
                "UPDATE pt_counters SET value = value + 1 WHERE name='pt_id' RETURNING value",
                [],
                |r| r.get(0),
            )?;
            let child_pt = crate::pt_id::format_pt_id(n);
            tx.execute(
                "INSERT INTO tasks (
                    id, title, description, priority, status, status_v2,
                    created_at, updated_at, source_type, ai_reasoning,
                    pt_id, parent_uuid, created_by_pt
                 ) VALUES (?1, ?2, '', ?3, 'pending', 'todo', ?4, ?4,
                           'subtask_promotion', ?5, ?6, ?7, 1)",
                params![
                    child_uuid,
                    title,
                    priority,
                    now,
                    format!(
                        "promoted from {} subtasks at schema v2",
                        parent_pt.as_deref().unwrap_or("parent")
                    ),
                    child_pt,
                    parent_uuid,
                ],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO task_links (from_uuid, to_uuid, kind, created_at)
                 VALUES (?1, ?2, 'subtask_of', ?3)",
                params![child_uuid, parent_uuid, now],
            )?;
            crate::event_log::record_in_conn(
                &tx,
                &format!("v2-promote:{}", child_uuid),
                Some(&child_uuid),
                "task.created",
                &serde_json::json!({
                    "task_uuid": child_uuid,
                    "pt_id": child_pt,
                    "parent_uuid": parent_uuid,
                    "promoted_from_subtasks": true,
                }),
                &ctx,
            )?;
            created += 1;
        }
    }

    tx.execute(
        "INSERT OR IGNORE INTO pt_counters (name, value) VALUES (?1, 1)",
        [FLAG],
    )?;
    tx.commit()?;
    info!(
        target: "ptask::convert",
        parents = parents.len(),
        children = created,
        "subtask promotion complete"
    );
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{Extensions, NewTask, create_with_extensions};

    fn fresh_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        (dir, Db::open(&path).unwrap())
    }

    #[test]
    fn promotes_active_parents_only_and_is_idempotent() {
        let (_dir, db) = fresh_db();
        let ctx = EventCtx::test();
        let active =
            create_with_extensions(&db, NewTask::minimal("active"), Extensions::default(), &ctx)
                .unwrap();
        let done_t =
            create_with_extensions(&db, NewTask::minimal("done"), Extensions::default(), &ctx)
                .unwrap();
        crate::tasks::mark_done(&db, &done_t, &ctx).unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET subtasks='[\"step one\",\"step two\",\"step one\"]' WHERE id=?1",
                [&active.id],
            )?;
            c.execute(
                "UPDATE tasks SET subtasks='[\"old step\"]' WHERE id=?1",
                [&done_t.id],
            )?;
            Ok(())
        })
        .unwrap();

        let created = promote_subtasks_once(&db).unwrap();
        assert_eq!(created, 2, "two unique steps from the active parent only");

        db.with_conn(|c| {
            let kids: i64 = c.query_row(
                "SELECT COUNT(*) FROM tasks WHERE parent_uuid = ?1",
                [&active.id],
                |r| r.get(0),
            )?;
            assert_eq!(kids, 2);
            let links: i64 = c.query_row(
                "SELECT COUNT(*) FROM task_links WHERE to_uuid = ?1 AND kind='subtask_of'",
                [&active.id],
                |r| r.get(0),
            )?;
            assert_eq!(links, 2);
            let done_kids: i64 = c.query_row(
                "SELECT COUNT(*) FROM tasks WHERE parent_uuid = ?1",
                [&done_t.id],
                |r| r.get(0),
            )?;
            assert_eq!(done_kids, 0, "terminal parents keep JSON history");
            Ok(())
        })
        .unwrap();

        assert_eq!(promote_subtasks_once(&db).unwrap(), 0, "idempotent");
    }

    #[test]
    fn promotion_failing_midway_is_retried_without_duplicate_children() {
        // CORE-10: each child committed in its own transaction and the
        // done-flag was only set at the end, so a failure after the first
        // child left it in place and the retry created it again.
        let (_dir, db) = fresh_db();
        let ctx = EventCtx::test();
        let parent =
            create_with_extensions(&db, NewTask::minimal("parent"), Extensions::default(), &ctx)
                .unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET subtasks='[\"step one\",\"step two\"]' WHERE id=?1",
                [&parent.id],
            )?;
            c.execute_batch(
                "CREATE TRIGGER boom BEFORE INSERT ON tasks WHEN NEW.title = 'step two'
                 BEGIN SELECT RAISE(ABORT, 'boom'); END;",
            )?;
            Ok(())
        })
        .unwrap();
        assert!(promote_subtasks_once(&db).is_err());
        db.with_conn(|c| Ok(c.execute_batch("DROP TRIGGER boom")?))
            .unwrap();

        assert_eq!(promote_subtasks_once(&db).unwrap(), 2);
        let titles: Vec<String> = db
            .with_conn(|c| {
                let mut stmt =
                    c.prepare("SELECT title FROM tasks WHERE parent_uuid = ?1 ORDER BY title")?;
                let rows = stmt.query_map([&parent.id], |r| r.get(0))?;
                Ok(rows.collect::<std::result::Result<_, _>>()?)
            })
            .unwrap();
        assert_eq!(titles, ["step one", "step two"]);
        assert_eq!(promote_subtasks_once(&db).unwrap(), 0, "idempotent");
    }
}
