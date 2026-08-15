//! Task CRUD — direct SQL against the existing Python `tasks` table.
//!
//! The schema is owned by Python until v0.9. This module preserves byte-for-byte
//! defaults so rows written by `pt` and rows written by `python3 cli.py add`
//! are indistinguishable.

use crate::error::Result;
use crate::event_log::EventCtx;
use crate::storage::Db;
use crate::{priority, pt_id};
use jiff::Zoned;
use rusqlite::OptionalExtension;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use tracing::debug;
use uuid::Uuid;

/// Mirror of a row in `tasks` + the joined `pt_extensions.pt_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Scheduled date (`due:` token) — when the operator PLANS to do it,
    /// distinct from the hard `deadline`.
    pub due_at: Option<String>,
    /// Optional recurrence rule. When set, a row is written to
    /// `pt_recurrence` in the same transaction as the task insert.
    /// `next_occurrence` is initialised from the task's deadline.
    pub recurrence: Option<crate::recurrence::Recurrence>,
}

/// Insert a task with byte-for-byte Python defaults, mint a PT-N, log a
/// `create` interaction. Returns the created Task.
pub fn create(db: &Db, new: NewTask, ctx: &EventCtx) -> Result<Task> {
    create_with_extensions(db, new, Extensions::default(), ctx)
}

/// Generate an idempotency uuid for a locally-initiated mutation (CLI, TUI,
/// bot). Remote-initiated mutations supply the client's command uuid instead
/// so `/sync` replays stay idempotent.
fn local_event_uuid() -> String {
    format!("local:{}", Uuid::new_v4())
}

/// Write the attributed event row inside the mutation's own transaction.
/// Every event recorded here is what delta sync clients see — a mutation
/// without an event row is invisible to the fleet, and a mutation without
/// an actor is invisible to the audit trail; `ctx` is how the compiler
/// forces every writer to identify itself.
fn record_event_tx(
    tx: &rusqlite::Connection,
    ctx: &EventCtx,
    task_uuid: &str,
    event_type: &str,
    payload: &serde_json::Value,
) -> Result<()> {
    let generated;
    let uuid = match ctx.event_uuid.as_deref() {
        Some(u) => u,
        None => {
            generated = local_event_uuid();
            &generated
        }
    };
    crate::event_log::record_in_conn(tx, uuid, Some(task_uuid), event_type, payload, ctx)?;
    Ok(())
}

/// Variant that also writes pTask-native fields into `pt_extensions`
/// (labels JSON, project, duration_min, planned_at, energy) in the same
/// transaction. The attributed `task.created` event commits with the
/// insert; `ctx.event_uuid` carries the /sync idempotency key when remote.
pub fn create_with_extensions(
    db: &Db,
    new: NewTask,
    ext: Extensions,
    ctx: &EventCtx,
) -> Result<Task> {
    let id = Uuid::new_v4().to_string();
    let now = iso_now();

    let mut conn = db.get()?;
    let tx = conn.transaction()?;

    // Mint PT-N first so the row insert is a single statement.
    let n: i64 = tx.query_row(
        "UPDATE pt_counters SET value = value + 1 WHERE name='pt_id' RETURNING value",
        [],
        |r| r.get(0),
    )?;
    let pt_id_str = pt_id::format_pt_id(n);

    tx.execute(
        "INSERT INTO tasks (
            id, title, description, priority, status, status_v2,
            created_at, updated_at, deadline, source_type, source_files,
            ai_confidence, ai_reasoning, depends_on, blocks_tasks,
            escalation_level, dismissal_count, priority_score, score_urgency,
            score_dependency, score_neglect, subtasks, task_type,
            cluster_keywords, pt_id, project, duration_min, planned_at,
            energy, created_by_pt, due_at
         ) VALUES (
            ?1, ?2, ?3, ?4, 'pending', 'todo',
            ?5, ?5, ?6, ?7, '[]',
            ?8, ?9, '[]', '[]',
            0, 0, 0.0, 0.0,
            0.0, 0.0, '[]', 'operational',
            '[]', ?10, ?11, ?12, ?13,
            ?14, 1, ?15
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
            pt_id_str,
            ext.project,
            ext.duration_min,
            ext.planned_at,
            ext.energy,
            ext.due_at,
        ],
    )?;

    for label in &ext.labels {
        tx.execute(
            "INSERT OR IGNORE INTO task_labels (task_uuid, label) VALUES (?1, ?2)",
            params![id, label],
        )?;
    }

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
        params![id, now, format!("created by {}: {}", ctx.actor, new.title),],
    )?;

    let task = Task {
        id,
        pt_id: Some(pt_id_str.clone()),
        title: new.title,
        description: new.description,
        priority: new.priority,
        status: "pending".into(),
        created_at: now.clone(),
        updated_at: now,
        deadline: new.deadline,
        source_type: new.source_type,
        ai_reasoning: new.ai_reasoning,
    };
    let payload = serde_json::to_value(&task)
        .map_err(|e| crate::Error::Other(format!("task.created payload: {}", e)))?;
    record_event_tx(&tx, ctx, &task.id, "task.created", &payload)?;

    tx.commit()?;
    debug!(target: "ptask::tasks", pt_id = %pt_id_str, "created");

    Ok(task)
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
        // The filter DSL compiles `@label` / `#project` atoms to `x.labels` /
        // `x.project` (see filter::to_sql), so the base query must join the
        // pt_extensions compat view under alias `x`. LEFT JOIN keeps tasks with
        // no extension row (e.g. pre-backfill legacy rows) visible for
        // non-label/project filters.
        "SELECT t.id, t.pt_id, t.title, t.description, t.priority, t.status_v2 AS status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning
         FROM tasks t LEFT JOIN pt_extensions x ON x.task_uuid = t.id",
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
        "SELECT t.id, t.pt_id, t.title, t.description, t.priority, t.status_v2 AS status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning
         FROM tasks t",
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

/// List every task row, including completed tasks.
pub fn list_all(db: &Db) -> Result<Vec<Task>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT t.id, t.pt_id, t.title, t.description, t.priority, t.status_v2 AS status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning
         FROM tasks t
         ORDER BY t.priority_score DESC, t.priority DESC, t.created_at DESC",
    )?;
    let rows = stmt.query_map([], row_to_task)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
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
            "SELECT t.id, t.pt_id, t.title, t.description, t.priority, t.status_v2 AS status,
                    t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning
             FROM tasks t
             WHERE t.pt_id = ?1",
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
        "SELECT t.id, t.pt_id, t.title, t.description, t.priority, t.status_v2 AS status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning
         FROM tasks t
         WHERE t.status_v2 NOT IN ('done','dismissed') AND lower(t.title) LIKE ?1 ESCAPE '\\'
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

/// Resolve a task for server-side remote lookup/search.
///
/// Semantics match the pre-v1.12 remote CLI resolver, but execute in SQLite on
/// the canonical host instead of forcing the client to full-sync every task:
///
/// - PT-N (case-insensitive) -> exact pt_id match, any status
/// - Bare integer N         -> treated as PT-N, any status
/// - Otherwise              -> case-insensitive title substring
///   - `include_terminal=false`: excludes `done` and `dismissed`
///   - `include_terminal=true`: searches all statuses
///
/// Returns the matched task, or an error describing zero / multiple matches.
pub fn resolve_for_lookup(db: &Db, query: &str, include_terminal: bool) -> Result<Task> {
    let conn = db.get()?;
    let q = query.trim();
    if q.is_empty() {
        return Err(crate::Error::Other("empty task query".into()));
    }

    // Exact task uuid (the /tg/callback + machine-caller path — a uuid must
    // never fall through to title-substring matching).
    if q.len() == 36
        && q.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
    {
        let row = conn.query_row(
            "SELECT t.id, t.pt_id, t.title, t.description, t.priority, t.status_v2 AS status,
                    t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning
             FROM tasks t
             WHERE t.id = ?1",
            [&q.to_ascii_lowercase()],
            row_to_task,
        );
        return match row {
            Ok(t) => Ok(t),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                Err(crate::Error::Other(format!("no task with uuid {}", q)))
            }
            Err(e) => Err(e.into()),
        };
    }
    let upper = q.to_ascii_uppercase();

    let pt_candidate: Option<String> = if upper.starts_with("PT-") {
        Some(upper)
    } else if let Ok(n) = q.parse::<i64>() {
        Some(format!("PT-{}", n))
    } else {
        None
    };

    if let Some(pt_id_str) = pt_candidate {
        let row = conn.query_row(
            "SELECT t.id, t.pt_id, t.title, t.description, t.priority, t.status_v2 AS status,
                    t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning
             FROM tasks t
             WHERE t.pt_id = ?1",
            [&pt_id_str],
            row_to_task,
        );
        return match row {
            Ok(t) => Ok(t),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(crate::Error::PtIdNotFound(pt_id_str)),
            Err(e) => Err(e.into()),
        };
    }

    let mut sql = String::from(
        "SELECT t.id, t.pt_id, t.title, t.description, t.priority, t.status_v2 AS status,
                t.created_at, t.updated_at, t.deadline, t.source_type, t.ai_reasoning
         FROM tasks t
         WHERE lower(t.title) LIKE ?1 ESCAPE '\\'",
    );
    if !include_terminal {
        sql.push_str(" AND t.status NOT IN ('done', 'dismissed')");
    }
    sql.push_str(" ORDER BY t.priority_score DESC, t.priority DESC, t.created_at DESC");

    let mut stmt = conn.prepare(&sql)?;
    let pat = format!("%{}%", escape_like_pattern(&q.to_ascii_lowercase()));
    let rows: Vec<Task> = stmt
        .query_map([&pat], row_to_task)?
        .collect::<std::result::Result<_, _>>()?;

    match rows.len() {
        0 => {
            let scope = if include_terminal {
                "task"
            } else {
                "active task"
            };
            Err(crate::Error::Other(format!(
                "no {} matching '{}'",
                scope, query
            )))
        }
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
                "{} tasks match '{}':\n{}",
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
/// Mark done. The `task.completed` / `task.recurrence_advanced` event
/// commits in the same transaction as the status flip, attributed to `ctx`.
pub fn mark_done(db: &Db, task: &Task, ctx: &EventCtx) -> Result<DoneOutcome> {
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let now = iso_now();

    // Look up the recurrence rule, if any.
    let rec_row: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT r.mode, r.original_input, t.deadline
             FROM pt_recurrence AS r
             JOIN tasks AS t ON t.id = r.task_uuid
             WHERE r.task_uuid = ?1",
            [&task.id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;

    if let Some((mode_str, original, current_deadline)) = rec_row {
        let rec = crate::recurrence::parse(&original)
            .map_err(|e| crate::Error::Other(format!("re-parse recurrence: {}", e)))?;
        let completion_now = crate::dates::now_in_operator_tz()?;
        let explicit_time = recurrence_time_of_day(&original, &completion_now)?;
        // Pick the anchor for next_after based on mode:
        //   Fixed      → from the current deadline (preserves cadence)
        //   Completion → from now (drifts forward with completions)
        let anchor: jiff::Zoned = match mode_str.as_str() {
            "fixed" => match &current_deadline {
                Some(d) => parse_iso_zoned(d)?,
                None => completion_now.clone(),
            },
            "completion" => completion_now.clone(),
            other => {
                return Err(crate::Error::Other(format!(
                    "recurrence: unknown mode in pt_recurrence: {:?}",
                    other
                )));
            }
        };
        let mut next_z = crate::recurrence::next_after(&rec, &anchor)?;
        if mode_str == "fixed" {
            while next_z <= completion_now {
                next_z = crate::recurrence::next_after(&rec, &next_z)?;
            }
        }
        if mode_str == "completion"
            && let Some(time) = explicit_time.as_ref()
        {
            next_z = combine_date_with_time(&next_z, time)?;
        }
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
        record_event_tx(
            &tx,
            ctx,
            &task.id,
            "task.recurrence_advanced",
            &serde_json::json!({
                "task_uuid": task.id,
                "pt_id": task.pt_id,
                "next_deadline": next_iso,
            }),
        )?;
        tx.commit()?;
        return Ok(DoneOutcome::Advanced {
            next_deadline: next_iso,
        });
    }

    tx.execute(
        "UPDATE tasks SET status='done', status_v2='done', updated_at=?1 WHERE id=?2",
        params![now, task.id],
    )?;
    tx.execute(
        "INSERT INTO interactions (task_id, action, ts, details)
         VALUES (?1, 'status_change', ?2, 'Completed via Claude Code')",
        params![task.id, now],
    )?;
    record_event_tx(
        &tx,
        ctx,
        &task.id,
        "task.completed",
        &serde_json::json!({ "task_uuid": task.id, "pt_id": task.pt_id }),
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

fn recurrence_time_of_day(original: &str, now: &jiff::Zoned) -> Result<Option<jiff::Zoned>> {
    let (_rule, time) = crate::recurrence::split_time_suffix(original);
    time.map(|t| crate::dates::parse_at(&format!("today {}", t), now.clone()))
        .transpose()
}

/// Replace the time-of-day of `date_z` with the time-of-day of `time_z`,
/// keeping `date_z`'s timezone.
fn combine_date_with_time(date_z: &jiff::Zoned, time_z: &jiff::Zoned) -> Result<jiff::Zoned> {
    let tz = date_z.time_zone().clone();
    let civil = date_z.date().at(
        time_z.hour(),
        time_z.minute(),
        time_z.second(),
        time_z.subsec_nanosecond(),
    );
    civil
        .to_zoned(tz)
        .map_err(|e| crate::Error::Other(format!("combine date+time: {}", e)))
}

/// Status label formatter for CLI parity with Python output.
pub fn priority_label(p: i64) -> &'static str {
    priority::label(p)
}

/// Build a Linear-style branch name from a PT-N + title.
/// `feature/PT-123-slug-of-title`. Slug is lowercase, ASCII, hyphen-joined,
/// capped at 50 chars of title (after the prefix). Re-running with the
/// same inputs returns the same string — safe to copy into a shell pipe.
pub fn branch_name(pt_id: &str, title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    let mut last_was_hyphen = true;
    for ch in title.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if ch.is_whitespace() || matches!(ch, '-' | '_' | '/' | '\\') {
            Some('-')
        } else {
            None
        };
        match mapped {
            Some('-') if !last_was_hyphen => {
                slug.push('-');
                last_was_hyphen = true;
            }
            Some('-') => {} // duplicate hyphen — already collapsed
            Some(c) => {
                slug.push(c);
                last_was_hyphen = false;
            }
            None => {} // strip
        }
    }
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(50).collect();
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        format!("feature/{}", pt_id)
    } else {
        format!("feature/{}-{}", pt_id, slug)
    }
}

/// Set a task's priority (1..=5). Logs a `priority_change` interaction.
/// Set priority. The `task.updated` event commits in the same transaction,
/// attributed to `ctx` and keyed on `ctx.event_uuid` for /sync idempotency.
pub fn update_priority(db: &Db, task_uuid: &str, priority: i64, ctx: &EventCtx) -> Result<()> {
    if !(1..=5).contains(&priority) {
        return Err(crate::Error::Other(format!(
            "priority {} out of range 1..=5",
            priority
        )));
    }
    let now = iso_now();
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let changed = tx.execute(
        "UPDATE tasks SET priority=?1, updated_at=?2 WHERE id=?3",
        params![priority, now, task_uuid],
    )?;
    if changed == 0 {
        return Err(crate::Error::Other("task not found".into()));
    }
    tx.execute(
        "INSERT INTO interactions (task_id, action, ts, details)
         VALUES (?1, 'priority_change', ?2, ?3)",
        params![task_uuid, now, format!("priority → {}", priority)],
    )?;
    record_event_tx(
        &tx,
        ctx,
        task_uuid,
        "task.updated",
        &serde_json::json!({ "task_uuid": task_uuid, "priority": priority }),
    )?;
    tx.commit()?;
    Ok(())
}

/// Set or clear a task deadline. Logs a `deadline_change` interaction.
/// Set or clear a deadline. The `task.updated` event commits in the same
/// transaction, attributed to `ctx`.
pub fn update_deadline(
    db: &Db,
    task_uuid: &str,
    deadline: Option<&str>,
    ctx: &EventCtx,
) -> Result<()> {
    if let Some(d) = deadline {
        parse_iso_zoned(d)?;
    }
    let now = iso_now();
    let mut conn = db.get()?;
    let tx = conn.transaction()?;

    let has_recurrence_table = tx
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='pt_recurrence'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;
    let has_recurrence = if has_recurrence_table {
        tx.query_row(
            "SELECT COUNT(*) FROM pt_recurrence WHERE task_uuid=?1",
            [task_uuid],
            |r| r.get::<_, i64>(0),
        )? > 0
    } else {
        false
    };
    if deadline.is_none() && has_recurrence {
        return Err(crate::Error::Other(
            "cannot clear deadline on a recurring task; update it instead".into(),
        ));
    }

    let changed = tx.execute(
        "UPDATE tasks SET deadline=?1, updated_at=?2 WHERE id=?3",
        params![deadline, now, task_uuid],
    )?;
    if changed == 0 {
        return Err(crate::Error::Other("task not found".into()));
    }
    if has_recurrence {
        tx.execute(
            "UPDATE pt_recurrence SET next_occurrence=?1 WHERE task_uuid=?2",
            params![deadline, task_uuid],
        )?;
    }
    tx.execute(
        "INSERT INTO interactions (task_id, action, ts, details)
         VALUES (?1, 'deadline_change', ?2, ?3)",
        params![
            task_uuid,
            now,
            match deadline {
                Some(d) => format!("deadline → {}", d),
                None => "deadline cleared".to_string(),
            }
        ],
    )?;
    record_event_tx(
        &tx,
        ctx,
        task_uuid,
        "task.updated",
        &serde_json::json!({ "task_uuid": task_uuid, "deadline": deadline }),
    )?;
    tx.commit()?;
    Ok(())
}

/// Delete a task (and any side-table rows via ON DELETE CASCADE). The row
/// vanishes; the audit trail in `interactions` is wiped with it, but a
/// `task.deleted` tombstone lands in `pt_event_log` so delta sync clients
/// learn about the removal. Use with care.
pub fn delete_task(db: &Db, task_uuid: &str, ctx: &EventCtx) -> Result<()> {
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    // Capture the PT-N before the CASCADE wipes pt_extensions.
    let pt_id: Option<String> = tx
        .query_row("SELECT pt_id FROM tasks WHERE id=?1", [task_uuid], |r| {
            r.get(0)
        })
        .optional()?;
    tx.execute("DELETE FROM tasks WHERE id=?1", [task_uuid])?;
    record_event_tx(
        &tx,
        ctx,
        task_uuid,
        "task.deleted",
        &serde_json::json!({ "task_uuid": task_uuid, "pt_id": pt_id }),
    )?;
    tx.commit()?;
    Ok(())
}

/// Reopen a completed or dismissed task: flip `status` back to `'pending'`.
/// Errors if the task is already pending (nothing to do) or missing.
/// Reopen a completed/dismissed task (status → pending). The `task.updated`
/// event commits in the same transaction, attributed to `ctx`. The
/// `status_change` interaction's details contain `'pending'`, which the
/// neglect score reads as a reopen signal.
pub fn reopen(db: &Db, task_uuid: &str, ctx: &EventCtx) -> Result<()> {
    let now = iso_now();
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let status: Option<String> = tx
        .query_row("SELECT status FROM tasks WHERE id=?1", [task_uuid], |r| {
            r.get(0)
        })
        .optional()?;
    let status = status.ok_or_else(|| crate::Error::Other("task not found".into()))?;
    if !matches!(status.as_str(), "done" | "dismissed") {
        return Err(crate::Error::Other(
            "task is not done/dismissed — nothing to reopen".into(),
        ));
    }
    tx.execute(
        "UPDATE tasks SET status='pending', status_v2='todo', snoozed_until=NULL, updated_at=?1 WHERE id=?2",
        params![now, task_uuid],
    )?;
    tx.execute(
        "INSERT INTO interactions (task_id, action, ts, details)
         VALUES (?1, 'status_change', ?2, ?3)",
        params![
            task_uuid,
            now,
            format!("Reopened → pending (was {})", status)
        ],
    )?;
    record_event_tx(
        &tx,
        ctx,
        task_uuid,
        "task.updated",
        &serde_json::json!({ "task_uuid": task_uuid, "status": "pending" }),
    )?;
    tx.commit()?;
    Ok(())
}

/// Dismiss a task: set `status` to `'dismissed'` (a soft close — `reopen`
/// brings it back). Errors if the task is missing or already dismissed.
/// Dismiss (soft close; reversible via reopen). The `task.updated` event
/// commits in the same transaction, attributed to `ctx`.
pub fn dismiss(db: &Db, task_uuid: &str, ctx: &EventCtx) -> Result<()> {
    let now = iso_now();
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let status: Option<String> = tx
        .query_row("SELECT status FROM tasks WHERE id=?1", [task_uuid], |r| {
            r.get(0)
        })
        .optional()?;
    let status = status.ok_or_else(|| crate::Error::Other("task not found".into()))?;
    if status == "dismissed" {
        return Err(crate::Error::Other("task is already dismissed".into()));
    }
    tx.execute(
        "UPDATE tasks SET status='dismissed', status_v2='dismissed', updated_at=?1 WHERE id=?2",
        params![now, task_uuid],
    )?;
    tx.execute(
        "INSERT INTO interactions (task_id, action, ts, details)
         VALUES (?1, 'status_change', ?2, ?3)",
        params![task_uuid, now, format!("Dismissed (was {})", status)],
    )?;
    record_event_tx(
        &tx,
        ctx,
        task_uuid,
        "task.updated",
        &serde_json::json!({ "task_uuid": task_uuid, "status": "dismissed" }),
    )?;
    tx.commit()?;
    Ok(())
}

/// What `undo_last` reversed, for operator feedback.
#[derive(Debug, Clone)]
pub struct UndoOutcome {
    pub reversed_event_id: i64,
    pub description: String,
}

/// Reverse the most recent undoable mutation in the journal.
///
/// Undoable (honest v1 — reversals that need no "before" snapshot):
///   task.completed                 → reopen
///   task.updated{status=dismissed} → reopen
///   task.created                   → delete (with tombstone)
/// Everything else (priority/deadline/text edits, escalations) is skipped —
/// their events don't carry the prior state yet. The reversal itself is a
/// normal attributed mutation, so `pt log` shows both sides.
pub fn undo_last(db: &Db, ctx: &EventCtx) -> Result<UndoOutcome> {
    let candidates: Vec<(i64, String, String, String)> = {
        let conn = db.get()?;
        let mut stmt = conn.prepare(
            "SELECT id, task_uuid, event_type, payload FROM pt_event_log
             WHERE task_uuid IS NOT NULL
               AND event_type IN ('task.completed', 'task.created', 'task.updated')
             ORDER BY id DESC LIMIT 50",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    // Tasks with a newer, non-undoable modification (priority/deadline/text/
    // start/snooze edit) seen earlier in this newest-first scan. Their
    // `task.created` must NOT be deleted: the create is no longer the last
    // thing that happened, so deleting would silently discard the later edit.
    let mut modified_after_create: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    for (id, task_uuid, event_type, payload) in candidates {
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
        let exists: bool = {
            let conn = db.get()?;
            conn.query_row("SELECT 1 FROM tasks WHERE id=?1", [&task_uuid], |_| Ok(()))
                .optional()?
                .is_some()
        };
        if !exists {
            continue;
        }
        match event_type.as_str() {
            "task.completed" => {
                reopen(db, &task_uuid, ctx)?;
                return Ok(UndoOutcome {
                    reversed_event_id: id,
                    description: format!("reopened {} (was completed)", task_uuid),
                });
            }
            "task.updated"
                if payload.get("status").and_then(|s| s.as_str()) == Some("dismissed") =>
            {
                reopen(db, &task_uuid, ctx)?;
                return Ok(UndoOutcome {
                    reversed_event_id: id,
                    description: format!("reopened {} (was dismissed)", task_uuid),
                });
            }
            "task.created" => {
                if modified_after_create.contains(&task_uuid) {
                    // Created, then edited — "undo the create" would delete a
                    // task the operator has since worked on. Skip it.
                    continue;
                }
                delete_task(db, &task_uuid, ctx)?;
                return Ok(UndoOutcome {
                    reversed_event_id: id,
                    description: format!("deleted {} (undid create)", task_uuid),
                });
            }
            _ => {
                // A non-undoable edit (priority/deadline/text/start/snooze).
                // Record it so this task's create is protected from deletion
                // underneath a later modification.
                modified_after_create.insert(task_uuid);
                continue;
            }
        }
    }
    Err(crate::Error::Other(
        "nothing undoable in the recent journal (undo covers done/dismiss/create)".into(),
    ))
}

/// Mark a task in progress (status_v2 `in_progress`; legacy stays
/// `pending`). Attributed `task.updated` event in the same transaction.
pub fn start(db: &Db, task_uuid: &str, ctx: &EventCtx) -> Result<()> {
    let now = iso_now();
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let changed = tx.execute(
        "UPDATE tasks SET status_v2='in_progress', status='pending',
                          snoozed_until=NULL, updated_at=?1
         WHERE id=?2 AND status_v2 NOT IN ('done','dismissed')",
        params![now, task_uuid],
    )?;
    if changed == 0 {
        return Err(crate::Error::Other(
            "task not found or terminal — cannot start".into(),
        ));
    }
    tx.execute(
        "INSERT INTO interactions (task_id, action, ts, details)
         VALUES (?1, 'status_change', ?2, 'Started → in_progress')",
        params![task_uuid, now],
    )?;
    record_event_tx(
        &tx,
        ctx,
        task_uuid,
        "task.updated",
        &serde_json::json!({ "task_uuid": task_uuid, "status": "in_progress" }),
    )?;
    tx.commit()?;
    Ok(())
}

/// Atomically claim a task for agent work. The guarded status transition and
/// its sync-visible event are one transaction, so a successful claim can
/// never be committed without its audit record.
pub fn claim(db: &Db, task_uuid: &str, ctx: &EventCtx) -> Result<()> {
    let now = iso_now();
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let changed = tx.execute(
        "UPDATE tasks SET status_v2='in_progress', status='pending', updated_at=?1
         WHERE id=?2 AND status_v2 IN ('triage','backlog','todo')",
        params![now, task_uuid],
    )?;
    if changed == 0 {
        return Err(crate::Error::Other(
            "task not found or not claimable".into(),
        ));
    }
    record_event_tx(
        &tx,
        ctx,
        task_uuid,
        "task.claimed",
        &serde_json::json!({ "task_uuid": task_uuid, "by": ctx.actor }),
    )?;
    tx.commit()?;
    Ok(())
}

/// Snooze until `until_iso` (status_v2 `snoozed`; legacy `delayed`). The
/// task leaves `pt next` and accountability until the wake time passes,
/// when [`wake_expired_snoozes`] flips it back to todo.
pub fn snooze(db: &Db, task_uuid: &str, until_iso: &str, ctx: &EventCtx) -> Result<()> {
    parse_iso_zoned(until_iso)?;
    let now = iso_now();
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let changed = tx.execute(
        "UPDATE tasks SET status_v2='snoozed', status='delayed',
                          snoozed_until=?1, updated_at=?2
         WHERE id=?3 AND status_v2 NOT IN ('done','dismissed')",
        params![until_iso, now, task_uuid],
    )?;
    if changed == 0 {
        return Err(crate::Error::Other(
            "task not found or terminal — cannot snooze".into(),
        ));
    }
    tx.execute(
        "INSERT INTO interactions (task_id, action, ts, details)
         VALUES (?1, 'status_change', ?2, ?3)",
        params![task_uuid, now, format!("Snoozed until {}", until_iso)],
    )?;
    record_event_tx(
        &tx,
        ctx,
        task_uuid,
        "task.updated",
        &serde_json::json!({
            "task_uuid": task_uuid, "status": "snoozed", "snoozed_until": until_iso
        }),
    )?;
    tx.commit()?;
    Ok(())
}

/// Wake every snoozed task whose `snoozed_until` has passed (→ todo).
/// Invoked by the hourly scoring run so snoozes expire without their own
/// timer. Returns the number woken; each wake is an attributed event.
///
/// An unparseable `snoozed_until` wakes immediately: `julianday()` returns
/// NULL on junk and a NULL comparison is never true, so such a row would
/// otherwise stay `snoozed` forever with no timer that could ever fire it.
pub fn wake_expired_snoozes(db: &Db, now_iso: &str, ctx: &EventCtx) -> Result<usize> {
    let expired: Vec<String> = {
        let conn = db.get()?;
        let mut stmt = conn.prepare(
            "SELECT id FROM tasks
             WHERE status_v2='snoozed' AND snoozed_until IS NOT NULL
               AND (julianday(snoozed_until) IS NULL
                    OR (length(snoozed_until) = 10
                        AND snoozed_until <= substr(?1, 1, 10))
                    OR (length(snoozed_until) > 10
                        AND julianday(snoozed_until) <= julianday(?1)))",
        )?;
        let rows = stmt.query_map([now_iso], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let mut woken = 0usize;
    for uuid in &expired {
        let mut conn = db.get()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE tasks SET status_v2='todo', status='pending',
                              snoozed_until=NULL, updated_at=?1
              WHERE id=?2 AND status_v2='snoozed'
                AND snoozed_until IS NOT NULL
                AND (julianday(snoozed_until) IS NULL
                     OR (length(snoozed_until) = 10
                         AND snoozed_until <= substr(?1, 1, 10))
                     OR (length(snoozed_until) > 10
                         AND julianday(snoozed_until) <= julianday(?1)))",
            params![now_iso, uuid],
        )?;
        if changed == 0 {
            continue;
        }
        record_event_tx(
            &tx,
            ctx,
            uuid,
            "task.updated",
            &serde_json::json!({ "task_uuid": uuid, "status": "todo", "woke_from_snooze": true }),
        )?;
        tx.commit()?;
        woken += 1;
    }
    Ok(woken)
}

/// Add a `depends_on` edge: `from` cannot start until `to` is done.
/// Rejects self-dependency and cycles (bounded walk — the graph is small).
pub fn add_dependency(db: &Db, from_uuid: &str, to_uuid: &str, ctx: &EventCtx) -> Result<()> {
    if from_uuid == to_uuid {
        return Err(crate::Error::Other("a task cannot depend on itself".into()));
    }
    let now = iso_now();
    let mut conn = db.get()?;
    // Acquire SQLite's single-writer reservation before reading the graph.
    // Without it, two callers can both validate against the old graph and
    // then serially insert opposite edges, creating a cycle.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let exists: i64 = tx.query_row(
        "SELECT COUNT(*) FROM tasks WHERE id IN (?1, ?2)",
        params![from_uuid, to_uuid],
        |r| r.get(0),
    )?;
    if exists != 2 {
        return Err(crate::Error::Other("both tasks must exist".into()));
    }
    // Cycle check: is `from` reachable FROM `to` via depends_on edges?
    let mut frontier = vec![to_uuid.to_string()];
    let mut seen = std::collections::HashSet::new();
    while let Some(cur) = frontier.pop() {
        if cur == from_uuid {
            return Err(crate::Error::Other(
                "dependency would create a cycle".into(),
            ));
        }
        if !seen.insert(cur.clone()) || seen.len() > 10_000 {
            continue;
        }
        let mut stmt =
            tx.prepare("SELECT to_uuid FROM task_links WHERE from_uuid=?1 AND kind='depends_on'")?;
        let next: Vec<String> = stmt
            .query_map([&cur], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<_, _>>()?;
        frontier.extend(next);
    }
    tx.execute(
        "INSERT OR IGNORE INTO task_links (from_uuid, to_uuid, kind, created_at)
         VALUES (?1, ?2, 'depends_on', ?3)",
        params![from_uuid, to_uuid, now],
    )?;
    record_event_tx(
        &tx,
        ctx,
        from_uuid,
        "task.updated",
        &serde_json::json!({ "task_uuid": from_uuid, "depends_on_added": to_uuid }),
    )?;
    tx.commit()?;
    Ok(())
}

/// Remove a `depends_on` edge. Errors if the edge doesn't exist.
pub fn remove_dependency(db: &Db, from_uuid: &str, to_uuid: &str, ctx: &EventCtx) -> Result<()> {
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let n = tx.execute(
        "DELETE FROM task_links WHERE from_uuid=?1 AND to_uuid=?2 AND kind='depends_on'",
        params![from_uuid, to_uuid],
    )?;
    if n == 0 {
        return Err(crate::Error::Other("no such dependency edge".into()));
    }
    record_event_tx(
        &tx,
        ctx,
        from_uuid,
        "task.updated",
        &serde_json::json!({ "task_uuid": from_uuid, "depends_on_removed": to_uuid }),
    )?;
    tx.commit()?;
    Ok(())
}

/// Update title and/or description (at least one `Some`). The
/// `task.updated` event commits in the same transaction, attributed to `ctx`.
pub fn update_text(
    db: &Db,
    task_uuid: &str,
    title: Option<&str>,
    description: Option<&str>,
    ctx: &EventCtx,
) -> Result<()> {
    if title.is_none() && description.is_none() {
        return Err(crate::Error::Other("update_text: nothing to change".into()));
    }
    let now = iso_now();
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let exists = tx
        .query_row("SELECT 1 FROM tasks WHERE id=?1", [task_uuid], |_| Ok(()))
        .optional()?
        .is_some();
    if !exists {
        return Err(crate::Error::Other("task not found".into()));
    }
    if let Some(t) = title {
        tx.execute(
            "UPDATE tasks SET title=?1, updated_at=?2 WHERE id=?3",
            params![t, now, task_uuid],
        )?;
    }
    if let Some(d) = description {
        tx.execute(
            "UPDATE tasks SET description=?1, updated_at=?2 WHERE id=?3",
            params![d, now, task_uuid],
        )?;
    }
    let what = match (title.is_some(), description.is_some()) {
        (true, true) => "title + description edited",
        (true, false) => "title edited",
        (false, true) => "description edited",
        (false, false) => unreachable!("guarded above"),
    };
    tx.execute(
        "INSERT INTO interactions (task_id, action, ts, details) VALUES (?1, 'edit', ?2, ?3)",
        params![task_uuid, now, what],
    )?;
    record_event_tx(
        &tx,
        ctx,
        task_uuid,
        "task.updated",
        &serde_json::json!({ "task_uuid": task_uuid, "title": title, "description": description }),
    )?;
    tx.commit()?;
    Ok(())
}

/// Add/remove labels on an existing task (`task_labels` side table).
/// Labels don't feed the composite score, so callers skip the rescore that
/// priority/deadline edits trigger. Adds are INSERT OR IGNORE and removes of
/// absent labels are no-ops, so a retried command converges on the same state.
pub fn modify_labels(
    db: &Db,
    task_uuid: &str,
    add: &[String],
    remove: &[String],
    ctx: &EventCtx,
) -> Result<()> {
    let add: Vec<&str> = add
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let remove: Vec<&str> = remove
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    if add.is_empty() && remove.is_empty() {
        return Err(crate::Error::Other(
            "modify_labels: nothing to change".into(),
        ));
    }
    let now = iso_now();
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let exists = tx
        .query_row("SELECT 1 FROM tasks WHERE id=?1", [task_uuid], |_| Ok(()))
        .optional()?
        .is_some();
    if !exists {
        return Err(crate::Error::Other("task not found".into()));
    }
    for l in &add {
        tx.execute(
            "INSERT OR IGNORE INTO task_labels (task_uuid, label) VALUES (?1, ?2)",
            params![task_uuid, l],
        )?;
    }
    for l in &remove {
        tx.execute(
            "DELETE FROM task_labels WHERE task_uuid=?1 AND label=?2",
            params![task_uuid, l],
        )?;
    }
    tx.execute(
        "UPDATE tasks SET updated_at=?1 WHERE id=?2",
        params![now, task_uuid],
    )?;
    let what = format!("labels edited (+{} -{})", add.len(), remove.len());
    tx.execute(
        "INSERT INTO interactions (task_id, action, ts, details) VALUES (?1, 'edit', ?2, ?3)",
        params![task_uuid, now, what],
    )?;
    record_event_tx(
        &tx,
        ctx,
        task_uuid,
        "task.updated",
        &serde_json::json!({ "task_uuid": task_uuid, "labels_add": add, "labels_remove": remove }),
    )?;
    tx.commit()?;
    Ok(())
}

/// Extension fields for a single task, loaded on demand. Used by the TUI
/// detail pane and any other surface that wants the full row + side-table
/// state without paying the cost for every list query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDetail {
    pub labels: Vec<String>,
    pub project: Option<String>,
    pub duration_min: Option<i64>,
    pub planned_at: Option<String>,
    pub energy: Option<String>,
    pub depends_on: Vec<String>,
    pub blocks_tasks: Vec<String>,
    pub recurrence_input: Option<String>,
    pub recurrence_mode: Option<String>,
    pub recurrence_next: Option<String>,
}

/// Load the side-table state for one task. Returns defaults for missing rows.
pub fn load_detail(db: &Db, task_uuid: &str) -> Result<TaskDetail> {
    let conn = db.get()?;
    // Merged v2 columns (schema v2: pt_extensions folded into tasks).
    let ext: (Option<String>, Option<i64>, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT project, duration_min, planned_at, energy
             FROM tasks WHERE id = ?1",
            [task_uuid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap_or((None, None, None, None));

    let mut lstmt =
        conn.prepare("SELECT label FROM task_labels WHERE task_uuid = ?1 ORDER BY label")?;
    let labels: Vec<String> = lstmt
        .query_map([task_uuid], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<_, _>>()?;
    drop(lstmt);

    // Relations from task_links (schema v2 replaced the JSON blobs).
    let mut dstmt = conn
        .prepare("SELECT to_uuid FROM task_links WHERE from_uuid = ?1 AND kind = 'depends_on'")?;
    let depends_on: Vec<String> = dstmt
        .query_map([task_uuid], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<_, _>>()?;
    drop(dstmt);
    let mut bstmt = conn
        .prepare("SELECT from_uuid FROM task_links WHERE to_uuid = ?1 AND kind = 'depends_on'")?;
    let blocks_tasks: Vec<String> = bstmt
        .query_map([task_uuid], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<_, _>>()?;
    drop(bstmt);

    // pt_recurrence (optional).
    let rec: Option<(String, String, String)> = conn
        .query_row(
            "SELECT original_input, mode, next_occurrence
             FROM pt_recurrence WHERE task_uuid = ?1",
            [task_uuid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();

    Ok(TaskDetail {
        labels,
        project: ext.0,
        duration_min: ext.1,
        planned_at: ext.2,
        energy: ext.3,
        depends_on,
        blocks_tasks,
        recurrence_input: rec.as_ref().map(|r| r.0.clone()),
        recurrence_mode: rec.as_ref().map(|r| r.1.clone()),
        recurrence_next: rec.as_ref().map(|r| r.2.clone()),
    })
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

    fn event_count(db: &Db, event_type: &str) -> i64 {
        db.with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM pt_event_log WHERE event_type=?1",
                [event_type],
                |r| r.get(0),
            )?)
        })
        .unwrap()
    }

    #[test]
    fn create_records_task_created_event_in_same_tx() {
        let (_dir, db) = fresh_db();
        let t = create(
            &db,
            NewTask::minimal("local create logs event"),
            &EventCtx::test(),
        )
        .unwrap();
        db.with_conn(|c| {
            let (uuid, task_uuid): (String, String) = c.query_row(
                "SELECT uuid, task_uuid FROM pt_event_log WHERE event_type='task.created'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert!(uuid.starts_with("local:"), "uuid was {}", uuid);
            assert_eq!(task_uuid, t.id);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn list_with_filter_matches_label_and_project() {
        // Regression: the `@label` / `#project` filter atoms compile to
        // `x.labels` / `x.project`, which require the base query to
        // `LEFT JOIN pt_extensions x ON x.task_uuid = t.id` (documented at
        // filter.rs `to_sql`). `list_with_filter` omitted that join, so every
        // label/project filter errored at runtime with `no such column:
        // x.labels` across the CLI, HTTP, MCP, bot, and TUI surfaces.
        let (_dir, db) = fresh_db();
        let ext = Extensions {
            labels: vec!["ops".into()],
            project: Some("fleet".into()),
            ..Default::default()
        };
        create_with_extensions(
            &db,
            NewTask::minimal("wire up the fleet dashboard"),
            ext,
            &EventCtx::test(),
        )
        .unwrap();

        let by_label = crate::filter::parse("@ops").unwrap();
        let hit = list_with_filter(&db, Some(&by_label), Some("pending"), None, 50).unwrap();
        assert_eq!(hit.len(), 1, "@ops label filter should match the task");

        let by_project = crate::filter::parse("#fleet").unwrap();
        let hit = list_with_filter(&db, Some(&by_project), Some("pending"), None, 50).unwrap();
        assert_eq!(hit.len(), 1, "#fleet project filter should match the task");

        // A non-matching label still resolves cleanly (empty, not an error).
        let miss = crate::filter::parse("@nope").unwrap();
        let hit = list_with_filter(&db, Some(&miss), Some("pending"), None, 50).unwrap();
        assert!(hit.is_empty(), "unrelated label should match nothing");
    }

    #[test]
    fn mark_done_records_completed_event() {
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("done logs event"), &EventCtx::test()).unwrap();
        mark_done(&db, &t, &EventCtx::test()).unwrap();
        assert_eq!(event_count(&db, "task.completed"), 1);
    }

    #[test]
    fn reopen_returns_task_to_pending_and_logs_reopen_signal() {
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("reopen me"), &EventCtx::test()).unwrap();
        mark_done(&db, &t, &EventCtx::test()).unwrap();

        let status_done: String = db
            .with_conn(|c| {
                Ok(
                    c.query_row("SELECT status FROM tasks WHERE id=?1", [&t.id], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(status_done, "done");

        reopen(&db, &t.id, &EventCtx::test()).unwrap();
        let status_reopened: String = db
            .with_conn(|c| {
                Ok(
                    c.query_row("SELECT status FROM tasks WHERE id=?1", [&t.id], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(status_reopened, "pending");

        // The reopen must leave the neglect-score signal the scorer reads:
        // a `status_change` interaction whose details contain 'pending'.
        let signals: i64 = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM interactions
                     WHERE task_id=?1 AND action='status_change' AND details LIKE '%pending%'",
                    [&t.id],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(signals, 1, "reopen must log a status_change→pending signal");
        assert_eq!(event_count(&db, "task.updated"), 1);

        // Reopening an already-pending task is an error (nothing to do).
        assert!(reopen(&db, &t.id, &EventCtx::test()).is_err());
    }

    #[test]
    fn update_text_changes_title_and_description() {
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("old title"), &EventCtx::test()).unwrap();

        update_text(
            &db,
            &t.id,
            Some("new title"),
            Some("a body"),
            &EventCtx::test(),
        )
        .unwrap();
        let (title, desc): (String, String) = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT title, description FROM tasks WHERE id=?1",
                    [&t.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();
        assert_eq!(title, "new title");
        assert_eq!(desc, "a body");
        assert_eq!(event_count(&db, "task.updated"), 1);

        // Title-only edit leaves the description untouched.
        update_text(&db, &t.id, Some("title2"), None, &EventCtx::test()).unwrap();
        let (title2, desc2): (String, String) = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT title, description FROM tasks WHERE id=?1",
                    [&t.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();
        assert_eq!(title2, "title2");
        assert_eq!(
            desc2, "a body",
            "description must survive a title-only edit"
        );

        // Nothing-to-change and missing-task are both errors.
        assert!(update_text(&db, &t.id, None, None, &EventCtx::test()).is_err());
        assert!(update_text(&db, "nonexistent-uuid", Some("x"), None, &EventCtx::test()).is_err());
    }

    #[test]
    fn modify_labels_adds_removes_and_records_updated_event() {
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("label me"), &EventCtx::test()).unwrap();

        let labels_of = |db: &Db| -> Vec<String> {
            db.with_conn(|c| {
                let mut st =
                    c.prepare("SELECT label FROM task_labels WHERE task_uuid=?1 ORDER BY label")?;
                let v = st
                    .query_map([&t.id], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(v)
            })
            .unwrap()
        };

        modify_labels(
            &db,
            &t.id,
            &["domain:mgmt".into(), "finance".into()],
            &[],
            &EventCtx::test(),
        )
        .unwrap();
        assert_eq!(labels_of(&db), vec!["domain:mgmt", "finance"]);
        assert_eq!(event_count(&db, "task.updated"), 1);

        // Re-adding an existing label is idempotent; a swap add+remove works in one call.
        modify_labels(
            &db,
            &t.id,
            &["domain:eng".into(), "finance".into()],
            &["domain:mgmt".into()],
            &EventCtx::test(),
        )
        .unwrap();
        assert_eq!(labels_of(&db), vec!["domain:eng", "finance"]);

        // Removing an absent label is a no-op, not an error.
        modify_labels(&db, &t.id, &[], &["ghost".into()], &EventCtx::test()).unwrap();
        assert_eq!(labels_of(&db), vec!["domain:eng", "finance"]);

        // Nothing-to-change (incl. whitespace-only), and missing task, are errors.
        assert!(modify_labels(&db, &t.id, &[], &[], &EventCtx::test()).is_err());
        assert!(modify_labels(&db, &t.id, &["  ".into()], &[], &EventCtx::test()).is_err());
        assert!(
            modify_labels(
                &db,
                "nonexistent-uuid",
                &["x".into()],
                &[],
                &EventCtx::test()
            )
            .is_err()
        );
    }

    #[test]
    fn dismiss_sets_status_and_reopen_reverses_it() {
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("dismiss me"), &EventCtx::test()).unwrap();

        dismiss(&db, &t.id, &EventCtx::test()).unwrap();
        let s: String = db
            .with_conn(|c| {
                Ok(
                    c.query_row("SELECT status FROM tasks WHERE id=?1", [&t.id], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(s, "dismissed");
        assert_eq!(event_count(&db, "task.updated"), 1);

        // Dismissing an already-dismissed task is an error.
        assert!(dismiss(&db, &t.id, &EventCtx::test()).is_err());

        // reopen reverses it back to pending.
        reopen(&db, &t.id, &EventCtx::test()).unwrap();
        let s2: String = db
            .with_conn(|c| {
                Ok(
                    c.query_row("SELECT status FROM tasks WHERE id=?1", [&t.id], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(s2, "pending");
    }

    #[test]
    fn update_priority_records_updated_event() {
        let (_dir, db) = fresh_db();
        let t = create(
            &db,
            NewTask::minimal("priority logs event"),
            &EventCtx::test(),
        )
        .unwrap();
        update_priority(&db, &t.id, 4, &EventCtx::test()).unwrap();
        assert_eq!(event_count(&db, "task.updated"), 1);
    }

    #[test]
    fn update_deadline_records_updated_event() {
        let (_dir, db) = fresh_db();
        let t = create(
            &db,
            NewTask::minimal("deadline logs event"),
            &EventCtx::test(),
        )
        .unwrap();
        update_deadline(&db, &t.id, Some("2026-06-16"), &EventCtx::test()).unwrap();
        update_deadline(&db, &t.id, None, &EventCtx::test()).unwrap();
        assert_eq!(event_count(&db, "task.updated"), 2);
    }

    #[test]
    fn delete_records_tombstone_with_pt_id() {
        let (_dir, db) = fresh_db();
        let t = create(
            &db,
            NewTask::minimal("delete logs tombstone"),
            &EventCtx::test(),
        )
        .unwrap();
        delete_task(&db, &t.id, &EventCtx::test()).unwrap();
        db.with_conn(|c| {
            let payload: String = c.query_row(
                "SELECT payload FROM pt_event_log WHERE event_type='task.deleted'",
                [],
                |r| r.get(0),
            )?;
            let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(v["pt_id"], "PT-1");
            assert_eq!(v["task_uuid"], t.id);
            let rows: i64 = c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?;
            assert_eq!(rows, 0);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn duplicate_event_uuid_rolls_back_the_whole_mutation() {
        // The atomicity guarantee: if the event row can't be written (replayed
        // idempotency uuid), the task mutation must not commit either.
        let (_dir, db) = fresh_db();
        let ctx = EventCtx::test().with_uuid("cmd-replayed");
        let first =
            create_with_extensions(&db, NewTask::minimal("first"), Extensions::default(), &ctx)
                .unwrap();
        let second =
            create_with_extensions(&db, NewTask::minimal("second"), Extensions::default(), &ctx);
        assert!(second.is_err(), "duplicate event uuid must fail");
        db.with_conn(|c| {
            let rows: i64 = c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?;
            assert_eq!(rows, 1, "second task insert must have rolled back");
            let counter: i64 = c.query_row(
                "SELECT value FROM pt_counters WHERE name='pt_id'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(counter, 1, "PT-N counter must have rolled back");
            Ok(())
        })
        .unwrap();
        assert_eq!(first.pt_id.as_deref(), Some("PT-1"));
    }

    #[test]
    fn create_inserts_and_mints_pt_id() {
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("Buy bread"), &EventCtx::test()).unwrap();
        assert_eq!(t.pt_id.as_deref(), Some("PT-1"));
        assert_eq!(t.priority, 2);
        assert_eq!(t.status, "pending");
        db.with_conn(|c| {
            let (pt, v2): (String, String) = c
                .query_row(
                    "SELECT pt_id, status_v2 FROM tasks WHERE id=?1",
                    [&t.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(pt, "PT-1");
            assert_eq!(v2, "todo");
            Ok(())
        })
        .unwrap();

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
            assert_eq!(details, "created by test: Buy bread");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn list_filters_by_status_and_priority() {
        let (_dir, db) = fresh_db();
        create(&db, NewTask::minimal("low task"), &EventCtx::test()).unwrap();
        let mut high = NewTask::minimal("high task");
        high.priority = 4;
        create(&db, high, &EventCtx::test()).unwrap();

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
        create(&db, critical, &EventCtx::test()).unwrap();
        let scored = create(
            &db,
            NewTask::minimal("normal but scored"),
            &EventCtx::test(),
        )
        .unwrap();

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
    fn update_deadline_sets_and_logs_interaction() {
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("date me"), &EventCtx::test()).unwrap();

        update_deadline(&db, &t.id, Some("2026-06-16"), &EventCtx::test()).unwrap();

        db.with_conn(|c| {
            let deadline: Option<String> = c
                .query_row("SELECT deadline FROM tasks WHERE id=?1", [&t.id], |r| r.get(0))
                .unwrap();
            assert_eq!(deadline.as_deref(), Some("2026-06-16"));
            let details: String = c
                .query_row(
                    "SELECT details FROM interactions WHERE task_id=?1 AND action='deadline_change'",
                    [&t.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(details, "deadline → 2026-06-16");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn update_deadline_can_clear_non_recurring_task() {
        let (_dir, db) = fresh_db();
        let mut new = NewTask::minimal("clear me");
        new.deadline = Some("2026-06-16".into());
        let t = create(&db, new, &EventCtx::test()).unwrap();

        update_deadline(&db, &t.id, None, &EventCtx::test()).unwrap();

        db.with_conn(|c| {
            let deadline: Option<String> = c
                .query_row("SELECT deadline FROM tasks WHERE id=?1", [&t.id], |r| {
                    r.get(0)
                })
                .unwrap();
            assert!(deadline.is_none());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn update_deadline_rejects_non_iso_deadline() {
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("validate me"), &EventCtx::test()).unwrap();

        let err = update_deadline(&db, &t.id, Some("not-a-date"), &EventCtx::test()).unwrap_err();
        assert!(format!("{}", err).contains("parse iso zoned"));
    }

    #[test]
    fn resolve_by_pt_n_and_substring() {
        let (_dir, db) = fresh_db();
        let t = create(
            &db,
            NewTask::minimal("Buy artisanal bread"),
            &EventCtx::test(),
        )
        .unwrap();
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
        create(&db, NewTask::minimal("Buy bread"), &EventCtx::test()).unwrap();
        create(&db, NewTask::minimal("Buy milk"), &EventCtx::test()).unwrap();
        let err = resolve(&db, "Buy").unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("2 pending tasks match"), "msg was: {}", msg);
    }

    #[test]
    fn resolve_substring_treats_like_wildcards_literally() {
        let (_dir, db) = fresh_db();
        create(
            &db,
            NewTask::minimal("literal percent % task"),
            &EventCtx::test(),
        )
        .unwrap();
        create(&db, NewTask::minimal("plain task"), &EventCtx::test()).unwrap();

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
    fn resolve_for_lookup_matches_pt_id_across_terminal_statuses() {
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("completed task"), &EventCtx::test()).unwrap();
        mark_done(&db, &t, &EventCtx::test()).unwrap();

        let by_pt = resolve_for_lookup(&db, "PT-1", false).unwrap();
        assert_eq!(by_pt.id, t.id);
        assert_eq!(by_pt.status, "done");
    }

    #[test]
    fn resolve_for_lookup_substring_scope_is_explicit() {
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("archive receipts"), &EventCtx::test()).unwrap();
        mark_done(&db, &t, &EventCtx::test()).unwrap();

        let err = resolve_for_lookup(&db, "archive", false).unwrap_err();
        assert!(format!("{}", err).contains("no active task matching"));

        let found = resolve_for_lookup(&db, "archive", true).unwrap();
        assert_eq!(found.id, t.id);
        assert_eq!(found.status, "done");
    }

    #[test]
    fn branch_name_lowercases_and_hyphenates() {
        assert_eq!(
            branch_name("PT-42", "Buy bread tomorrow 10am"),
            "feature/PT-42-buy-bread-tomorrow-10am"
        );
    }

    #[test]
    fn branch_name_strips_unicode_and_punctuation() {
        // The em-dash, comma, and ñ are non-ASCII / non-alphanum and get
        // collapsed to hyphens (or stripped, in the case of ñ).
        assert_eq!(
            branch_name("PT-1", "Café, deploy — now ñ"),
            "feature/PT-1-caf-deploy-now"
        );
    }

    #[test]
    fn branch_name_empty_title_returns_prefix_only() {
        assert_eq!(branch_name("PT-99", "   ,,, "), "feature/PT-99");
    }

    #[test]
    fn branch_name_truncates_long_title() {
        let t = "a".repeat(200);
        let b = branch_name("PT-1", &t);
        assert!(b.starts_with("feature/PT-1-"));
        // Slug is capped at 50 chars; total length therefore ~50+13.
        assert!(b.len() <= 50 + "feature/PT-1-".len());
    }

    #[test]
    fn mark_done_returns_completed_for_non_recurring() {
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("solo task"), &EventCtx::test()).unwrap();
        let outcome = mark_done(&db, &t, &EventCtx::test()).unwrap();
        assert_eq!(outcome, DoneOutcome::Completed);
    }

    #[test]
    fn create_with_recurrence_writes_pt_recurrence_row() {
        let (_dir, db) = fresh_db();
        let rec = crate::recurrence::parse("every monday at 9am").unwrap();
        let mut new = NewTask::minimal("standup");
        new.deadline = Some("2026-05-18T09:00:00+01:00".into());
        let ext = Extensions {
            recurrence: Some(rec.clone()),
            ..Default::default()
        };
        let t = create_with_extensions(&db, new, ext, &EventCtx::test()).unwrap();
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
            assert_eq!(rec.original_input, "every monday at 9am");
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
        let now = crate::dates::now_in_operator_tz().unwrap();
        let tz = jiff::tz::TimeZone::get(crate::dates::OPERATOR_TZ).unwrap();
        let mut current_deadline = now.clone();
        loop {
            current_deadline = current_deadline
                .date()
                .at(9, 0, 0, 0)
                .to_zoned(tz.clone())
                .unwrap();
            if current_deadline > now && current_deadline.weekday() == jiff::civil::Weekday::Monday
            {
                break;
            }
            current_deadline = current_deadline
                .checked_add(jiff::Span::new().days(1))
                .unwrap();
        }
        let expected_next = crate::recurrence::next_after(&rec, &current_deadline).unwrap();
        new.deadline = Some(crate::dates::format_iso(&current_deadline));
        let ext = Extensions {
            recurrence: Some(rec),
            ..Default::default()
        };
        let t = create_with_extensions(&db, new, ext, &EventCtx::test()).unwrap();
        let outcome = mark_done(&db, &t, &EventCtx::test()).unwrap();
        match outcome {
            DoneOutcome::Advanced { next_deadline } => {
                // Fixed mode anchors on the original deadline → next Monday.
                assert_eq!(next_deadline, crate::dates::format_iso(&expected_next));
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
    fn mark_done_uses_deadline_read_inside_the_transaction() {
        let (_dir, db) = fresh_db();
        let rec = crate::recurrence::parse("every day").unwrap();
        let mut new = NewTask::minimal("daily schedule");
        new.deadline = Some("2099-01-01T09:00:00+00:00".into());
        let ext = Extensions {
            recurrence: Some(rec.clone()),
            ..Default::default()
        };
        let stale = create_with_extensions(&db, new, ext, &EventCtx::test()).unwrap();
        let revised = "2099-02-01T09:00:00+00:00";
        update_deadline(&db, &stale.id, Some(revised), &EventCtx::test()).unwrap();
        let expected = crate::dates::format_iso(
            &crate::recurrence::next_after(&rec, &parse_iso_zoned(revised).unwrap()).unwrap(),
        );

        let outcome = mark_done(&db, &stale, &EventCtx::test()).unwrap();
        assert_eq!(
            outcome,
            DoneOutcome::Advanced {
                next_deadline: expected.clone()
            }
        );
        db.with_conn(|c| {
            let (deadline, next_occurrence): (String, String) = c.query_row(
                "SELECT t.deadline, r.next_occurrence
                 FROM tasks AS t
                 JOIN pt_recurrence AS r ON r.task_uuid = t.id
                 WHERE t.id = ?1",
                [&stale.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            assert_eq!(deadline, expected);
            assert_eq!(next_occurrence, expected);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn mark_done_fixed_recurring_skips_missed_occurrences() {
        let (_dir, db) = fresh_db();
        let rec = crate::recurrence::parse("every day").unwrap();
        let mut new = NewTask::minimal("daily stale");
        new.deadline = Some("2024-01-01T09:00:00+00:00".into());
        let ext = Extensions {
            recurrence: Some(rec),
            ..Default::default()
        };
        let t = create_with_extensions(&db, new, ext, &EventCtx::test()).unwrap();
        let outcome = mark_done(&db, &t, &EventCtx::test()).unwrap();
        match outcome {
            DoneOutcome::Advanced { next_deadline } => {
                assert!(
                    !next_deadline.starts_with("2024-"),
                    "got {next_deadline} — fixed mode should skip missed occurrences"
                );
                assert!(next_deadline.contains("T09:00:00"), "got {next_deadline}");
            }
            other => panic!("expected Advanced, got {:?}", other),
        }
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
        let t = create_with_extensions(&db, new, ext, &EventCtx::test()).unwrap();
        let outcome = mark_done(&db, &t, &EventCtx::test()).unwrap();
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
    fn mark_done_completion_recurring_preserves_explicit_time_of_day() {
        let (_dir, db) = fresh_db();
        let rec = crate::recurrence::parse("every! 5 days at 9am").unwrap();
        let mut new = NewTask::minimal("water plants");
        new.deadline = Some("2024-01-01T09:00:00+00:00".into());
        let ext = Extensions {
            recurrence: Some(rec),
            ..Default::default()
        };
        let t = create_with_extensions(&db, new, ext, &EventCtx::test()).unwrap();
        let outcome = mark_done(&db, &t, &EventCtx::test()).unwrap();
        match outcome {
            DoneOutcome::Advanced { next_deadline } => {
                assert!(next_deadline.contains("T09:00:00"), "got {next_deadline}");
            }
            other => panic!("expected Advanced, got {:?}", other),
        }
    }

    #[test]
    fn mark_done_for_existing_test_keeps_status_done() {
        // Preserves prior behaviour test of non-recurring completion path,
        // including the interaction details string.
        let (_dir, db) = fresh_db();
        let t = create(&db, NewTask::minimal("write tests"), &EventCtx::test()).unwrap();
        mark_done(&db, &t, &EventCtx::test()).unwrap();
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
    /// PHASE-3 GATE: every mutation path emits exactly one attributed event,
    /// the delta cursor sees every touched task, and tombstones appear on
    /// delete. If a new mutation forgets its event or its actor, this fails.
    #[test]
    fn every_mutation_emits_exactly_one_attributed_event() {
        let (_dir, db) = fresh_db();
        let ctx = EventCtx {
            actor: "gate".into(),
            source: "test".into(),
            event_uuid: None,
        };
        let count = |db: &Db| -> i64 {
            db.with_conn(
                |c| Ok(c.query_row("SELECT COUNT(*) FROM pt_event_log", [], |r| r.get(0))?),
            )
            .unwrap()
        };
        let last_actor = |db: &Db| -> (String, String) {
            db.with_conn(|c| {
                Ok(c.query_row(
                    "SELECT actor, payload FROM pt_event_log ORDER BY id DESC LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap()
        };

        let mut expected = 0i64;
        let t = create(&db, NewTask::minimal("gate task"), &ctx).unwrap();
        expected += 1;
        assert_eq!(count(&db), expected, "create emits one event");

        update_priority(&db, &t.id, 4, &ctx).unwrap();
        expected += 1;
        assert_eq!(count(&db), expected, "priority emits one event");

        update_deadline(&db, &t.id, Some("2026-08-01"), &ctx).unwrap();
        expected += 1;
        update_text(&db, &t.id, Some("gate task 2"), None, &ctx).unwrap();
        expected += 1;
        mark_done(&db, &t, &ctx).unwrap();
        expected += 1;
        reopen(&db, &t.id, &ctx).unwrap();
        expected += 1;
        dismiss(&db, &t.id, &ctx).unwrap();
        expected += 1;
        assert_eq!(
            count(&db),
            expected,
            "each mutation emits exactly one event"
        );

        // Attribution: column AND payload envelope carry the actor.
        let (actor, payload) = last_actor(&db);
        assert_eq!(actor, "gate");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["actor"], "gate");
        assert_eq!(v["source"], "test");

        // Cursor replay: the task is visible from cursor 0.
        let changed = crate::event_log::changed_task_uuids_since(&db, 0).unwrap();
        assert!(changed.contains(&t.id));

        // Delete emits a tombstone the delta protocol reports.
        delete_task(&db, &t.id, &ctx).unwrap();
        expected += 1;
        assert_eq!(count(&db), expected);
        let deleted = crate::event_log::deleted_task_uuids_since(&db, 0).unwrap();
        assert!(deleted.contains(&t.id));

        // pt log sees the attributed history.
        let history = crate::event_log::history_for_task(&db, &t.id, 50).unwrap();
        assert_eq!(history.len(), expected as usize);
        assert!(history.iter().all(|e| e.actor.as_deref() == Some("gate")));
    }

    /// Undo reverses done → pending as an attributed mutation of its own.
    #[test]
    fn undo_reverses_last_done() {
        let (_dir, db) = fresh_db();
        let ctx = EventCtx::test();
        let t = create(&db, NewTask::minimal("undo me"), &ctx).unwrap();
        mark_done(&db, &t, &ctx).unwrap();
        let out = undo_last(&db, &ctx).unwrap();
        assert!(out.description.contains("reopened"));
        let status: String = db
            .with_conn(|c| {
                Ok(
                    c.query_row("SELECT status FROM tasks WHERE id=?1", [&t.id], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(status, "pending");
    }

    #[test]
    fn wake_expired_snoozes_compares_instants_across_offsets() {
        let (_dir, db) = fresh_db();
        let ctx = EventCtx::test();
        let expired = create(&db, NewTask::minimal("wake me"), &ctx).unwrap();
        let future = create(&db, NewTask::minimal("not yet"), &ctx).unwrap();
        snooze(&db, &expired.id, "2026-07-01T07:00:00Z", &ctx).unwrap();
        snooze(&db, &future.id, "2026-07-01T08:00:00Z", &ctx).unwrap();

        // 08:30 BST is 07:30 UTC: only the first snooze has expired. A
        // lexical comparison wakes both because "08:00Z" < "08:30+01".
        assert_eq!(
            wake_expired_snoozes(
                &db,
                "2026-07-01T08:30:00+01:00",
                &EventCtx::system("wake-test")
            )
            .unwrap(),
            1
        );
        db.with_conn(|c| {
            let expired_status: String = c.query_row(
                "SELECT status_v2 FROM tasks WHERE id=?1",
                [&expired.id],
                |r| r.get(0),
            )?;
            let future_status: String = c.query_row(
                "SELECT status_v2 FROM tasks WHERE id=?1",
                [&future.id],
                |r| r.get(0),
            )?;
            assert_eq!(expired_status, "todo");
            assert_eq!(future_status, "snoozed");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn wake_expired_snoozes_preserves_operator_date_semantics() {
        let (_dir, db) = fresh_db();
        let ctx = EventCtx::test();
        let task = create(&db, NewTask::minimal("wake on date"), &ctx).unwrap();
        snooze(&db, &task.id, "2026-07-01", &ctx).unwrap();

        assert_eq!(
            wake_expired_snoozes(
                &db,
                "2026-06-30T23:30:00+01:00",
                &EventCtx::system("wake-test")
            )
            .unwrap(),
            0,
            "date-only snooze must not wake on the preceding operator date"
        );
        assert_eq!(
            wake_expired_snoozes(
                &db,
                "2026-07-01T00:30:00+01:00",
                &EventCtx::system("wake-test")
            )
            .unwrap(),
            1,
            "date-only snooze expires at operator-local midnight"
        );
    }

    #[test]
    fn wake_expired_snoozes_wakes_unparseable_snooze() {
        // Regression: `julianday('someday')` is NULL, every comparison against
        // it is NULL, so the row matched neither branch and stayed `snoozed`
        // forever — with no timer left that could ever fire it. A snooze we
        // cannot read is a snooze we cannot honour: wake it.
        let (_dir, db) = fresh_db();
        let ctx = EventCtx::test();
        let junk = create(&db, NewTask::minimal("unreadable snooze"), &ctx).unwrap();
        let future = create(&db, NewTask::minimal("real snooze"), &ctx).unwrap();
        snooze(&db, &junk.id, "2026-07-01T07:00:00Z", &ctx).unwrap();
        snooze(&db, &future.id, "2026-07-01T08:00:00Z", &ctx).unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET snoozed_until='someday soon' WHERE id=?1",
                [&junk.id],
            )?;
            Ok(())
        })
        .unwrap();

        assert_eq!(
            wake_expired_snoozes(&db, "2026-07-01T07:30:00Z", &EventCtx::system("wake-test"))
                .unwrap(),
            1,
            "the unparseable snooze must wake"
        );
        db.with_conn(|c| {
            let junk_status: String =
                c.query_row("SELECT status_v2 FROM tasks WHERE id=?1", [&junk.id], |r| {
                    r.get(0)
                })?;
            let future_status: String = c.query_row(
                "SELECT status_v2 FROM tasks WHERE id=?1",
                [&future.id],
                |r| r.get(0),
            )?;
            assert_eq!(junk_status, "todo");
            assert_eq!(
                future_status, "snoozed",
                "a valid future snooze still holds"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn undo_does_not_delete_task_edited_after_create() {
        // Regression: `create → priority edit → undo` walked past the
        // (non-undoable) edit event to `task.created` and DELETED the task,
        // discarding it and all its edits. Undo must leave the task intact and
        // report nothing undoable instead.
        let (_dir, db) = fresh_db();
        let ctx = EventCtx::test();
        let t = create(&db, NewTask::minimal("keep me"), &ctx).unwrap();
        update_priority(&db, &t.id, 4, &ctx).unwrap();

        let res = undo_last(&db, &ctx);
        assert!(
            res.is_err(),
            "a priority edit is not undoable — expected Err, got {:?}",
            res.ok().map(|o| o.description)
        );

        let count: i64 = db
            .with_conn(|c| {
                Ok(
                    c.query_row("SELECT COUNT(*) FROM tasks WHERE id=?1", [&t.id], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(
            count, 1,
            "task must survive undo-after-edit, not be deleted"
        );
    }

    #[test]
    fn undo_still_deletes_a_bare_create() {
        // Guard the intended behaviour: with no edits after the create, undo of
        // a fresh create still deletes it.
        let (_dir, db) = fresh_db();
        let ctx = EventCtx::test();
        let t = create(&db, NewTask::minimal("oops"), &ctx).unwrap();
        let out = undo_last(&db, &ctx).unwrap();
        assert!(out.description.contains("deleted"));

        let count: i64 = db
            .with_conn(|c| {
                Ok(
                    c.query_row("SELECT COUNT(*) FROM tasks WHERE id=?1", [&t.id], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(count, 0, "bare-create undo should delete the task");
    }
}
