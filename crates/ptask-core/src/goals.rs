//! Goal tree — mission ancestry for tasks.
//!
//! Goals form a tree (`G-n`). A task links to a goal directly (`tasks.goal_id`)
//! or inherits its parent task's effective goal by walking `parent_uuid`.
//! Every parent-pointer walk is cycle-safe: stop on a repeated node and cap
//! depth at [`MAX_WALK_DEPTH`]. A corrupted DB must never hang `pt show`.

use crate::dates;
use crate::error::{Error, Result};
use crate::event_log::EventCtx;
use crate::storage::Db;
use crate::tasks::{self, Task};
use rusqlite::OptionalExtension;
use rusqlite::params;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Cap on any parent-pointer walk (task `parent_uuid` or goal `parent_id`).
pub const MAX_WALK_DEPTH: usize = 16;

const STATUSES: &[&str] = &["active", "achieved", "abandoned"];

/// Domain errors for the goal tree.
#[derive(Debug, thiserror::Error)]
pub enum GoalError {
    #[error("goal not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Cycle(String),
}

/// Where a task's effective goal came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalSource {
    Direct,
    Parent,
    None,
}

impl GoalSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Parent => "parent",
            Self::None => "none",
        }
    }
}

/// One row in `goals`.
#[derive(Debug, Clone)]
pub struct Goal {
    pub uuid: String,
    pub seq: i64,
    pub title: String,
    pub why: Option<String>,
    /// Parent row uuid, if any.
    pub parent_id: Option<String>,
    /// Parent human id (`G-n`), if the parent row still exists.
    pub parent: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Goal {
    pub fn g_id(&self) -> String {
        format_g_id(self.seq)
    }

    /// Machine object matching the contract JSON shape.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.g_id(),
            "uuid": self.uuid,
            "title": self.title,
            "why": self.why,
            "parent": self.parent,
            "status": self.status,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }
}

/// A goal plus its depth in the tree (roots are 0).
#[derive(Debug, Clone)]
pub struct GoalListItem {
    pub goal: Goal,
    pub depth: u32,
}

impl GoalListItem {
    pub fn to_json(&self) -> serde_json::Value {
        let mut v = self.goal.to_json();
        v["depth"] = serde_json::json!(self.depth);
        v
    }
}

/// Compact task projection used by `goal show` and `goal orphans`.
#[derive(Debug, Clone)]
pub struct GoalTask {
    pub id: String,
    pub pt_id: Option<String>,
    pub title: String,
    pub status: String,
}

impl GoalTask {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "pt_id": self.pt_id,
            "title": self.title,
            "status": self.status,
        })
    }
}

/// Open/done counts over a goal and all its descendants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rollup {
    pub open: i64,
    pub done: i64,
}

/// `goal show` payload: the goal plus ancestors, children, tasks, rollup.
#[derive(Debug, Clone)]
pub struct GoalShow {
    pub goal: Goal,
    /// Ancestors, nearest first (does not include `goal` itself).
    pub chain: Vec<Goal>,
    pub children: Vec<Goal>,
    /// Tasks whose *effective* goal is this goal (not descendants).
    pub tasks: Vec<GoalTask>,
    pub rollup: Rollup,
}

impl GoalShow {
    pub fn to_json(&self) -> serde_json::Value {
        let mut v = self.goal.to_json();
        v["chain"] = serde_json::json!(self.chain.iter().map(Goal::to_json).collect::<Vec<_>>());
        v["children"] =
            serde_json::json!(self.children.iter().map(Goal::to_json).collect::<Vec<_>>());
        v["tasks"] =
            serde_json::json!(self.tasks.iter().map(GoalTask::to_json).collect::<Vec<_>>());
        v["rollup"] = serde_json::json!({
            "open": self.rollup.open,
            "done": self.rollup.done,
        });
        v
    }
}

/// Direct-or-inherited goal for a task, with the leaf→root chain.
#[derive(Debug, Clone)]
pub struct EffectiveGoal {
    pub source: GoalSource,
    /// Leaf (the linked goal) first, then ancestors toward the mission.
    pub chain: Vec<Goal>,
}

/// One open `depends_on` prerequisite.
#[derive(Debug, Clone)]
pub struct Blocker {
    pub pt_id: String,
    pub title: String,
}

/// Worker-brief inputs for `pt context`.
#[derive(Debug, Clone)]
pub struct TaskContext {
    pub pt_id: Option<String>,
    pub title: String,
    pub description: String,
    pub source: GoalSource,
    /// Root (mission) first, then down to the linked leaf.
    pub why_chain: Vec<Goal>,
    pub blockers: Vec<Blocker>,
}

pub fn format_g_id(n: i64) -> String {
    format!("G-{n}")
}

pub fn parse_g_id(s: &str) -> Option<i64> {
    let rest = s
        .trim()
        .strip_prefix("G-")
        .or_else(|| s.trim().strip_prefix("g-"))?;
    rest.parse::<i64>().ok().filter(|n| *n > 0)
}

/// `[{id, title, why}, ...]` — the `pt show` / MCP `goal_chain` shape.
pub fn chain_json(chain: &[Goal]) -> serde_json::Value {
    serde_json::Value::Array(
        chain
            .iter()
            .map(|g| {
                serde_json::json!({
                    "id": g.g_id(),
                    "title": g.title,
                    "why": g.why,
                })
            })
            .collect(),
    )
}

const SELECT_SQL: &str = "SELECT g.id, g.seq, g.title, g.why, g.parent_id, p.seq,
       g.status, g.created_at, g.updated_at
FROM goals g
LEFT JOIN goals p ON p.id = g.parent_id";

fn map_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Goal> {
    let seq: i64 = r.get(1)?;
    let parent_seq: Option<i64> = r.get(5)?;
    Ok(Goal {
        uuid: r.get(0)?,
        seq,
        title: r.get(2)?,
        why: r.get(3)?,
        parent_id: r.get(4)?,
        parent: parent_seq.map(format_g_id),
        status: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

fn local_event_uuid(ctx: &EventCtx) -> String {
    ctx.event_uuid
        .clone()
        .unwrap_or_else(|| format!("local:{}", Uuid::new_v4()))
}

fn record_event(
    tx: &rusqlite::Connection,
    ctx: &EventCtx,
    entity_uuid: &str,
    event_type: &str,
    extra: serde_json::Value,
) -> Result<()> {
    let uuid = local_event_uuid(ctx);
    crate::event_log::record_in_conn(tx, &uuid, Some(entity_uuid), event_type, &extra, ctx)
        .map(|_| ())
}

fn now_iso() -> Result<String> {
    Ok(dates::format_iso(&dates::now_in_operator_tz()?))
}

fn load_by_uuid_conn(conn: &rusqlite::Connection, uuid: &str) -> Result<Option<Goal>> {
    Ok(conn
        .query_row(&format!("{SELECT_SQL} WHERE g.id = ?1"), [uuid], map_row)
        .optional()?)
}

fn load_by_seq_conn(conn: &rusqlite::Connection, seq: i64) -> Result<Option<Goal>> {
    Ok(conn
        .query_row(&format!("{SELECT_SQL} WHERE g.seq = ?1"), [seq], map_row)
        .optional()?)
}

fn get_in_conn(conn: &rusqlite::Connection, id: &str) -> Result<Goal> {
    let found = if let Some(seq) = parse_g_id(id) {
        load_by_seq_conn(conn, seq)?
    } else {
        load_by_uuid_conn(conn, id.trim())?
    };
    found.ok_or_else(|| Error::Goal(GoalError::NotFound(id.trim().to_string())))
}

/// Resolve `G-n` or the row uuid.
pub fn get(db: &Db, id: &str) -> Result<Goal> {
    let conn = db.get()?;
    get_in_conn(&conn, id)
}

fn load_all_conn(conn: &rusqlite::Connection) -> Result<Vec<Goal>> {
    let mut stmt = conn.prepare(&format!("{SELECT_SQL} ORDER BY g.seq ASC"))?;
    let rows = stmt.query_map([], map_row)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn parent_uuid_of(conn: &rusqlite::Connection, uuid: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT parent_id FROM goals WHERE id = ?1", [uuid], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten())
}

/// Walk `parent_id` from `leaf` toward the root. Stops on a repeated node
/// or at [`MAX_WALK_DEPTH`]. The leaf is the first element when present.
fn goal_chain_from_conn(conn: &rusqlite::Connection, leaf_uuid: &str) -> Result<Vec<Goal>> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut current = Some(leaf_uuid.to_string());
    while let Some(id) = current {
        if chain.len() >= MAX_WALK_DEPTH {
            break;
        }
        if !seen.insert(id.clone()) {
            break;
        }
        match load_by_uuid_conn(conn, &id)? {
            Some(g) => {
                current = g.parent_id.clone();
                chain.push(g);
            }
            None => break,
        }
    }
    Ok(chain)
}

/// Would setting `goal_uuid`'s parent to `new_parent_uuid` close a cycle?
fn would_cycle(
    conn: &rusqlite::Connection,
    goal_uuid: &str,
    new_parent_uuid: &str,
) -> Result<bool> {
    if goal_uuid == new_parent_uuid {
        return Ok(true);
    }
    let mut seen = HashSet::new();
    let mut current = Some(new_parent_uuid.to_string());
    let mut depth = 0usize;
    while let Some(id) = current {
        if depth >= MAX_WALK_DEPTH {
            break;
        }
        if id == goal_uuid {
            return Ok(true);
        }
        if !seen.insert(id.clone()) {
            break;
        }
        current = parent_uuid_of(conn, &id)?;
        depth += 1;
    }
    Ok(false)
}

struct TaskNav {
    id: String,
    pt_id: Option<String>,
    title: String,
    status: String,
    parent_uuid: Option<String>,
    goal_id: Option<String>,
}

fn load_task_nav(conn: &rusqlite::Connection) -> Result<Vec<TaskNav>> {
    let mut stmt =
        conn.prepare("SELECT id, pt_id, title, status_v2, parent_uuid, goal_id FROM tasks")?;
    let rows = stmt.query_map([], |r| {
        Ok(TaskNav {
            id: r.get(0)?,
            pt_id: r.get(1)?,
            title: r.get(2)?,
            status: r.get(3)?,
            parent_uuid: r.get(4)?,
            goal_id: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn task_fields(
    conn: &rusqlite::Connection,
    task_uuid: &str,
) -> Result<Option<(Option<String>, Option<String>)>> {
    Ok(conn
        .query_row(
            "SELECT goal_id, parent_uuid FROM tasks WHERE id = ?1",
            [task_uuid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// Direct link, else walk `parent_uuid` until a linked ancestor (or none).
fn find_leaf_goal(
    conn: &rusqlite::Connection,
    task_uuid: &str,
) -> Result<(GoalSource, Option<String>)> {
    let mut seen = HashSet::new();
    let mut current = Some(task_uuid.to_string());
    let mut depth = 0usize;
    let mut first = true;
    while let Some(id) = current {
        if depth >= MAX_WALK_DEPTH || !seen.insert(id.clone()) {
            break;
        }
        match task_fields(conn, &id)? {
            Some((goal_id, parent_uuid)) => {
                if let Some(g) = goal_id {
                    let source = if first {
                        GoalSource::Direct
                    } else {
                        GoalSource::Parent
                    };
                    return Ok((source, Some(g)));
                }
                first = false;
                current = parent_uuid;
                depth += 1;
            }
            None => break,
        }
    }
    Ok((GoalSource::None, None))
}

fn find_leaf_goal_in_mem(
    rows: &HashMap<String, &TaskNav>,
    task_uuid: &str,
) -> (GoalSource, Option<String>) {
    let mut seen = HashSet::new();
    let mut current = Some(task_uuid.to_string());
    let mut depth = 0usize;
    let mut first = true;
    while let Some(id) = current {
        if depth >= MAX_WALK_DEPTH || !seen.insert(id.clone()) {
            break;
        }
        let Some(row) = rows.get(&id) else {
            break;
        };
        if let Some(g) = row.goal_id.as_ref() {
            let source = if first {
                GoalSource::Direct
            } else {
                GoalSource::Parent
            };
            return (source, Some(g.clone()));
        }
        first = false;
        current = row.parent_uuid.clone();
        depth += 1;
    }
    (GoalSource::None, None)
}

fn is_open_status(status: &str) -> bool {
    status != "done" && status != "dismissed"
}

/// Direct-or-inherited goal chain for a task, leaf→root.
pub fn effective_goal(db: &Db, task_uuid: &str) -> Result<EffectiveGoal> {
    let conn = db.get()?;
    let (source, leaf) = find_leaf_goal(&conn, task_uuid)?;
    let chain = match leaf {
        Some(uuid) => goal_chain_from_conn(&conn, &uuid)?,
        None => Vec::new(),
    };
    Ok(EffectiveGoal { source, chain })
}

fn subtree_uuids(all: &[Goal], root: &str) -> HashSet<String> {
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for g in all {
        if let Some(p) = &g.parent_id {
            children.entry(p.clone()).or_default().push(g.uuid.clone());
        }
    }
    let mut out = HashSet::new();
    let mut stack = vec![root.to_string()];
    while let Some(id) = stack.pop() {
        if !out.insert(id.clone()) {
            continue;
        }
        if let Some(ch) = children.get(&id) {
            stack.extend(ch.iter().cloned());
        }
    }
    out
}

/// Create a goal. `parent` is `G-n` or a uuid.
pub fn add(
    db: &Db,
    title: &str,
    why: Option<&str>,
    parent: Option<&str>,
    ctx: &EventCtx,
) -> Result<Goal> {
    let title = title.trim();
    if title.is_empty() {
        return Err(Error::Goal(GoalError::Invalid(
            "goal title must not be empty".into(),
        )));
    }
    let why = why
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let parent_uuid = match parent {
        Some(p) if !p.trim().is_empty() => Some(get(db, p)?.uuid),
        _ => None,
    };
    let now = now_iso()?;
    let uuid = Uuid::new_v4().to_string();
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let seq: i64 = tx.query_row(
        "UPDATE pt_counters SET value = value + 1 WHERE name='goal_id' RETURNING value",
        [],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO goals (id, seq, title, why, parent_id, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?6)",
        params![uuid, seq, title, why, parent_uuid, now],
    )?;
    record_event(
        &tx,
        ctx,
        &uuid,
        "goal.created",
        serde_json::json!({"goal_id": format_g_id(seq), "title": title}),
    )?;
    tx.commit()?;
    get(db, &uuid)
}

/// Tree-order listing: parent before children, siblings by seq.
///
/// Default (`include_inactive = false`) returns only `active` rows; depth is
/// still the real tree depth (an active child of an achieved parent keeps
/// depth 2). `--all` includes achieved and abandoned.
pub fn list(db: &Db, include_inactive: bool) -> Result<Vec<GoalListItem>> {
    let conn = db.get()?;
    let all = load_all_conn(&conn)?;
    let by_uuid: HashMap<String, Goal> = all.iter().cloned().map(|g| (g.uuid.clone(), g)).collect();
    let mut children: HashMap<Option<String>, Vec<String>> = HashMap::new();
    for g in &all {
        children
            .entry(g.parent_id.clone())
            .or_default()
            .push(g.uuid.clone());
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    if let Some(roots) = children.get(&None) {
        for id in roots {
            walk_list_node(
                id,
                0,
                include_inactive,
                &by_uuid,
                &children,
                &mut seen,
                &mut out,
            );
        }
    }
    // Cyclic or parent-missing components: still terminate, emit leftovers.
    for g in &all {
        if !seen.contains(&g.uuid) {
            walk_list_node(
                &g.uuid,
                0,
                include_inactive,
                &by_uuid,
                &children,
                &mut seen,
                &mut out,
            );
        }
    }
    Ok(out)
}

fn walk_list_node(
    id: &str,
    depth: u32,
    include_inactive: bool,
    by_uuid: &HashMap<String, Goal>,
    children: &HashMap<Option<String>, Vec<String>>,
    seen: &mut HashSet<String>,
    out: &mut Vec<GoalListItem>,
) {
    if !seen.insert(id.to_string()) {
        return;
    }
    let Some(g) = by_uuid.get(id) else {
        return;
    };
    if include_inactive || g.status == "active" {
        out.push(GoalListItem {
            goal: g.clone(),
            depth,
        });
    }
    if let Some(ch) = children.get(&Some(id.to_string())) {
        for child in ch {
            walk_list_node(
                child,
                depth.saturating_add(1),
                include_inactive,
                by_uuid,
                children,
                seen,
                out,
            );
        }
    }
}

/// Re-parent `id` under `parent_id`. Rejects self and any cycle.
pub fn set_parent(db: &Db, id: &str, parent_id: &str, ctx: &EventCtx) -> Result<Goal> {
    let goal = get(db, id)?;
    let parent = get(db, parent_id)?;
    if goal.uuid == parent.uuid {
        return Err(Error::Goal(GoalError::Cycle(
            "cannot set a goal as its own parent".into(),
        )));
    }
    {
        let conn = db.get()?;
        if would_cycle(&conn, &goal.uuid, &parent.uuid)? {
            return Err(Error::Goal(GoalError::Cycle(
                "cannot set parent: that would create a cycle".into(),
            )));
        }
    }
    let now = now_iso()?;
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE goals SET parent_id = ?1, updated_at = ?2 WHERE id = ?3",
        params![parent.uuid, now, goal.uuid],
    )?;
    record_event(
        &tx,
        ctx,
        &goal.uuid,
        "goal.updated",
        serde_json::json!({
            "goal_id": goal.g_id(),
            "parent": parent.g_id(),
        }),
    )?;
    tx.commit()?;
    get(db, &goal.uuid)
}

pub fn set_status(db: &Db, id: &str, status: &str, ctx: &EventCtx) -> Result<Goal> {
    if !STATUSES.contains(&status) {
        return Err(Error::Goal(GoalError::Invalid(format!(
            "invalid goal status {status:?}; expected one of {}",
            STATUSES.join(", ")
        ))));
    }
    let goal = get(db, id)?;
    let now = now_iso()?;
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE goals SET status = ?1, updated_at = ?2 WHERE id = ?3",
        params![status, now, goal.uuid],
    )?;
    record_event(
        &tx,
        ctx,
        &goal.uuid,
        "goal.updated",
        serde_json::json!({"goal_id": goal.g_id(), "status": status}),
    )?;
    tx.commit()?;
    get(db, &goal.uuid)
}

pub fn mark_achieved(db: &Db, id: &str, ctx: &EventCtx) -> Result<Goal> {
    set_status(db, id, "achieved", ctx)
}

pub fn mark_abandoned(db: &Db, id: &str, ctx: &EventCtx) -> Result<Goal> {
    set_status(db, id, "abandoned", ctx)
}

/// Direct-link `task` to `goal`. Replaces any previous direct link.
pub fn link(db: &Db, task_query: &str, goal_id: &str, ctx: &EventCtx) -> Result<Task> {
    let task = tasks::resolve_for_lookup(db, task_query, true)?;
    let goal = get(db, goal_id)?;
    let conn = db.get()?;
    let old: Option<String> = conn
        .query_row("SELECT goal_id FROM tasks WHERE id = ?1", [&task.id], |r| {
            r.get(0)
        })
        .optional()?
        .flatten();
    drop(conn);
    if old.as_deref() == Some(goal.uuid.as_str()) {
        return Ok(task);
    }
    let now = now_iso()?;
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE tasks SET goal_id = ?1, updated_at = ?2 WHERE id = ?3",
        params![goal.uuid, now, task.id],
    )?;
    if let Some(old_uuid) = old {
        record_event(
            &tx,
            ctx,
            &task.id,
            "task.goal_unlinked",
            serde_json::json!({
                "pt_id": task.pt_id,
                "goal_uuid": old_uuid,
            }),
        )?;
    }
    record_event(
        &tx,
        ctx,
        &task.id,
        "task.goal_linked",
        serde_json::json!({
            "pt_id": task.pt_id,
            "goal_id": goal.g_id(),
            "goal_uuid": goal.uuid,
        }),
    )?;
    tx.commit()?;
    Ok(task)
}

/// Clear a task's direct goal link. Inheritance via `parent_uuid` remains.
pub fn unlink(db: &Db, task_query: &str, ctx: &EventCtx) -> Result<Task> {
    let task = tasks::resolve_for_lookup(db, task_query, true)?;
    let conn = db.get()?;
    let old: Option<String> = conn
        .query_row("SELECT goal_id FROM tasks WHERE id = ?1", [&task.id], |r| {
            r.get(0)
        })
        .optional()?
        .flatten();
    drop(conn);
    if old.is_none() {
        return Ok(task);
    }
    let now = now_iso()?;
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE tasks SET goal_id = NULL, updated_at = ?1 WHERE id = ?2",
        params![now, task.id],
    )?;
    record_event(
        &tx,
        ctx,
        &task.id,
        "task.goal_unlinked",
        serde_json::json!({
            "pt_id": task.pt_id,
            "goal_uuid": old,
        }),
    )?;
    tx.commit()?;
    Ok(task)
}

/// Full `goal show` payload.
pub fn show(db: &Db, id: &str) -> Result<GoalShow> {
    let conn = db.get()?;
    let goal = get_in_conn(&conn, id)?;
    let full = goal_chain_from_conn(&conn, &goal.uuid)?;
    let chain = full.into_iter().skip(1).collect::<Vec<_>>();
    let mut child_stmt = conn.prepare(&format!(
        "{SELECT_SQL} WHERE g.parent_id = ?1 ORDER BY g.seq ASC"
    ))?;
    let children = child_stmt
        .query_map([&goal.uuid], map_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(child_stmt);

    let all_goals = load_all_conn(&conn)?;
    let subtree = subtree_uuids(&all_goals, &goal.uuid);
    let nav = load_task_nav(&conn)?;
    let by_id: HashMap<String, &TaskNav> = nav.iter().map(|t| (t.id.clone(), t)).collect();

    let mut tasks = Vec::new();
    let mut open = 0i64;
    let mut done = 0i64;
    for t in &nav {
        let (_source, leaf) = find_leaf_goal_in_mem(&by_id, &t.id);
        let Some(leaf) = leaf else {
            continue;
        };
        if leaf == goal.uuid {
            tasks.push(GoalTask {
                id: t.id.clone(),
                pt_id: t.pt_id.clone(),
                title: t.title.clone(),
                status: t.status.clone(),
            });
        }
        if subtree.contains(&leaf) {
            if is_open_status(&t.status) {
                open += 1;
            } else {
                done += 1;
            }
        }
    }
    Ok(GoalShow {
        goal,
        chain,
        children,
        tasks,
        rollup: Rollup { open, done },
    })
}

/// Open tasks with no effective goal (no direct link and no inherited one).
pub fn orphans(db: &Db) -> Result<Vec<GoalTask>> {
    let conn = db.get()?;
    let nav = load_task_nav(&conn)?;
    let by_id: HashMap<String, &TaskNav> = nav.iter().map(|t| (t.id.clone(), t)).collect();
    let mut out = Vec::new();
    for t in &nav {
        if !is_open_status(&t.status) {
            continue;
        }
        let (source, _) = find_leaf_goal_in_mem(&by_id, &t.id);
        if source == GoalSource::None {
            out.push(GoalTask {
                id: t.id.clone(),
                pt_id: t.pt_id.clone(),
                title: t.title.clone(),
                status: t.status.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.pt_id.cmp(&b.pt_id).then(a.id.cmp(&b.id)));
    Ok(out)
}

fn open_blockers_conn(conn: &rusqlite::Connection, task_uuid: &str) -> Result<Vec<Blocker>> {
    let mut stmt = conn.prepare(
        "SELECT d.pt_id, d.id, d.title FROM task_links l
         JOIN tasks d ON d.id = l.to_uuid
         WHERE l.from_uuid = ?1 AND l.kind = 'depends_on'
           AND d.status_v2 NOT IN ('done', 'dismissed')
         ORDER BY d.created_at",
    )?;
    let rows = stmt
        .query_map([task_uuid], |r| {
            let pt: Option<String> = r.get(0)?;
            let id: String = r.get(1)?;
            let title: String = r.get(2)?;
            Ok(Blocker {
                pt_id: pt.unwrap_or(id),
                title,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Inputs for a worker-mission brief.
pub fn task_context(db: &Db, task: &Task) -> Result<TaskContext> {
    let eg = effective_goal(db, &task.id)?;
    let mut why_chain = eg.chain.clone();
    why_chain.reverse();
    let conn = db.get()?;
    let blockers = open_blockers_conn(&conn, &task.id)?;
    Ok(TaskContext {
        pt_id: task.pt_id.clone(),
        title: task.title.clone(),
        description: task.description.clone(),
        source: eg.source,
        why_chain,
        blockers,
    })
}

/// Markdown worker brief: title, description, Why (root→leaf), blockers.
pub fn context_markdown(db: &Db, task: &Task) -> Result<String> {
    let ctx = task_context(db, task)?;
    let mut md = String::new();
    md.push_str("# ");
    md.push_str(&ctx.title);
    md.push('\n');
    if !ctx.description.trim().is_empty() {
        md.push('\n');
        md.push_str(ctx.description.trim());
        md.push('\n');
    }
    if !ctx.why_chain.is_empty() {
        md.push_str("\n## Why\n\n");
        for g in &ctx.why_chain {
            match g.why.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(why) => {
                    md.push_str(&format!("- **{}**: {}\n", g.title, why));
                }
                None => {
                    md.push_str(&format!("- **{}**\n", g.title));
                }
            }
        }
    }
    if !ctx.blockers.is_empty() {
        md.push_str("\n## Blockers\n\n");
        for b in &ctx.blockers {
            md.push_str(&format!("- {}: {}\n", b.pt_id, b.title));
        }
    }
    Ok(md)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::EventCtx;
    use crate::storage::Db;
    use crate::tasks::NewTask;

    fn fresh() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("t.db")).unwrap();
        (dir, db)
    }

    fn ctx() -> EventCtx {
        EventCtx::test()
    }

    fn add_task(db: &Db, title: &str) -> Task {
        tasks::create(db, NewTask::minimal(title), &ctx()).unwrap()
    }

    #[test]
    fn tree_order_parent_before_children_siblings_by_seq() {
        let (_dir, db) = fresh();
        let g1 = add(&db, "mission", Some("runway"), None, &ctx()).unwrap();
        let g2 = add(&db, "revenue", Some("burn"), Some(&g1.g_id()), &ctx()).unwrap();
        let g3 = add(&db, "argus", Some("fast"), Some(&g2.g_id()), &ctx()).unwrap();
        let g4 = add(&db, "fleet", None, Some(&g1.g_id()), &ctx()).unwrap();
        let items = list(&db, false).unwrap();
        let ids: Vec<String> = items.iter().map(|i| i.goal.g_id()).collect();
        assert_eq!(ids, vec![g1.g_id(), g2.g_id(), g3.g_id(), g4.g_id()]);
        let depth: HashMap<String, u32> = items.iter().map(|i| (i.goal.g_id(), i.depth)).collect();
        assert_eq!(depth[&g1.g_id()], 0);
        assert_eq!(depth[&g2.g_id()], 1);
        assert_eq!(depth[&g3.g_id()], 2);
        assert_eq!(depth[&g4.g_id()], 1);
    }

    #[test]
    fn set_parent_rejects_self_and_cycle() {
        let (_dir, db) = fresh();
        let g1 = add(&db, "a", None, None, &ctx()).unwrap();
        let g2 = add(&db, "b", None, Some(&g1.g_id()), &ctx()).unwrap();
        let g3 = add(&db, "c", None, Some(&g2.g_id()), &ctx()).unwrap();
        let err = set_parent(&db, &g2.g_id(), &g2.g_id(), &ctx()).unwrap_err();
        assert!(matches!(err, Error::Goal(GoalError::Cycle(_))));
        let err = set_parent(&db, &g1.g_id(), &g3.g_id(), &ctx()).unwrap_err();
        assert!(matches!(err, Error::Goal(GoalError::Cycle(_))));
        set_parent(&db, &g3.g_id(), &g1.g_id(), &ctx()).unwrap();
        assert_eq!(get(&db, &g3.g_id()).unwrap().parent, Some(g1.g_id()));
    }

    #[test]
    fn subtask_inherits_until_direct_link() {
        let (_dir, db) = fresh();
        let g = add(&db, "leaf", None, None, &ctx()).unwrap();
        let parent = add_task(&db, "parent");
        let child = add_task(&db, "child");
        link(&db, parent.pt_id.as_deref().unwrap(), &g.g_id(), &ctx()).unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET parent_uuid = ?1 WHERE id = ?2",
                params![parent.id, child.id],
            )?;
            Ok(())
        })
        .unwrap();
        let eg = effective_goal(&db, &child.id).unwrap();
        assert_eq!(eg.source, GoalSource::Parent);
        assert_eq!(eg.chain[0].g_id(), g.g_id());
        let g2 = add(&db, "other", None, None, &ctx()).unwrap();
        link(&db, child.pt_id.as_deref().unwrap(), &g2.g_id(), &ctx()).unwrap();
        let eg = effective_goal(&db, &child.id).unwrap();
        assert_eq!(eg.source, GoalSource::Direct);
        assert_eq!(eg.chain[0].g_id(), g2.g_id());
        unlink(&db, child.pt_id.as_deref().unwrap(), &ctx()).unwrap();
        let eg = effective_goal(&db, &child.id).unwrap();
        assert_eq!(eg.source, GoalSource::Parent);
    }

    #[test]
    fn corrupt_goal_cycle_does_not_repeat() {
        let (_dir, db) = fresh();
        let g1 = add(&db, "a", None, None, &ctx()).unwrap();
        let g2 = add(&db, "b", None, Some(&g1.g_id()), &ctx()).unwrap();
        let t = add_task(&db, "y");
        link(&db, t.pt_id.as_deref().unwrap(), &g2.g_id(), &ctx()).unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE goals SET parent_id = ?1 WHERE id = ?2",
                params![g2.uuid, g1.uuid],
            )?;
            Ok(())
        })
        .unwrap();
        let eg = effective_goal(&db, &t.id).unwrap();
        assert!((1..=MAX_WALK_DEPTH).contains(&eg.chain.len()));
        let ids: Vec<String> = eg.chain.iter().map(|g| g.uuid.clone()).collect();
        let unique: HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }

    #[test]
    fn rollup_counts_descendant_tasks() {
        let (_dir, db) = fresh();
        let g1 = add(&db, "mission", None, None, &ctx()).unwrap();
        let g2 = add(&db, "rev", None, Some(&g1.g_id()), &ctx()).unwrap();
        let g3 = add(&db, "argus", None, Some(&g2.g_id()), &ctx()).unwrap();
        let g4 = add(&db, "fleet", None, Some(&g1.g_id()), &ctx()).unwrap();
        let a = add_task(&db, "a");
        let b = add_task(&db, "b");
        let c = add_task(&db, "c");
        link(&db, a.pt_id.as_deref().unwrap(), &g3.g_id(), &ctx()).unwrap();
        link(&db, b.pt_id.as_deref().unwrap(), &g3.g_id(), &ctx()).unwrap();
        link(&db, c.pt_id.as_deref().unwrap(), &g4.g_id(), &ctx()).unwrap();
        tasks::mark_done(&db, &b, &ctx()).unwrap();
        let s3 = show(&db, &g3.g_id()).unwrap();
        let mut pts: Vec<String> = s3.tasks.iter().filter_map(|t| t.pt_id.clone()).collect();
        pts.sort();
        let mut expect = vec![a.pt_id.clone().unwrap(), b.pt_id.clone().unwrap()];
        expect.sort();
        assert_eq!(pts, expect);
        assert_eq!(
            s3.chain.iter().map(|g| g.g_id()).collect::<Vec<_>>(),
            vec![g2.g_id(), g1.g_id()]
        );
        let s1 = show(&db, &g1.g_id()).unwrap();
        let mut kids: Vec<String> = s1.children.iter().map(|g| g.g_id()).collect();
        kids.sort();
        let mut expect_kids = vec![g2.g_id(), g4.g_id()];
        expect_kids.sort();
        assert_eq!(kids, expect_kids);
        assert_eq!(s1.rollup, Rollup { open: 2, done: 1 });
    }

    #[test]
    fn orphans_skip_linked_closed_and_inherited() {
        let (_dir, db) = fresh();
        let g = add(&db, "g", None, None, &ctx()).unwrap();
        let linked = add_task(&db, "linked");
        link(&db, linked.pt_id.as_deref().unwrap(), &g.g_id(), &ctx()).unwrap();
        let loose = add_task(&db, "loose");
        let closed = add_task(&db, "closed");
        tasks::mark_done(&db, &closed, &ctx()).unwrap();
        let child = add_task(&db, "child");
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET parent_uuid = ?1 WHERE id = ?2",
                params![linked.id, child.id],
            )?;
            Ok(())
        })
        .unwrap();
        let ids: Vec<Option<String>> = orphans(&db).unwrap().into_iter().map(|t| t.pt_id).collect();
        assert!(ids.contains(&loose.pt_id));
        assert!(!ids.contains(&linked.pt_id));
        assert!(!ids.contains(&closed.pt_id));
        assert!(!ids.contains(&child.pt_id));
    }

    #[test]
    fn unknown_parent_is_rejected() {
        let (_dir, db) = fresh();
        let err = add(&db, "x", None, Some("G-999"), &ctx()).unwrap_err();
        assert!(matches!(err, Error::Goal(GoalError::NotFound(_))));
    }

    #[test]
    fn json_shape_has_g_id_and_null_parent() {
        let (_dir, db) = fresh();
        let g = add(&db, "mission", Some("why text"), None, &ctx()).unwrap();
        let v = g.to_json();
        assert_eq!(v["id"], "G-1");
        assert_eq!(v["parent"], serde_json::Value::Null);
        assert_eq!(v["why"], "why text");
        assert_eq!(v["status"], "active");
        assert!(v.get("uuid").is_some());
    }
}
