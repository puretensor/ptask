//! Task CRUD — direct SQL against the existing Python `tasks` table.
//!
//! The schema is owned by Python until v0.9. This module preserves byte-for-byte
//! defaults so rows written by `pt` and rows written by `python3 cli.py add`
//! are indistinguishable.

use crate::error::Result;
use crate::storage::Db;
use crate::{priority, pt_id};
use jiff::Zoned;
use rusqlite::params;
use serde::Serialize;
use tracing::debug;
use uuid::Uuid;

/// Mirror of a row in `tasks` + the joined `pt_extensions.pt_id`.
#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: String,
    pub pt_id: Option<String>,
    pub title: String,
    pub description: String,
    pub priority: i64,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub deadline: Option<String>,
    pub source_type: String,
    pub ai_reasoning: String,
}

/// What to write for a new task. Matches Python `create_task` parameters.
pub struct NewTask {
    pub title: String,
    pub description: String,
    pub priority: i64,
    pub deadline: Option<String>,
    pub source_type: String,
    pub ai_confidence: f64,
    pub ai_reasoning: String,
}

impl NewTask {
    pub fn minimal(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            description: String::new(),
            priority: 2,
            deadline: None,
            source_type: "manual".into(),
            ai_confidence: 1.0,
            ai_reasoning: String::new(),
        }
    }
}

/// Optional pTask-native fields written to `pt_extensions` alongside a `tasks` row.
/// Any None / empty entry leaves the corresponding column at its default.
#[derive(Debug, Default, Clone)]
pub struct Extensions {
    pub labels: Vec<String>,
    pub project: Option<String>,
    pub duration_min: Option<i64>,
    pub planned_at: Option<String>,
    pub energy: Option<String>,
    /// Optional recurrence rule. When set, a row is written to
    /// `pt_recurrence` in the same transaction as the task insert.
    /// `next_occurrence` is initialised from the task's deadline.
    pub recurrence: Option<crate::recurrence::Recurrence>,
}

/// Insert a task with byte-for-byte Python defaults, mint a PT-N, log a
/// `create` interaction. Returns the created Task.
pub fn create(db: &Db, new: NewTask) -> Result<Task> {
    create_with_extensions(db, new, Extensions::default())
}

/// Variant that also writes pTask-native fields into `pt_extensions`
/// (labels JSON, project, duration_min, planned_at, energy) in the same
/// transaction. Used by the inline-token quick-add path.
pub fn create_with_extensions(db: &Db, new: NewTask, ext: Extensions) -> Result<Task> {
    let id = Uuid::new_v4().to_string();
    let now = iso_now();

    let mut conn = db.get()?;
    let tx = conn.transaction()?;

    tx.execute(
        "INSERT INTO tasks (
            id, title, description, priority, status, created_at, updated_at,
            deadline, source_type, source_files, ai_confidence, ai_reasoning,
            depends_on, blocks_tasks, escalation_level, dismissal_count,
            priority_score, score_urgency, score_dependency, score_neglect,
            subtasks, task_type, cluster_keywords
         ) VALUES (
            ?1, ?2, ?3, ?4, 'pending', ?5, ?5,
            ?6, ?7, '[]', ?8, ?9,
            '[]', '[]', 0, 0,
            0.0, 0.0, 0.0, 0.0,
            '[]', 'operational', '[]'
         )",
        params![
            id,
            new.title,
            new.description,
            new.priority,
            now,
            new.deadline,
            new.source_type,
            new.ai_confidence,
            new.ai_reasoning,
        ],
    )?;

    // Mint PT-N + write extension columns. Reuse the same tx for atomicity.
    let n: i64 = tx.query_row(
        "UPDATE pt_counters SET value = value + 1 WHERE name='pt_id' RETURNING value",
        [],
        |r| r.get(0),
    )?;
    let pt_id_str = pt_id::format_pt_id(n);
    let labels_json = serde_json::to_string(&ext.labels)
        .map_err(|e| crate::Error::Other(format!("labels serialise: {}", e)))?;
    tx.execute(
        "INSERT INTO pt_extensions (
            task_uuid, pt_id, status_category, labels, project,
            duration_min, planned_at, energy, created_by_pt
         ) VALUES (?1, ?2, 'todo', ?3, ?4, ?5, ?6, ?7, 1)",
        params![
            id,
            pt_id_str,
            labels_json,
            ext.project,
            ext.duration_min,
            ext.planned_at,
            ext.energy,
        ],
    )?;

    // Recurrence: persist a pt_recurrence row when the caller supplies one.
    // next_occurrence is seeded from the task's deadline (must be Some for
    // a recurring task — the quick-add layer always sets one).
    if let Some(rec) = ext.recurrence.as_ref() {
        let next_occ = new.deadline.clone().ok_or_else(|| {
            crate::Error::Other(
                "recurrence requires a deadline (first occurrence); none provided".into(),
            )
        })?;
        let mode_str = match rec.mode {
            crate::recurrence::Mode::Fixed => "fixed",
            crate::recurrence::Mode::Completion => "completion",
        };
        tx.execute(
            "INSERT INTO pt_recurrence (task_uuid, rrule, mode, original_input, next_occurrence)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, rec.rrule_str, mode_str, rec.original_input, next_occ],
        )?;
    }

    // Audit-log the creation. Reuses Python `interactions` table verbatim.
    tx.execute(
        "INSERT INTO interactions (task_id, action, ts, details) VALUES (?1, 'create', ?2, ?3)",
        params![id, now, format!("Claude Code manual insert: {}", new.title),],
    )?;

    tx.commit()?;
    debug!(target: "ptask::tasks", pt_id = %pt_id_str, "created");

    Ok(Task {
        id,
        pt_id: Some(pt_id_str),
        title: new.title,
        description: new.description,
        priority: new.priority,
        status: "pending".into(),
        created_at: now.clone(),
        updated_at: now,
        deadline: new.deadline,
        source_type: new.source_type,
        ai_reasoning: new.ai_reasoning,
    })
}

/// List tasks against an optional DSL filter, optionally intersected with
/// status + priority. Replaces `list()` when filter support is needed; the
/// existing `list()` remains for parity callers that don't use the DSL.
pub fn list_with_filter(
    db: &Db,
    filter_expr: Option<&crate::filter::Expr>,
    status: Option<&str>,
    priority_filter: Option<i64>,
    limit: usize,
) -> Result<Vec<Task>> {
    let conn = db.get()?;
    let mut sql = String::from(
        "SELECT t.id, x.pt_id, t.title, t.description, t.priority, t.status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning
         FROM tasks t
         LEFT JOIN pt_extensions x ON x.task_uuid = t.id",
    );
    let mut conds: Vec<String> = Vec::new();
    let mut bound: Vec<rusqlite::types::Value> = Vec::new();

    if let Some(expr) = filter_expr {
        let now = crate::dates::now_in_operator_tz()?;
        let compiled = crate::filter::to_sql(expr, &now)?;
        // Shift the filter's positional placeholders to start after our offset.
        let shift = bound.len();
        let shifted = renumber_placeholders(&compiled.where_clause, shift);
        conds.push(shifted);
        bound.extend(compiled.params);
    }
    if let Some(s) = status {
        bound.push(rusqlite::types::Value::Text(s.to_string()));
        conds.push(format!("t.status = ?{}", bound.len()));
    }
    if let Some(p) = priority_filter {
        bound.push(rusqlite::types::Value::Integer(p));
        conds.push(format!("t.priority = ?{}", bound.len()));
    }
    if !conds.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conds.join(" AND "));
    }
    sql.push_str(" ORDER BY t.priority_score DESC, t.priority DESC, t.created_at DESC LIMIT ?");
    bound.push(rusqlite::types::Value::Integer(limit as i64));
    // Final placeholder index for LIMIT.
    let limit_idx = bound.len();
    sql = sql.replacen("LIMIT ?", &format!("LIMIT ?{}", limit_idx), 1);

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::ToSql> =
        bound.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let rows = stmt.query_map(params_refs.as_slice(), row_to_task)?;
    let out: Vec<Task> = rows.collect::<std::result::Result<_, _>>()?;
    Ok(out)
}

/// Shift positional `?N` placeholders in `sql` by `shift` so they don't
/// collide with externally-bound parameters that precede them.
fn renumber_placeholders(sql: &str, shift: usize) -> String {
    if shift == 0 {
        return sql.to_string();
    }
    let mut out = String::with_capacity(sql.len() + 4);
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'?' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 {
                let n: usize = sql[i + 1..j].parse().unwrap();
                out.push_str(&format!("?{}", n + shift));
                i = j;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// List tasks (optionally filtered by status + priority). Mirrors Python `get_tasks`.
pub fn list(
    db: &Db,
    status: Option<&str>,
    priority_filter: Option<i64>,
    limit: usize,
) -> Result<Vec<Task>> {
    let conn = db.get()?;
    let mut sql = String::from(
        "SELECT t.id, x.pt_id, t.title, t.description, t.priority, t.status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning
         FROM tasks t
         LEFT JOIN pt_extensions x ON x.task_uuid = t.id",
    );
    let mut conds: Vec<String> = Vec::new();
    let mut bound: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(s) = status {
        conds.push(format!("t.status = ?{}", bound.len() + 1));
        bound.push(Box::new(s.to_string()));
    }
    if let Some(p) = priority_filter {
        conds.push(format!("t.priority = ?{}", bound.len() + 1));
        bound.push(Box::new(p));
    }
    if !conds.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conds.join(" AND "));
    }
    sql.push_str(" ORDER BY t.priority_score DESC, t.priority DESC, t.created_at DESC LIMIT ?");
    bound.push(Box::new(limit as i64));

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::ToSql> = bound.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(params_refs.as_slice(), |r| {
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
        })
    })?;
    let out: Vec<Task> = rows.collect::<std::result::Result<_, _>>()?;
    Ok(out)
}

/// Resolve a query string to a task. Accepts:
///
/// - PT-N (case-insensitive) -> exact pt_id match
/// - Bare integer N         -> treated as PT-N
/// - Otherwise              -> case-insensitive substring on title (status='pending')
///
/// Returns the matched task, or an error describing zero / multiple matches.
pub fn resolve(db: &Db, query: &str) -> Result<Task> {
    let conn = db.get()?;
    let q = query.trim();
    let upper = q.to_ascii_uppercase();

    // PT-N exact match.
    let pt_candidate: Option<String> = if upper.starts_with("PT-") {
        Some(upper.clone())
    } else if let Ok(n) = q.parse::<i64>() {
        Some(format!("PT-{}", n))
    } else {
        None
    };

    if let Some(pt_id_str) = pt_candidate {
        let row = conn.query_row(
            "SELECT t.id, x.pt_id, t.title, t.description, t.priority, t.status,
                    t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning
             FROM tasks t JOIN pt_extensions x ON x.task_uuid = t.id
             WHERE x.pt_id = ?1",
            [&pt_id_str],
            row_to_task,
        );
        return match row {
            Ok(t) => Ok(t),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(crate::Error::PtIdNotFound(pt_id_str)),
            Err(e) => Err(e.into()),
        };
    }

    // Title substring search on pending tasks.
    let mut stmt = conn.prepare(
        "SELECT t.id, x.pt_id, t.title, t.description, t.priority, t.status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning
         FROM tasks t LEFT JOIN pt_extensions x ON x.task_uuid = t.id
         WHERE t.status = 'pending' AND lower(t.title) LIKE ?1 ESCAPE '\\'
         ORDER BY t.priority_score DESC, t.priority DESC, t.created_at DESC",
    )?;
    let pat = format!("%{}%", escape_like_pattern(&q.to_ascii_lowercase()));
    let rows: Vec<Task> = stmt
        .query_map([&pat], row_to_task)?
        .collect::<std::result::Result<_, _>>()?;

    match rows.len() {
        0 => Err(crate::Error::Other(format!(
            "no pending task matching '{}'",
            query
        ))),
        1 => Ok(rows.into_iter().next().unwrap()),
        n => {
            let titles: Vec<String> = rows
                .into_iter()
                .map(|t| {
                    format!(
                        "  - {} {}",
                        t.pt_id.as_deref().unwrap_or("(no PT-id)"),
                        t.title
                    )
                })
                .collect();
            Err(crate::Error::Other(format!(
                "{} pending tasks match '{}':\n{}",
                n,
                query,
                titles.join("\n")
            )))
        }
    }
}

/// Outcome of `mark_done`: either the task was completed, or it was
/// recurring and the deadline was advanced in-place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoneOutcome {
    Completed,
    Advanced { next_deadline: String },
}

/// Mark a task done. If the task has a `pt_recurrence` row, the deadline
/// is advanced in-place and `status` stays `pending` (Todoist-style).
/// Otherwise the task is set to `status='done'`. Either way an
/// `interactions` row is logged.
pub fn mark_done(db: &Db, task: &Task) -> Result<DoneOutcome> {
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let now = iso_now();

    // Look up the recurrence rule, if any.
    let rec_row: Option<(String, String)> = tx
        .query_row(
            "SELECT mode, original_input FROM pt_recurrence WHERE task_uuid = ?1",
            [&task.id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;

    if let Some((mode_str, original)) = rec_row {
        let rec = crate::recurrence::parse(&original)
            .map_err(|e| crate::Error::Other(format!("re-parse recurrence: {}", e)))?;
        // Pick the anchor for next_after based on mode:
        //   Fixed      → from the current deadline (preserves cadence)
        //   Completion → from now (drifts forward with completions)
        let anchor: jiff::Zoned = match mode_str.as_str() {
            "fixed" => match &task.deadline {
                Some(d) => parse_iso_zoned(d)?,
                None => crate::dates::now_in_operator_tz()?,
            },
            "completion" => crate::dates::now_in_operator_tz()?,
            other => {
                return Err(crate::Error::Other(format!(
                    "recurrence: unknown mode in pt_recurrence: {:?}",
                    other
                )));
            }
        };
        let next_z = crate::recurrence::next_after(&rec, &anchor)?;
        let next_iso = crate::dates::format_iso(&next_z);

        tx.execute(
            "UPDATE tasks SET deadline=?1, updated_at=?2 WHERE id=?3",
            params![next_iso, now, task.id],
        )?;
        tx.execute(
            "UPDATE pt_recurrence SET next_occurrence=?1 WHERE task_uuid=?2",
            params![next_iso, task.id],
        )?;
        tx.execute(
            "INSERT INTO interactions (task_id, action, ts, details)
             VALUES (?1, 'recurrence_advance', ?2, ?3)",
            params![
                task.id,
                now,
                format!("Recurring task advanced to {}", next_iso),
            ],
        )?;
        tx.commit()?;
        return Ok(DoneOutcome::Advanced {
            next_deadline: next_iso,
        });
    }

    tx.execute(
        "UPDATE tasks SET status='done', updated_at=?1 WHERE id=?2",
        params![now, task.id],
    )?;
    tx.execute(
        "INSERT INTO interactions (task_id, action, ts, details)
         VALUES (?1, 'status_change', ?2, 'Completed via Claude Code')",
        params![task.id, now],
    )?;
    tx.commit()?;
    Ok(DoneOutcome::Completed)
}

/// Parse an ISO-formatted deadline string (as produced by `dates::format_iso`,
/// or any ISO-8601 with an offset) back to a `Zoned` anchored in the operator
/// timezone. Bare date strings (`YYYY-MM-DD`) are interpreted at midnight in
/// the operator tz.
fn parse_iso_zoned(s: &str) -> Result<jiff::Zoned> {
    let tz = jiff::tz::TimeZone::get(crate::dates::OPERATOR_TZ)
        .map_err(|e| crate::Error::Other(format!("operator tz: {}", e)))?;
    // ISO with offset → Timestamp → Zoned in operator tz.
    if let Ok(ts) = s.parse::<jiff::Timestamp>() {
        return Ok(ts.to_zoned(tz));
    }
    // Bare date.
    if let Ok(d) = s.parse::<jiff::civil::Date>() {
        return d
            .at(0, 0, 0, 0)
            .to_zoned(tz)
            .map_err(|e| crate::Error::Other(format!("date→zoned {}: {}", s, e)));
    }
    Err(crate::Error::Other(format!(
        "parse iso zoned {:?}: not a Timestamp or Date",
        s
    )))
}

/// Status label formatter for CLI parity with Python output.
pub fn priority_label(p: i64) -> &'static str {
    priority::label(p)
}

fn row_to_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
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
    })
}

/// ISO-8601 UTC timestamp matching the existing Python format
/// (e.g. `2026-05-13T17:34:56.789012+00:00`).
fn iso_now() -> String {
    let now: Zoned = Zoned::now().with_time_zone(jiff::tz::TimeZone::UTC);
    let base = now.strftime("%Y-%m-%dT%H:%M:%S").to_string();
    let micros = now.subsec_nanosecond().div_euclid(1_000);
    if micros == 0 {
        format!("{base}+00:00")
    } else {
        format!("{base}.{micros:06}+00:00")
    }
}

fn escape_like_pattern(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        // Minimal Python schema stub (matches the live shape closely enough for tests).
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

    #[test]
    fn create_inserts_and_mints_pt_id() {
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("Buy bread")).unwrap();
        assert_eq!(t.pt_id.as_deref(), Some("PT-1"));
        assert_eq!(t.priority, 2);
        assert_eq!(t.status, "pending");

        // Interaction logged.
        db.with_conn(|c| {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM interactions WHERE task_id=?1 AND action='create'",
                    [&t.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1);
            let details: String = c
                .query_row(
                    "SELECT details FROM interactions WHERE task_id=?1 AND action='create'",
                    [&t.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(details, "Claude Code manual insert: Buy bread");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn list_filters_by_status_and_priority() {
        let (_dir, db) = fresh_db();
        create(&db, NewTask::minimal("low task")).unwrap();
        let mut high = NewTask::minimal("high task");
        high.priority = 4;
        create(&db, high).unwrap();

        let all = list(&db, Some("pending"), None, 100).unwrap();
        assert_eq!(all.len(), 2);
        let only_high = list(&db, Some("pending"), Some(4), 100).unwrap();
        assert_eq!(only_high.len(), 1);
        assert_eq!(only_high[0].title, "high task");
    }

    #[test]
    fn list_orders_by_priority_score_before_priority() {
        let (_dir, db) = fresh_db();
        let mut critical = NewTask::minimal("critical but unscored");
        critical.priority = 5;
        create(&db, critical).unwrap();
        let scored = create(&db, NewTask::minimal("normal but scored")).unwrap();

        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET priority_score=10.0 WHERE id=?1",
                [&scored.id],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();

        let rows = list(&db, Some("pending"), None, 100).unwrap();
        assert_eq!(rows[0].title, "normal but scored");
    }

    #[test]
    fn resolve_by_pt_n_and_substring() {
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("Buy artisanal bread")).unwrap();
        let by_pt = resolve(&db, "PT-1").unwrap();
        assert_eq!(by_pt.id, t.id);
        let by_short = resolve(&db, "1").unwrap();
        assert_eq!(by_short.id, t.id);
        let by_sub = resolve(&db, "artisanal").unwrap();
        assert_eq!(by_sub.id, t.id);
    }

    #[test]
    fn resolve_substring_with_multiple_matches_errors() {
        let (_dir, db) = fresh_db();
        create(&db, NewTask::minimal("Buy bread")).unwrap();
        create(&db, NewTask::minimal("Buy milk")).unwrap();
        let err = resolve(&db, "Buy").unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("2 pending tasks match"), "msg was: {}", msg);
    }

    #[test]
    fn resolve_substring_treats_like_wildcards_literally() {
        let (_dir, db) = fresh_db();
        create(&db, NewTask::minimal("literal percent % task")).unwrap();
        create(&db, NewTask::minimal("plain task")).unwrap();

        let by_percent = resolve(&db, "%").unwrap();
        assert_eq!(by_percent.title, "literal percent % task");

        let err = resolve(&db, "_").unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("no pending task matching '_'"),
            "msg was: {}",
            msg
        );
    }

    #[test]
    fn mark_done_returns_completed_for_non_recurring() {
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("solo task")).unwrap();
        let outcome = mark_done(&db, &t).unwrap();
        assert_eq!(outcome, DoneOutcome::Completed);
    }

    #[test]
    fn create_with_recurrence_writes_pt_recurrence_row() {
        let (_dir, db) = fresh_db();
        let rec = crate::recurrence::parse("every monday").unwrap();
        let mut new = NewTask::minimal("standup");
        new.deadline = Some("2026-05-18T09:00:00+01:00".into());
        let ext = Extensions {
            recurrence: Some(rec.clone()),
            ..Default::default()
        };
        let t = create_with_extensions(&db, new, ext).unwrap();
        db.with_conn(|c| {
            let (rrule, mode, next): (String, String, String) = c
                .query_row(
                    "SELECT rrule, mode, next_occurrence FROM pt_recurrence WHERE task_uuid=?1",
                    [&t.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(rrule, rec.rrule_str);
            assert_eq!(mode, "fixed");
            assert_eq!(next, "2026-05-18T09:00:00+01:00");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn mark_done_advances_fixed_recurring_from_current_deadline() {
        let (_dir, db) = fresh_db();
        let rec = crate::recurrence::parse("every monday").unwrap();
        let mut new = NewTask::minimal("standup");
        // Mon 2026-05-18 09:00 BST.
        new.deadline = Some("2026-05-18T09:00:00+01:00".into());
        let ext = Extensions {
            recurrence: Some(rec),
            ..Default::default()
        };
        let t = create_with_extensions(&db, new, ext).unwrap();
        let outcome = mark_done(&db, &t).unwrap();
        match outcome {
            DoneOutcome::Advanced { next_deadline } => {
                // Fixed mode anchors on the original deadline → next Monday.
                assert!(
                    next_deadline.starts_with("2026-05-25T09:00"),
                    "got {next_deadline}"
                );
            }
            other => panic!("expected Advanced, got {:?}", other),
        }
        db.with_conn(|c| {
            let status: String = c
                .query_row("SELECT status FROM tasks WHERE id=?1", [&t.id], |r| r.get(0))
                .unwrap();
            // Status stays pending — Todoist-style advance-in-place.
            assert_eq!(status, "pending");
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM interactions WHERE task_id=?1 AND action='recurrence_advance'",
                    [&t.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn mark_done_advances_completion_recurring_from_now() {
        let (_dir, db) = fresh_db();
        let rec = crate::recurrence::parse("every! 5 days").unwrap();
        let mut new = NewTask::minimal("water plants");
        // Stale deadline well in the past; Completion mode ignores it.
        new.deadline = Some("2024-01-01T12:00:00+00:00".into());
        let ext = Extensions {
            recurrence: Some(rec),
            ..Default::default()
        };
        let t = create_with_extensions(&db, new, ext).unwrap();
        let outcome = mark_done(&db, &t).unwrap();
        match outcome {
            DoneOutcome::Advanced { next_deadline } => {
                // Completion mode → 5 days from now, so the new deadline must
                // be in the future, NOT 2024-01-06.
                assert!(
                    !next_deadline.starts_with("2024-"),
                    "got {next_deadline} — should be ~5 days from now"
                );
            }
            other => panic!("expected Advanced, got {:?}", other),
        }
    }

    #[test]
    fn mark_done_for_existing_test_keeps_status_done() {
        // Preserves prior behaviour test of non-recurring completion path,
        // including the interaction details string.
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("write tests")).unwrap();
        mark_done(&db, &t).unwrap();
        db.with_conn(|c| {
            let status: String = c
                .query_row("SELECT status FROM tasks WHERE id=?1", [&t.id], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(status, "done");
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM interactions WHERE task_id=?1 AND action='status_change'",
                    [&t.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1);
            let details: String = c
                .query_row(
                    "SELECT details FROM interactions WHERE task_id=?1 AND action='status_change'",
                    [&t.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(details, "Completed via Claude Code");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn iso_now_uses_python_utc_offset_format() {
        let ts = iso_now();
        assert!(ts.ends_with("+00:00"), "timestamp was {ts}");
        assert!(!ts.ends_with('Z'), "timestamp was {ts}");

        let core = ts.strip_suffix("+00:00").unwrap();
        let (datetime, micros) = core
            .split_once('.')
            .map_or((core, None), |(dt, frac)| (dt, Some(frac)));
        assert_eq!(datetime.len(), "2026-05-13T17:34:56".len());
        if let Some(frac) = micros {
            assert_eq!(frac.len(), 6);
            assert!(frac.chars().all(|ch| ch.is_ascii_digit()));
        }
    }
}
