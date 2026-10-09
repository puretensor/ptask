//! Acceptance criteria: a task's definition of done, checked item by item.
//!
//! The best-documented agent failure is calling work done before it is:
//! premature completion without the testing the task needed. A criterion
//! is one checkable condition ("tests pass on CI", "deployed to Fox-n0 and
//! read back"). Checking one records who checked it and, optionally, the
//! evidence. **A task with an unchecked criterion cannot be closed**, the
//! way a task with an open prerequisite cannot: `pt done`, MCP `task_done`,
//! `/sync`, the cockpit, git closes and recovery closes all refuse it with
//! the list. Tasks without criteria are unaffected, so nothing changes for
//! existing work: a task opts in by being given criteria.
//!
//! Criteria are journal events, not a table: `task.criterion_added`,
//! `task.criterion_checked`, `task.criterion_unchecked`,
//! `task.criterion_removed`, and `task.criteria_reset` when a recurring
//! task advances or the task is reopened (each occurrence / reopening has
//! to meet them again). The state is folded from the task's events, so
//! every change is attributed and the trail is the record.

use crate::error::{Error, Result};
use crate::event_log::EventCtx;
use crate::storage::Db;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Longest criterion text, in characters.
pub const MAX_CRITERION_CHARS: usize = 500;
/// Longest evidence on a check, in characters (the note cap).
pub const MAX_EVIDENCE_CHARS: usize = 16 * 1024;
/// Most criteria one task may carry.
pub const MAX_CRITERIA: usize = 50;

/// One criterion and its state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Criterion {
    /// Stable 1-based number, never reused on the task.
    pub n: i64,
    pub text: String,
    pub done: bool,
    pub checked_by: Option<String>,
    pub checked_at: Option<String>,
    pub evidence: Option<String>,
}

/// SQL `IN (...)` list of criteria journal events. Undo treats these as
/// transparent (not a target, not a later change), and the cockpit drawer
/// always includes them when folding the checklist.
pub(crate) const EVENTS: &str = "'task.criterion_added', 'task.criterion_checked', \
     'task.criterion_unchecked', 'task.criterion_removed', 'task.criteria_reset'";

/// The task's criteria, in number order (folded from its journal).
pub fn list(db: &Db, task_uuid: &str) -> Result<Vec<Criterion>> {
    let conn = db.get()?;
    list_in_conn(&conn, task_uuid)
}

/// [`list`] on an existing connection (a transaction derefs to one).
pub fn list_in_conn(conn: &rusqlite::Connection, task_uuid: &str) -> Result<Vec<Criterion>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT event_type, payload, ts, actor FROM pt_event_log
          WHERE task_uuid = ?1 AND event_type IN ({EVENTS}) AND json_valid(payload)
          ORDER BY id"
    ))?;
    let rows = stmt
        .query_map([task_uuid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut out: Vec<Criterion> = Vec::new();
    for (kind, payload, ts, actor) in rows {
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
        let n = v.get("n").and_then(|n| n.as_i64());
        let find = |out: &mut Vec<Criterion>, n: Option<i64>| -> Option<usize> {
            n.and_then(|n| out.iter().position(|c| c.n == n))
        };
        match kind.as_str() {
            "task.criterion_added" => {
                if let (Some(n), Some(text)) = (n, v.get("text").and_then(|t| t.as_str())) {
                    out.push(Criterion {
                        n,
                        text: text.to_string(),
                        done: false,
                        checked_by: None,
                        checked_at: None,
                        evidence: None,
                    });
                }
            }
            "task.criterion_checked" => {
                if let Some(i) = find(&mut out, n) {
                    let c = &mut out[i];
                    c.done = true;
                    c.checked_by = actor;
                    c.checked_at = Some(ts);
                    c.evidence = v
                        .get("evidence")
                        .and_then(|e| e.as_str())
                        .map(str::to_string);
                }
            }
            "task.criterion_unchecked" => {
                if let Some(i) = find(&mut out, n) {
                    let c = &mut out[i];
                    c.done = false;
                    c.checked_by = None;
                    c.checked_at = None;
                    c.evidence = None;
                }
            }
            "task.criterion_removed" => {
                if let Some(i) = find(&mut out, n) {
                    out.remove(i);
                }
            }
            "task.criteria_reset" => {
                for c in &mut out {
                    c.done = false;
                    c.checked_by = None;
                    c.checked_at = None;
                    c.evidence = None;
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// The criteria still unchecked, for the close gate.
pub fn unchecked_in_conn(conn: &rusqlite::Connection, task_uuid: &str) -> Result<Vec<Criterion>> {
    Ok(list_in_conn(conn, task_uuid)?
        .into_iter()
        .filter(|c| !c.done)
        .collect())
}

/// The refusal a close gets while criteria are unchecked (an
/// [`Error::Blocked`], so every surface reports it like open prerequisites).
pub fn blocked_error(handle: &str, open: &[Criterion]) -> Error {
    let items: Vec<String> = open
        .iter()
        .map(|c| format!("{}. {}", c.n, crate::text::one_line(&c.text)))
        .collect();
    Error::Blocked(format!(
        "{handle} has unchecked acceptance criteria: {} — check them (`pt criteria check {handle} N`) or remove them first",
        items.join("; ")
    ))
}

fn validate_text(text: &str) -> Result<String> {
    let t = text.trim();
    if t.is_empty() {
        return Err(Error::Other("criterion is empty".into()));
    }
    if t.chars().count() > MAX_CRITERION_CHARS {
        return Err(Error::Other(format!(
            "criterion exceeds {MAX_CRITERION_CHARS} characters"
        )));
    }
    Ok(t.to_string())
}

fn validate_evidence(evidence: Option<&str>) -> Result<Option<&str>> {
    let evidence = evidence.map(str::trim).filter(|e| !e.is_empty());
    if let Some(e) = evidence
        && e.chars().count() > MAX_EVIDENCE_CHARS
    {
        return Err(Error::Other(format!(
            "evidence exceeds {MAX_EVIDENCE_CHARS} characters"
        )));
    }
    Ok(evidence)
}

fn unique_keep_order(ns: &[i64]) -> Vec<i64> {
    let mut seen = std::collections::HashSet::new();
    ns.iter().copied().filter(|n| seen.insert(*n)).collect()
}

/// Validate a batch of new criteria (e.g. for a create), all or nothing.
pub fn validate_all(texts: &[String]) -> Result<Vec<String>> {
    if texts.len() > MAX_CRITERIA {
        return Err(Error::Other(format!("at most {MAX_CRITERIA} criteria")));
    }
    texts.iter().map(|t| validate_text(t)).collect()
}

fn event_uuid(ctx: &EventCtx, part: &str) -> String {
    match ctx.event_uuid.as_deref() {
        Some(key) if part.is_empty() => key.to_string(),
        Some(key) => format!("{key}:{part}"),
        None => crate::tasks::local_event_uuid(),
    }
}

/// Add criteria inside the caller's transaction (the create path uses it
/// so a task and its definition of done commit together). Returns them.
pub fn add_in_conn(
    conn: &rusqlite::Connection,
    task_uuid: &str,
    texts: &[String],
    ctx: &EventCtx,
) -> Result<Vec<Criterion>> {
    let texts = validate_all(texts)?;
    let existing = list_in_conn(conn, task_uuid)?;
    if existing.len() + texts.len() > MAX_CRITERIA {
        return Err(Error::Other(format!("at most {MAX_CRITERIA} criteria")));
    }
    // Numbers are never reused, even after a removal.
    let mut next: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(CAST(json_extract(payload, '$.n') AS INTEGER)), 0)
               FROM pt_event_log
              WHERE task_uuid = ?1 AND event_type = 'task.criterion_added' AND json_valid(payload)",
            [task_uuid],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let mut added = Vec::new();
    for (i, text) in texts.into_iter().enumerate() {
        next += 1;
        crate::event_log::record_in_conn(
            conn,
            &event_uuid(ctx, &format!("ac{i}")),
            Some(task_uuid),
            "task.criterion_added",
            &serde_json::json!({ "task_uuid": task_uuid, "n": next, "text": text }),
            ctx,
        )?;
        added.push(Criterion {
            n: next,
            text,
            done: false,
            checked_by: None,
            checked_at: None,
            evidence: None,
        });
    }
    Ok(added)
}

fn with_task<T>(
    db: &Db,
    task_uuid: &str,
    f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
) -> Result<T> {
    let mut conn = db.get()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let exists: Option<String> = tx
        .query_row("SELECT id FROM tasks WHERE id = ?1", [task_uuid], |r| {
            r.get(0)
        })
        .optional()?;
    if exists.is_none() {
        return Err(Error::Other("task not found".into()));
    }
    let out = f(&tx)?;
    tx.execute(
        "UPDATE tasks SET updated_at = ?1 WHERE id = ?2",
        params![crate::tasks::iso_now(), task_uuid],
    )?;
    tx.commit()?;
    Ok(out)
}

/// Add criteria to an existing task.
pub fn add(db: &Db, task_uuid: &str, texts: &[String], ctx: &EventCtx) -> Result<Vec<Criterion>> {
    with_task(db, task_uuid, |tx| add_in_conn(tx, task_uuid, texts, ctx))
}

fn current(conn: &rusqlite::Connection, task_uuid: &str, n: i64) -> Result<Criterion> {
    list_in_conn(conn, task_uuid)?
        .into_iter()
        .find(|c| c.n == n)
        .ok_or_else(|| Error::Other(format!("no criterion {n} on this task")))
}

fn check_in_conn(
    conn: &rusqlite::Connection,
    task_uuid: &str,
    n: i64,
    evidence: Option<&str>,
    ctx: &EventCtx,
    uuid_part: &str,
) -> Result<Criterion> {
    let c = current(conn, task_uuid, n)?;
    if c.done {
        return Err(Error::Other(format!("criterion {n} is already checked")));
    }
    let mut payload = serde_json::json!({ "task_uuid": task_uuid, "n": n });
    if let Some(e) = evidence {
        payload["evidence"] = serde_json::json!(e);
    }
    crate::event_log::record_in_conn(
        conn,
        &event_uuid(ctx, uuid_part),
        Some(task_uuid),
        "task.criterion_checked",
        &payload,
        ctx,
    )?;
    current(conn, task_uuid, n)
}

fn uncheck_in_conn(
    conn: &rusqlite::Connection,
    task_uuid: &str,
    n: i64,
    ctx: &EventCtx,
    uuid_part: &str,
) -> Result<Criterion> {
    let c = current(conn, task_uuid, n)?;
    if !c.done {
        return Err(Error::Other(format!("criterion {n} is not checked")));
    }
    crate::event_log::record_in_conn(
        conn,
        &event_uuid(ctx, uuid_part),
        Some(task_uuid),
        "task.criterion_unchecked",
        &serde_json::json!({ "task_uuid": task_uuid, "n": n }),
        ctx,
    )?;
    current(conn, task_uuid, n)
}

/// Check criterion `n`, optionally with evidence (what shows it holds).
pub fn check(
    db: &Db,
    task_uuid: &str,
    n: i64,
    evidence: Option<&str>,
    ctx: &EventCtx,
) -> Result<Criterion> {
    let evidence = validate_evidence(evidence)?;
    with_task(db, task_uuid, |tx| {
        check_in_conn(tx, task_uuid, n, evidence, ctx, "")
    })
}

/// Uncheck criterion `n` (it no longer holds, or was checked by mistake).
pub fn uncheck(db: &Db, task_uuid: &str, n: i64, ctx: &EventCtx) -> Result<Criterion> {
    with_task(db, task_uuid, |tx| {
        uncheck_in_conn(tx, task_uuid, n, ctx, "")
    })
}

/// Add, check and uncheck in one transaction. An invalid batch changes
/// nothing: evidence length, duplicate numbers (treated as one), missing
/// numbers and already-checked/not-checked are all validated before any
/// event is written. `check`/`uncheck` numbers refer to the task as it
/// stood before the adds in this batch.
pub fn apply_batch(
    db: &Db,
    task_uuid: &str,
    add: &[String],
    check: &[i64],
    uncheck: &[i64],
    evidence: Option<&str>,
    ctx: &EventCtx,
) -> Result<Vec<Criterion>> {
    let evidence = validate_evidence(evidence)?;
    validate_all(add)?;
    let check = unique_keep_order(check);
    let uncheck = unique_keep_order(uncheck);
    for n in &check {
        if uncheck.contains(n) {
            return Err(Error::Other(format!(
                "criterion {n} is both checked and unchecked"
            )));
        }
    }
    with_task(db, task_uuid, |tx| {
        let before = list_in_conn(tx, task_uuid)?;
        let state = |n: i64| before.iter().find(|c| c.n == n).map(|c| c.done);
        for &n in &check {
            match state(n) {
                None => {
                    return Err(Error::Other(format!("no criterion {n} on this task")));
                }
                Some(true) => {
                    return Err(Error::Other(format!("criterion {n} is already checked")));
                }
                Some(false) => {}
            }
        }
        for &n in &uncheck {
            if state(n) != Some(true) {
                return Err(Error::Other(format!("criterion {n} is not checked")));
            }
        }
        if !add.is_empty() {
            add_in_conn(tx, task_uuid, add, ctx)?;
        }
        for (i, n) in check.iter().enumerate() {
            check_in_conn(tx, task_uuid, *n, evidence, ctx, &format!("check{i}"))?;
        }
        for (i, n) in uncheck.iter().enumerate() {
            uncheck_in_conn(tx, task_uuid, *n, ctx, &format!("uncheck{i}"))?;
        }
        list_in_conn(tx, task_uuid)
    })
}

/// Uncheck every criterion (journaled). No-op when the task has none.
/// `extra` is merged into the event payload (`task_uuid` is always set).
pub fn reset_in_conn(
    conn: &rusqlite::Connection,
    task_uuid: &str,
    ctx: &EventCtx,
    extra: serde_json::Value,
) -> Result<()> {
    if list_in_conn(conn, task_uuid)?.is_empty() {
        return Ok(());
    }
    let mut payload = extra;
    if let Some(obj) = payload.as_object_mut() {
        obj.entry("task_uuid")
            .or_insert_with(|| serde_json::json!(task_uuid));
    }
    crate::event_log::record_in_conn(
        conn,
        &event_uuid(ctx, "criteria-reset"),
        Some(task_uuid),
        "task.criteria_reset",
        &payload,
        ctx,
    )?;
    Ok(())
}

/// Remove criterion `n` from the definition of done (journaled; the number
/// is not reused).
pub fn remove(db: &Db, task_uuid: &str, n: i64, ctx: &EventCtx) -> Result<Criterion> {
    with_task(db, task_uuid, |tx| {
        let c = current(tx, task_uuid, n)?;
        crate::event_log::record_in_conn(
            tx,
            &event_uuid(ctx, ""),
            Some(task_uuid),
            "task.criterion_removed",
            &serde_json::json!({ "task_uuid": task_uuid, "n": n, "text": c.text }),
            ctx,
        )?;
        Ok(c)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{self, NewTask};

    fn fresh() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("ac.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn criteria_fold_from_the_journal_and_keep_their_numbers() {
        let (_d, db) = fresh();
        let t = tasks::create(
            &db,
            NewTask::minimal("ship the backup drill"),
            &EventCtx::test(),
        )
        .unwrap();
        let hal = EventCtx::local("hal");
        add(
            &db,
            &t.id,
            &["restic check passes".into(), "drill on the timer".into()],
            &hal,
        )
        .unwrap();
        check(&db, &t.id, 1, Some("restic check: 0 errors"), &hal).unwrap();
        remove(&db, &t.id, 2, &EventCtx::local("shell")).unwrap();
        add(&db, &t.id, &["offsite copy verified".into()], &hal).unwrap();
        let got = list(&db, &t.id).unwrap();
        let summary: Vec<(i64, &str, bool)> =
            got.iter().map(|c| (c.n, c.text.as_str(), c.done)).collect();
        assert_eq!(
            summary,
            [
                (1, "restic check passes", true),
                (3, "offsite copy verified", false)
            ],
            "2 is not reused"
        );
        assert_eq!(got[0].checked_by.as_deref(), Some("hal"));
        assert_eq!(got[0].evidence.as_deref(), Some("restic check: 0 errors"));
        uncheck(&db, &t.id, 1, &hal).unwrap();
        assert!(!list(&db, &t.id).unwrap()[0].done);
        assert!(check(&db, &t.id, 9, None, &hal).is_err());
        assert!(add(&db, &t.id, &["  ".into()], &hal).is_err());
        assert!(add(&db, &t.id, &["x".repeat(MAX_CRITERION_CHARS + 1)], &hal).is_err());
    }

    #[test]
    fn unchecked_criteria_refuse_the_close_until_met() {
        let (_d, db) = fresh();
        let ctx = EventCtx::test();
        let t = tasks::create(&db, NewTask::minimal("roll the OSDs"), &ctx).unwrap();
        add(
            &db,
            &t.id,
            &["all 24 OSDs up".into(), "HEALTH_OK".into()],
            &ctx,
        )
        .unwrap();
        let err = tasks::mark_done(&db, &t, &ctx).unwrap_err();
        assert!(matches!(err, Error::Blocked(_)), "{err}");
        let msg = err.to_string();
        assert!(
            msg.contains("1. all 24 OSDs up") && msg.contains("2. HEALTH_OK"),
            "{msg}"
        );
        check(&db, &t.id, 1, None, &ctx).unwrap();
        let msg = tasks::mark_done(&db, &t, &ctx).unwrap_err().to_string();
        assert!(
            !msg.contains("1. all 24") && msg.contains("2. HEALTH_OK"),
            "{msg}"
        );
        check(&db, &t.id, 2, Some("ceph -s"), &ctx).unwrap();
        tasks::mark_done(&db, &t, &ctx).unwrap();
        // A task without criteria closes as before.
        let plain = tasks::create(&db, NewTask::minimal("plain"), &ctx).unwrap();
        tasks::mark_done(&db, &plain, &ctx).unwrap();
    }

    #[test]
    fn a_recurring_occurrence_must_meet_its_criteria_again() {
        let (_d, db) = fresh();
        let ctx = EventCtx::test();
        let mut new = NewTask::minimal("weekly restore drill");
        new.deadline = Some("2099-01-05".into());
        let t = tasks::create_with_extensions(
            &db,
            new,
            crate::Extensions {
                recurrence: Some(crate::recurrence::parse("every monday").unwrap()),
                acceptance: vec!["restore verified".into()],
                ..Default::default()
            },
            &ctx,
        )
        .unwrap();
        assert_eq!(
            list(&db, &t.id).unwrap().len(),
            1,
            "created with its criteria"
        );
        check(&db, &t.id, 1, None, &ctx).unwrap();
        let outcome = tasks::mark_done(&db, &t, &ctx).unwrap();
        assert!(matches!(outcome, tasks::DoneOutcome::Advanced { .. }));
        assert!(
            !list(&db, &t.id).unwrap()[0].done,
            "the next occurrence starts unchecked"
        );
    }

    #[test]
    fn a_rejected_create_writes_neither_task_nor_criteria() {
        let (_d, db) = fresh();
        let bad = crate::Extensions {
            acceptance: vec!["ok".into(), "   ".into()],
            ..Default::default()
        };
        assert!(
            tasks::create_with_extensions(&db, NewTask::minimal("t"), bad, &EventCtx::test())
                .is_err()
        );
        let n: i64 = db
            .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn a_bad_batch_writes_nothing() {
        let (_d, db) = fresh();
        let ctx = EventCtx::test();
        let t = tasks::create(&db, NewTask::minimal("batch"), &ctx).unwrap();
        add(&db, &t.id, &["one".into(), "two".into()], &ctx).unwrap();
        let before = list(&db, &t.id).unwrap();
        assert!(
            apply_batch(
                &db,
                &t.id,
                &["three".into()],
                &[1, 1],
                &[],
                Some(&"e".repeat(MAX_EVIDENCE_CHARS + 1)),
                &ctx,
            )
            .is_err()
        );
        assert_eq!(list(&db, &t.id).unwrap(), before);
        let after = apply_batch(&db, &t.id, &["three".into()], &[1, 1], &[], None, &ctx).unwrap();
        assert!(after.iter().any(|c| c.text == "three" && !c.done));
        assert!(after[0].done);
    }

    #[test]
    fn reopening_resets_checks() {
        let (_d, db) = fresh();
        let ctx = EventCtx::test();
        let t = tasks::create_with_extensions(
            &db,
            NewTask::minimal("ship notes"),
            crate::Extensions {
                acceptance: vec!["reviewed".into()],
                ..Default::default()
            },
            &ctx,
        )
        .unwrap();
        check(&db, &t.id, 1, None, &ctx).unwrap();
        tasks::mark_done(&db, &t, &ctx).unwrap();
        tasks::reopen(&db, &t.id, &ctx).unwrap();
        assert!(!list(&db, &t.id).unwrap()[0].done);
    }
}
