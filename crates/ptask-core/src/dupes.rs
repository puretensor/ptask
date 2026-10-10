//! Duplicate detection and merge for tasks people and agents file.
//!
//! Distill deduplicates machine signals before they become tasks; nothing
//! did the same for tasks filed by hand or by agents, so backlog scans kept
//! finding pairs and trios of the same work, and a closing pass could leave
//! more open tasks than it closed. Two parts:
//!
//! - [`similar`] finds likely duplicates of a title among open tasks and
//!   tasks closed in the last [`RECENT_CLOSED_DAYS`] days (re-filing work
//!   that was just finished or just declined is the same mistake). It is
//!   lexical, deterministic and local: a Dice coefficient over normalised
//!   title words, needing at least two shared words. No model, no network.
//!   `pt add` / MCP `task_add` report candidates; `--unique` /
//!   `skip_if_duplicate` refuse to create one.
//! - [`merge`] closes one task into another in one transaction: the
//!   duplicate is dismissed with a `task_links.kind='duplicate_of'` row
//!   (the schema's merge relation since V012), every task that depended on
//!   it now depends on the canonical one (dismissing a prerequisite
//!   satisfies it, so without the move its dependents would unblock), its
//!   own prerequisites, labels, recurrence, goal, `discovered_from` links,
//!   subtasks and unchecked acceptance criteria carry over, and the
//!   canonical task takes the higher priority and, when it has none, the
//!   duplicate's deadline. A merge into a closed target that would unblock
//!   open dependents, or drop unchecked criteria, is refused. `pt undo` of
//!   a merge reverses those moves.

use crate::error::{Error, Result};
use crate::event_log::EventCtx;
use crate::storage::Db;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};

/// Similarity at or above which two titles are reported as likely
/// duplicates.
pub const DEFAULT_THRESHOLD: f64 = 0.6;

/// Similarity at or above which `--unique` / `skip_if_duplicate` refuse to
/// file a task. Stricter than reporting: related work ("prepare the report"
/// vs "submit the assessment") shares words and deserves a mention, not a
/// refusal. Identifier-like words (numbers, dates, hashes, hosts) that
/// differ also keep a high-scoring pair report-only: "phase 2" is not
/// "phase 3".
pub const REFUSE_THRESHOLD: f64 = 0.75;

/// True when `cands` holds one close enough to refuse a filing of `title`:
/// score at least [`REFUSE_THRESHOLD`] and the same identifier-like words.
pub fn refuses(title: &str, cands: &[Candidate]) -> bool {
    let ids = identifier_tokens(title);
    cands
        .iter()
        .any(|c| c.score >= REFUSE_THRESHOLD && identifier_tokens(&c.title) == ids)
}

/// Tokens that distinguish otherwise-similar titles: any alphanumeric run
/// that contains a digit. "phase 2" vs "phase 3", "web-01" vs "web-02",
/// differing dates and hashes all disagree here; a rewording of the same
/// title does not.
fn identifier_tokens(title: &str) -> BTreeSet<String> {
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && w.chars().any(|c| c.is_ascii_digit()))
        .map(str::to_string)
        .collect()
}

/// Closed tasks this recent still count as candidates.
pub const RECENT_CLOSED_DAYS: i64 = 14;

/// Words that carry no identity in a task title.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "into", "is", "it", "of",
    "on", "or", "so", "the", "to", "via", "with", "we", "our", "this", "that", "pt", "task",
    "todo", "again", "please", "need", "needs", "should",
];

/// Normalised identity words of a title: lowercase alphanumeric runs of two
/// or more characters (a lone digit is kept: "phase 2" ≠ "phase 3"),
/// stopwords and `PT-N` references dropped, a plural `s` folded.
pub fn tokens(title: &str) -> BTreeSet<String> {
    let lower = title.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        // "PT-42" splits into "pt" + "42": a reference, not identity.
        if w == "pt"
            && words
                .get(i + 1)
                .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
        {
            i += 2;
            continue;
        }
        i += 1;
        let numeric = w.chars().all(|c| c.is_ascii_digit());
        if (!numeric && w.chars().count() < 2) || STOPWORDS.contains(&w) {
            continue;
        }
        // Plural `s` only: not "ss" (access), "us" (plus, status), "is" (basis).
        let folded = if w.len() > 3
            && w.ends_with('s')
            && !w.ends_with("ss")
            && !w.ends_with("us")
            && !w.ends_with("is")
        {
            &w[..w.len() - 1]
        } else {
            w
        };
        out.insert(folded.to_string());
    }
    out
}

/// Dice coefficient of two token sets; 0 unless they share at least two
/// words (one shared word is coincidence, not identity).
pub fn similarity(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let shared = a.intersection(b).count();
    if shared < 2 {
        return 0.0;
    }
    (2 * shared) as f64 / (a.len() + b.len()) as f64
}

/// One likely duplicate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    pub task_uuid: String,
    pub pt_id: Option<String>,
    pub title: String,
    pub status: String,
    /// Similarity, 0..=1, rounded to two places.
    pub score: f64,
}

/// A pair of open tasks that look like the same work.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pair {
    pub score: f64,
    pub a: Candidate,
    pub b: Candidate,
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

struct Row {
    id: String,
    pt_id: Option<String>,
    title: String,
    status: String,
    created_at: String,
}

/// Open tasks plus, with `recent_closed`, tasks done or dismissed in the
/// last [`RECENT_CLOSED_DAYS`] days. A task already merged away (a
/// `duplicate_of` link) is never a candidate: its canonical task is.
fn pool(conn: &rusqlite::Connection, recent_closed: bool) -> Result<Vec<Row>> {
    let closed = if recent_closed {
        format!(
            "OR (t.status_v2 IN ('done','dismissed')
                 AND julianday(t.updated_at) >= julianday('now', '-{RECENT_CLOSED_DAYS} days'))"
        )
    } else {
        String::new()
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT t.id, t.pt_id, t.title, t.status_v2, t.created_at FROM tasks t
          WHERE (t.status_v2 NOT IN ('done','dismissed') {closed})
            AND NOT EXISTS (
                SELECT 1 FROM task_links l
                 WHERE l.from_uuid = t.id AND l.kind = 'duplicate_of')"
    ))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Row {
                id: r.get(0)?,
                pt_id: r.get(1)?,
                title: r.get(2)?,
                status: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn candidate(r: &Row, score: f64) -> Candidate {
    Candidate {
        task_uuid: r.id.clone(),
        pt_id: r.pt_id.clone(),
        title: r.title.clone(),
        status: r.status.clone(),
        score: round2(score),
    }
}

/// Likely duplicates of `title` (best first, at most `limit`), among open
/// tasks and tasks closed in the last [`RECENT_CLOSED_DAYS`] days.
/// `exclude` leaves one task out (the task itself).
pub fn similar(
    db: &Db,
    title: &str,
    exclude: Option<&str>,
    threshold: f64,
    limit: usize,
) -> Result<Vec<Candidate>> {
    let probe = tokens(title);
    if probe.len() < 2 {
        return Ok(Vec::new());
    }
    let conn = db.get()?;
    let mut out: Vec<Candidate> = pool(&conn, true)?
        .iter()
        .filter(|r| Some(r.id.as_str()) != exclude)
        .filter_map(|r| {
            let s = similarity(&probe, &tokens(&r.title));
            (s >= threshold).then(|| candidate(r, s))
        })
        .collect();
    // Best match first; open work before closed at equal score.
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| is_closed(&a.status).cmp(&is_closed(&b.status)))
    });
    out.truncate(limit);
    Ok(out)
}

fn is_closed(status: &str) -> bool {
    matches!(status, "done" | "dismissed")
}

/// Pairs of open tasks that look like the same work, best first. The newer
/// task of each pair is `b`: usually the one to merge into `a`.
pub fn pairs(db: &Db, threshold: f64, limit: usize) -> Result<Vec<Pair>> {
    let conn = db.get()?;
    let mut rows = pool(&conn, false)?;
    rows.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    let toks: Vec<BTreeSet<String>> = rows.iter().map(|r| tokens(&r.title)).collect();
    // Inverted index: only pairs sharing at least two words can score, so
    // count shared words per pair through the index instead of comparing
    // every pair (a real backlog's vocabulary is wide; most pairs share none).
    let mut index: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, t) in toks.iter().enumerate() {
        if t.len() < 2 {
            continue;
        }
        for w in t {
            index.entry(w.as_str()).or_default().push(i);
        }
    }
    let mut out = Vec::new();
    let mut shared: HashMap<usize, usize> = HashMap::new();
    for i in 0..rows.len() {
        if toks[i].len() < 2 {
            continue;
        }
        shared.clear();
        for w in &toks[i] {
            for &j in index.get(w.as_str()).into_iter().flatten() {
                if j > i {
                    *shared.entry(j).or_default() += 1;
                }
            }
        }
        for (&j, &n) in &shared {
            if n < 2 {
                continue;
            }
            let s = similarity(&toks[i], &toks[j]);
            if s >= threshold {
                out.push(Pair {
                    score: round2(s),
                    a: candidate(&rows[i], s),
                    b: candidate(&rows[j], s),
                });
            }
        }
    }
    // Best first; ties in a stable order (HashMap iteration is not).
    out.sort_by(|x, y| {
        y.score
            .partial_cmp(&x.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| x.a.task_uuid.cmp(&y.a.task_uuid))
            .then_with(|| x.b.task_uuid.cmp(&y.b.task_uuid))
    });
    out.truncate(limit);
    Ok(out)
}

/// What [`merge`] did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Merged {
    pub duplicate: String,
    pub into: String,
    /// Tasks that depended on the duplicate and now depend on `into`.
    pub dependents_moved: Vec<String>,
    /// The duplicate's prerequisites `into` now also depends on.
    pub prerequisites_added: Vec<String>,
    pub labels_added: Vec<String>,
    /// `(from, to)` when `into` took the duplicate's higher priority.
    pub priority_raised: Option<(i64, i64)>,
    /// The duplicate's deadline, when `into` had none and took it.
    pub deadline_set: Option<String>,
    /// True when `into` had no recurrence rule and took the duplicate's.
    pub recurrence_copied: bool,
    /// Goal id copied onto `into` when it had none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_copied: Option<String>,
    /// `discovered_from` targets copied onto `into`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub discovered_from_added: Vec<String>,
    /// Subtasks whose parent is now `into`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subtasks_moved: Vec<String>,
    /// The duplicate's unchecked acceptance criteria, as their numbers on
    /// `into` (text `into` already had is not added twice).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub criteria_carried: Vec<i64>,
}

struct Side {
    id: String,
    pt_id: Option<String>,
    title: String,
    status: String,
    legacy_status: String,
    priority: i64,
    deadline: Option<String>,
}

fn load_side(tx: &rusqlite::Transaction<'_>, uuid: &str) -> Result<Option<Side>> {
    Ok(tx
        .query_row(
            "SELECT id, pt_id, title, status_v2, status, priority, deadline FROM tasks WHERE id=?1",
            [uuid],
            |r| {
                Ok(Side {
                    id: r.get(0)?,
                    pt_id: r.get(1)?,
                    title: r.get(2)?,
                    status: r.get(3)?,
                    legacy_status: r.get(4)?,
                    priority: r.get(5)?,
                    deadline: r.get(6)?,
                })
            },
        )
        .optional()?)
}

fn handle(s: &Side) -> String {
    s.pt_id.clone().unwrap_or_else(|| s.id.clone())
}

fn pt_handle(tx: &rusqlite::Transaction<'_>, uuid: &str) -> Result<String> {
    let pt: Option<String> = tx
        .query_row("SELECT pt_id FROM tasks WHERE id=?1", [uuid], |r| r.get(0))
        .optional()?
        .flatten();
    Ok(pt.unwrap_or_else(|| uuid.to_string()))
}

fn resolve_handle(tx: &rusqlite::Transaction<'_>, handle: &str) -> Result<Option<String>> {
    let by_id: Option<String> = tx
        .query_row("SELECT id FROM tasks WHERE id=?1", [handle], |r| r.get(0))
        .optional()?;
    if by_id.is_some() {
        return Ok(by_id);
    }
    Ok(tx
        .query_row("SELECT id FROM tasks WHERE pt_id=?1", [handle], |r| {
            r.get(0)
        })
        .optional()?)
}

fn json_str_list(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Is `target` reachable from `start` along depends_on edges?
fn reaches(tx: &rusqlite::Transaction<'_>, start: &str, target: &str) -> Result<bool> {
    Ok(tx.query_row(
        "WITH RECURSIVE reach(id) AS (
             SELECT ?1
             UNION
             SELECT l.to_uuid FROM task_links l JOIN reach r ON l.from_uuid = r.id
             WHERE l.kind = 'depends_on'
         )
         SELECT EXISTS(SELECT 1 FROM reach WHERE id = ?2)",
        params![start, target],
        |r| r.get(0),
    )?)
}

/// The event key for one of a merge's journal rows: a keyed merge derives
/// one key per row (the journal's uuid is unique), an unkeyed one generates.
fn sub_ctx(ctx: &EventCtx, part: &str) -> EventCtx {
    match ctx.event_uuid.as_deref() {
        Some(key) => ctx.with_uuid(format!("{key}:{part}")),
        None => ctx.clone(),
    }
}

/// Merge `duplicate` into `into` (see the module docs). The duplicate must
/// be open; `into` may be done ("already done") but not dismissed, and a
/// done target is refused when the duplicate still has open dependents
/// (moving them onto closed work would unblock them) or unchecked
/// acceptance criteria (a done task cannot owe them). A dependency move
/// that would close a cycle refuses the whole merge, changing nothing.
pub fn merge(
    db: &Db,
    duplicate: &str,
    into: &str,
    reason: Option<&str>,
    ctx: &EventCtx,
) -> Result<Merged> {
    if duplicate == into {
        return Err(Error::Other("a task cannot be merged into itself".into()));
    }
    let reason = reason.map(str::trim).filter(|r| !r.is_empty());
    if let Some(r) = reason
        && r.chars().count() > crate::approvals::MAX_NOTE_CHARS
    {
        return Err(Error::Other(format!(
            "reason exceeds {} characters",
            crate::approvals::MAX_NOTE_CHARS
        )));
    }
    let mut conn = db.get()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let dup =
        load_side(&tx, duplicate)?.ok_or_else(|| Error::Other("duplicate not found".into()))?;
    let canon =
        load_side(&tx, into)?.ok_or_else(|| Error::Other("target task not found".into()))?;
    if matches!(dup.status.as_str(), "done" | "dismissed") {
        return Err(Error::Other(format!(
            "{} is already {}: reopen it first to merge it",
            handle(&dup),
            dup.status
        )));
    }
    if canon.status == "dismissed" {
        return Err(Error::Other(format!(
            "{} is dismissed: reopen it, or merge the other way",
            handle(&canon)
        )));
    }
    if is_closed(&canon.status) {
        let open_dependents: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT COALESCE(t.pt_id, t.id) FROM task_links l
                   JOIN tasks t ON t.id = l.from_uuid
                  WHERE l.to_uuid=?1 AND l.kind='depends_on'
                    AND t.status_v2 NOT IN ('done','dismissed')
                  ORDER BY t.pt_id",
            )?;
            stmt.query_map([&dup.id], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?
        };
        if !open_dependents.is_empty() {
            return Err(Error::Other(format!(
                "{} is {} and merging would unblock open dependent(s) {}: reopen the target first, or close or move those dependents; nothing was merged",
                handle(&canon),
                canon.status,
                open_dependents.join(", ")
            )));
        }
    }
    // Acceptance criteria: the duplicate's unchecked ones are still owed by
    // the work, wherever it lives; checked ones already held. Without the
    // carry, merging would drop part of the definition of done the close
    // gate enforces.
    let canon_texts: BTreeSet<String> = crate::criteria::list_in_conn(&tx, &canon.id)?
        .into_iter()
        .map(|c| c.text.to_lowercase())
        .collect();
    let owed: Vec<String> = crate::criteria::unchecked_in_conn(&tx, &dup.id)?
        .into_iter()
        .filter(|c| !canon_texts.contains(&c.text.to_lowercase()))
        .map(|c| c.text)
        .collect();
    if is_closed(&canon.status) && !owed.is_empty() {
        return Err(Error::Other(format!(
            "{} is {} and {} has unchecked acceptance criteria ({}): check or remove them, or reopen the target first; nothing was merged",
            handle(&canon),
            canon.status,
            handle(&dup),
            owed.join("; ")
        )));
    }
    let now = crate::tasks::iso_now();

    // Dependents: whatever waited on the duplicate now waits on `into`.
    let dependents: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT from_uuid FROM task_links WHERE to_uuid=?1 AND kind='depends_on'
             ORDER BY created_at",
        )?;
        stmt.query_map([&dup.id], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut dependents_moved = Vec::new();
    for f in &dependents {
        tx.execute(
            "DELETE FROM task_links WHERE from_uuid=?1 AND to_uuid=?2 AND kind='depends_on'",
            params![f, dup.id],
        )?;
        if *f == canon.id {
            // `into` waited on its own duplicate: the edge just goes.
            continue;
        }
        if reaches(&tx, &canon.id, f)? {
            return Err(Error::Other(format!(
                "merging would create a dependency cycle through {f}: drop that edge first; nothing was merged"
            )));
        }
        tx.execute(
            "INSERT OR IGNORE INTO task_links (from_uuid, to_uuid, kind, created_at)
             VALUES (?1, ?2, 'depends_on', ?3)",
            params![f, canon.id, now],
        )?;
        crate::tasks::record_event_tx(
            &tx,
            &sub_ctx(ctx, &format!("dep:{f}")),
            f,
            "task.updated",
            &serde_json::json!({
                "task_uuid": f, "depends_on_added": canon.id, "depends_on_removed": dup.id,
            }),
        )?;
        dependents_moved.push(pt_handle(&tx, f)?);
    }

    // Prerequisites: the duplicate's blockers block the work wherever it lives.
    let prereqs: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT to_uuid FROM task_links WHERE from_uuid=?1 AND kind='depends_on'
             ORDER BY created_at",
        )?;
        stmt.query_map([&dup.id], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut prerequisites_added = Vec::new();
    for p in &prereqs {
        if *p == canon.id {
            continue;
        }
        if reaches(&tx, p, &canon.id)? {
            return Err(Error::Other(format!(
                "merging would create a dependency cycle through {p}: drop that edge first; nothing was merged"
            )));
        }
        let added = tx.execute(
            "INSERT OR IGNORE INTO task_links (from_uuid, to_uuid, kind, created_at)
             VALUES (?1, ?2, 'depends_on', ?3)",
            params![canon.id, p, now],
        )?;
        if added == 1 {
            prerequisites_added.push(pt_handle(&tx, p)?);
        }
    }

    // Labels, priority, deadline.
    let labels_added: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT label FROM task_labels WHERE task_uuid=?1
               AND label NOT IN (SELECT label FROM task_labels WHERE task_uuid=?2)
             ORDER BY label",
        )?;
        stmt.query_map(params![dup.id, canon.id], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    for l in &labels_added {
        tx.execute(
            "INSERT OR IGNORE INTO task_labels (task_uuid, label) VALUES (?1, ?2)",
            params![canon.id, l],
        )?;
    }
    let priority_raised = (dup.priority > canon.priority).then_some((canon.priority, dup.priority));
    if let Some((_, to)) = priority_raised {
        tx.execute(
            "UPDATE tasks SET priority=?1 WHERE id=?2",
            params![to, canon.id],
        )?;
    }
    let canon_recurring: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pt_recurrence WHERE task_uuid=?1)",
        [&canon.id],
        |r| r.get(0),
    )?;
    let dup_recurring: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pt_recurrence WHERE task_uuid=?1)",
        [&dup.id],
        |r| r.get(0),
    )?;
    let recurrence_copied = !canon_recurring && dup_recurring;
    if recurrence_copied {
        tx.execute(
            "INSERT INTO pt_recurrence (task_uuid, rrule, mode, original_input, next_occurrence, anchor)
             SELECT ?1, rrule, mode, original_input, next_occurrence, anchor
               FROM pt_recurrence WHERE task_uuid=?2",
            params![canon.id, dup.id],
        )?;
    }
    let deadline_set = match (&canon.deadline, &dup.deadline) {
        (None, Some(d)) if !canon_recurring => Some(d.clone()),
        _ => None,
    };
    if let Some(d) = &deadline_set {
        tx.execute(
            "UPDATE tasks SET deadline=?1 WHERE id=?2",
            params![d, canon.id],
        )?;
    }

    let dup_goal: Option<String> = tx
        .query_row("SELECT goal_id FROM tasks WHERE id=?1", [&dup.id], |r| {
            r.get(0)
        })
        .optional()?
        .flatten();
    let canon_goal: Option<String> = tx
        .query_row("SELECT goal_id FROM tasks WHERE id=?1", [&canon.id], |r| {
            r.get(0)
        })
        .optional()?
        .flatten();
    let mut goal_copied = None;
    if let (None, Some(g)) = (canon_goal, dup_goal) {
        tx.execute(
            "UPDATE tasks SET goal_id=?1 WHERE id=?2",
            params![g, canon.id],
        )?;
        let g_id: Option<String> = tx
            .query_row("SELECT 'G-' || seq FROM goals WHERE id=?1", [&g], |r| {
                r.get(0)
            })
            .optional()?;
        goal_copied = Some(g_id.unwrap_or(g));
    }

    let discovered: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT to_uuid FROM task_links WHERE from_uuid=?1 AND kind='discovered_from'
             ORDER BY created_at",
        )?;
        stmt.query_map([&dup.id], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut discovered_from_added = Vec::new();
    for to in &discovered {
        if *to == canon.id {
            continue;
        }
        let added = tx.execute(
            "INSERT OR IGNORE INTO task_links (from_uuid, to_uuid, kind, created_at)
             VALUES (?1, ?2, 'discovered_from', ?3)",
            params![canon.id, to, now],
        )?;
        if added == 1 {
            discovered_from_added.push(pt_handle(&tx, to)?);
        }
    }

    let mut children: BTreeSet<String> = BTreeSet::new();
    {
        let mut stmt = tx.prepare("SELECT id FROM tasks WHERE parent_uuid=?1")?;
        for id in stmt.query_map([&dup.id], |r| r.get::<_, String>(0))? {
            children.insert(id?);
        }
    }
    {
        let mut stmt =
            tx.prepare("SELECT from_uuid FROM task_links WHERE to_uuid=?1 AND kind='subtask_of'")?;
        for id in stmt.query_map([&dup.id], |r| r.get::<_, String>(0))? {
            children.insert(id?);
        }
    }
    children.remove(&canon.id);
    let mut subtasks_moved = Vec::new();
    for child in &children {
        tx.execute(
            "UPDATE tasks SET parent_uuid=?1 WHERE id=?2 AND parent_uuid=?3",
            params![canon.id, child, dup.id],
        )?;
        tx.execute(
            "DELETE FROM task_links WHERE from_uuid=?1 AND to_uuid=?2 AND kind='subtask_of'",
            params![child, dup.id],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO task_links (from_uuid, to_uuid, kind, created_at)
             VALUES (?1, ?2, 'subtask_of', ?3)",
            params![child, canon.id, now],
        )?;
        subtasks_moved.push(pt_handle(&tx, child)?);
    }

    let criteria_carried: Vec<i64> = if owed.is_empty() {
        Vec::new()
    } else {
        crate::criteria::add_in_conn(&tx, &canon.id, &owed, &sub_ctx(ctx, "criteria"))
            .map_err(|e| {
                Error::Other(format!(
                    "carrying {}'s acceptance criteria to {}: {e}; nothing was merged",
                    handle(&dup),
                    handle(&canon)
                ))
            })?
            .into_iter()
            .map(|c| c.n)
            .collect()
    };

    tx.execute(
        "UPDATE tasks SET updated_at=?1 WHERE id=?2",
        params![now, canon.id],
    )?;

    // The duplicate: dismissed, saying what it duplicates. A dismissal in
    // the journal's own vocabulary, so reopen and undo treat it as one.
    tx.execute(
        "UPDATE tasks SET status='dismissed', status_v2='dismissed', updated_at=?1 WHERE id=?2",
        params![now, dup.id],
    )?;
    tx.execute(
        "INSERT INTO interactions (task_id, action, ts, details)
         VALUES (?1, 'status_change', ?2, ?3)",
        params![
            dup.id,
            now,
            format!(
                "Dismissed as a duplicate of {} (was {})",
                handle(&canon),
                dup.legacy_status
            )
        ],
    )?;
    tx.execute(
        "DELETE FROM task_links WHERE from_uuid=?1 AND kind='duplicate_of'",
        [&dup.id],
    )?;
    tx.execute(
        "INSERT INTO task_links (from_uuid, to_uuid, kind, created_at)
         VALUES (?1, ?2, 'duplicate_of', ?3)",
        params![dup.id, canon.id, now],
    )?;
    // Journaled before the dismissal so the dismissal stays the duplicate's
    // latest event: `pt undo` selects it and [`unmerge_if_needed`] reverses
    // what moved.
    crate::tasks::record_event_tx(
        &tx,
        &sub_ctx(ctx, "merged"),
        &dup.id,
        "task.merged",
        &serde_json::json!({
            "task_uuid": dup.id, "into": canon.id, "into_pt_id": canon.pt_id,
        }),
    )?;
    let mut dup_payload = serde_json::json!({
        "task_uuid": dup.id, "pt_id": dup.pt_id, "status": "dismissed",
        "duplicate_of": canon.id, "duplicate_of_pt_id": canon.pt_id,
    });
    if let Some(r) = reason {
        dup_payload["reason"] = serde_json::json!(r);
    }
    crate::tasks::record_event_tx(&tx, ctx, &dup.id, "task.updated", &dup_payload)?;
    let mut canon_payload = serde_json::json!({
        "task_uuid": canon.id, "pt_id": canon.pt_id,
        "from": dup.id, "from_pt_id": dup.pt_id, "from_title": dup.title,
        "dependents_moved": dependents_moved, "prerequisites_added": prerequisites_added,
        "labels_added": labels_added,
        "recurrence_copied": recurrence_copied,
        "discovered_from_added": discovered_from_added,
        "subtasks_moved": subtasks_moved,
    });
    if !criteria_carried.is_empty() {
        canon_payload["criteria_carried"] = serde_json::json!(criteria_carried);
    }
    if let Some((from, to)) = priority_raised {
        canon_payload["priority"] = serde_json::json!(to);
        canon_payload["priority_from"] = serde_json::json!(from);
    }
    if let Some(d) = &deadline_set {
        canon_payload["deadline"] = serde_json::json!(d);
    }
    if let Some(g) = &goal_copied {
        canon_payload["goal_copied"] = serde_json::json!(g);
    }
    if let Some(r) = reason {
        canon_payload["reason"] = serde_json::json!(r);
    }
    crate::tasks::record_event_tx(
        &tx,
        &sub_ctx(ctx, "merged-in"),
        &canon.id,
        "task.merged_in",
        &canon_payload,
    )?;
    tx.commit()?;
    Ok(Merged {
        duplicate: handle(&dup),
        into: handle(&canon),
        dependents_moved,
        prerequisites_added,
        labels_added,
        priority_raised,
        deadline_set,
        recurrence_copied,
        goal_copied,
        discovered_from_added,
        subtasks_moved,
        criteria_carried,
    })
}

/// Merge relations recorded as `task_links.kind='duplicate_of'`, for
/// `pt show` / `task_show`: the task this one was merged into, and the
/// tasks merged into this one. Pre-V3.45 merges that wrote only the link
/// (no journal) show up here too.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MergeLinks {
    pub duplicate_of: Option<String>,
    pub merged_in: Vec<String>,
}

pub fn links(db: &Db, task_uuid: &str) -> Result<MergeLinks> {
    let conn = db.get()?;
    let duplicate_of: Option<String> = conn
        .query_row(
            "SELECT COALESCE(t.pt_id, t.id)
               FROM task_links l JOIN tasks t ON t.id = l.to_uuid
              WHERE l.from_uuid = ?1 AND l.kind = 'duplicate_of'
              LIMIT 1",
            [task_uuid],
            |r| r.get(0),
        )
        .optional()?;
    let mut stmt = conn.prepare(
        "SELECT COALESCE(t.pt_id, t.id)
           FROM task_links l JOIN tasks t ON t.id = l.from_uuid
          WHERE l.to_uuid = ?1 AND l.kind = 'duplicate_of'
          ORDER BY t.pt_id",
    )?;
    let merged_in = stmt
        .query_map([task_uuid], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(MergeLinks {
        duplicate_of,
        merged_in,
    })
}

/// Drop the `duplicate_of` link for `task_uuid` (reopen / unmerge).
pub(crate) fn clear_duplicate_of(tx: &rusqlite::Connection, task_uuid: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM task_links WHERE from_uuid=?1 AND kind='duplicate_of'",
        [task_uuid],
    )?;
    Ok(())
}

/// Reverse a merge whose duplicate is `duplicate_uuid`, using the
/// `task.merged_in` payload. No-op when there is no `duplicate_of` link.
/// Called from undo before the duplicate is reopened.
pub(crate) fn unmerge_if_needed(
    tx: &rusqlite::Transaction<'_>,
    duplicate_uuid: &str,
    ctx: &EventCtx,
) -> Result<()> {
    let canon: Option<String> = tx
        .query_row(
            "SELECT to_uuid FROM task_links
              WHERE from_uuid=?1 AND kind='duplicate_of'",
            [duplicate_uuid],
            |r| r.get(0),
        )
        .optional()?;
    let Some(canon) = canon else {
        return Ok(());
    };
    let payload_s: Option<String> = tx
        .query_row(
            "SELECT payload FROM pt_event_log
              WHERE task_uuid=?1 AND event_type='task.merged_in' AND json_valid(payload)
                AND json_extract(payload, '$.from') = ?2
              ORDER BY id DESC LIMIT 1",
            params![canon, duplicate_uuid],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(payload_s) = payload_s {
        let payload: serde_json::Value = serde_json::from_str(&payload_s).unwrap_or_default();
        let now = crate::tasks::iso_now();
        for handle in json_str_list(&payload, "dependents_moved") {
            let Some(dep) = resolve_handle(tx, &handle)? else {
                continue;
            };
            tx.execute(
                "DELETE FROM task_links WHERE from_uuid=?1 AND to_uuid=?2 AND kind='depends_on'",
                params![dep, canon],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO task_links (from_uuid, to_uuid, kind, created_at)
                 VALUES (?1, ?2, 'depends_on', ?3)",
                params![dep, duplicate_uuid, now],
            )?;
            crate::tasks::record_event_tx(
                tx,
                &sub_ctx(ctx, &format!("undep:{dep}")),
                &dep,
                "task.updated",
                &serde_json::json!({
                    "task_uuid": dep, "depends_on_added": duplicate_uuid, "depends_on_removed": canon,
                }),
            )?;
        }
        for handle in json_str_list(&payload, "prerequisites_added") {
            let Some(p) = resolve_handle(tx, &handle)? else {
                continue;
            };
            tx.execute(
                "DELETE FROM task_links WHERE from_uuid=?1 AND to_uuid=?2 AND kind='depends_on'",
                params![canon, p],
            )?;
        }
        for label in json_str_list(&payload, "labels_added") {
            tx.execute(
                "DELETE FROM task_labels WHERE task_uuid=?1 AND label=?2",
                params![canon, label],
            )?;
        }
        if let Some(from) = payload.get("priority_from").and_then(|v| v.as_i64()) {
            tx.execute(
                "UPDATE tasks SET priority=?1 WHERE id=?2",
                params![from, canon],
            )?;
        }
        if let Some(d) = payload.get("deadline").and_then(|v| v.as_str()) {
            tx.execute(
                "UPDATE tasks SET deadline=NULL WHERE id=?1 AND deadline=?2",
                params![canon, d],
            )?;
        }
        if payload.get("recurrence_copied").and_then(|v| v.as_bool()) == Some(true) {
            tx.execute("DELETE FROM pt_recurrence WHERE task_uuid=?1", [&canon])?;
        }
        if payload.get("goal_copied").is_some() {
            tx.execute("UPDATE tasks SET goal_id=NULL WHERE id=?1", [&canon])?;
        }
        for handle in json_str_list(&payload, "discovered_from_added") {
            let Some(to) = resolve_handle(tx, &handle)? else {
                continue;
            };
            tx.execute(
                "DELETE FROM task_links WHERE from_uuid=?1 AND to_uuid=?2 AND kind='discovered_from'",
                params![canon, to],
            )?;
        }
        for handle in json_str_list(&payload, "subtasks_moved") {
            let Some(child) = resolve_handle(tx, &handle)? else {
                continue;
            };
            tx.execute(
                "UPDATE tasks SET parent_uuid=?1 WHERE id=?2 AND parent_uuid=?3",
                params![duplicate_uuid, child, canon],
            )?;
            tx.execute(
                "DELETE FROM task_links WHERE from_uuid=?1 AND to_uuid=?2 AND kind='subtask_of'",
                params![child, canon],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO task_links (from_uuid, to_uuid, kind, created_at)
                 VALUES (?1, ?2, 'subtask_of', ?3)",
                params![child, duplicate_uuid, now],
            )?;
        }
        // The carried criteria go back with the duplicate (which kept its
        // own throughout), checked or not: they were the duplicate's.
        let carried: Vec<i64> = payload
            .get("criteria_carried")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|n| n.as_i64()).collect())
            .unwrap_or_default();
        if !carried.is_empty() {
            for c in crate::criteria::list_in_conn(tx, &canon)?
                .into_iter()
                .filter(|c| carried.contains(&c.n))
            {
                crate::criteria::remove_in_conn(
                    tx,
                    &canon,
                    &c,
                    &sub_ctx(ctx, &format!("uncriterion:{}", c.n)),
                )?;
            }
        }
        tx.execute(
            "UPDATE tasks SET updated_at=?1 WHERE id=?2",
            params![now, canon],
        )?;
    }
    clear_duplicate_of(tx, duplicate_uuid)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{self, NewTask};

    fn fresh() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("d.db")).unwrap();
        (dir, db)
    }

    fn add(db: &Db, title: &str) -> crate::Task {
        tasks::create(db, NewTask::minimal(title), &EventCtx::test()).unwrap()
    }

    fn status(db: &Db, id: &str) -> String {
        db.with_conn(|c| {
            Ok(
                c.query_row("SELECT status_v2 FROM tasks WHERE id=?1", [id], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap()
    }

    #[test]
    fn tokens_drop_noise_and_fold_plurals() {
        let t: Vec<String> = tokens("Fix the object store disks on rack-7 (PT-42) again")
            .into_iter()
            .collect();
        assert_eq!(t, ["7", "disk", "fix", "object", "rack", "store"]);
        assert!(tokens("a b").is_empty());
        assert_eq!(tokens("process access").len(), 2, "no folding of -ss");
        let t: Vec<String> = tokens("Campus plus status analysis").into_iter().collect();
        assert_eq!(t, ["analysis", "campus", "plus", "status"]);
        assert!(tokens("phase 2").contains("2"), "lone digits are identity");
    }

    #[test]
    fn similarity_needs_two_shared_words() {
        let s = |a: &str, b: &str| similarity(&tokens(a), &tokens(b));
        assert!(s("Vendor outreach", "Vendor outreach again") >= 0.99);
        assert!(s("North depot filing", "Depot north filing") >= DEFAULT_THRESHOLD);
        assert!(s("Fix DNS", "Fix DNS cache on rack-7") < DEFAULT_THRESHOLD);
        assert_eq!(s("Renew passport", "Renew TLS cert"), 0.0);
        assert_eq!(s("", ""), 0.0);
    }

    #[test]
    fn refuses_rewordings_but_not_identifier_differences() {
        let cand = |title: &str, score: f64| Candidate {
            task_uuid: "u".into(),
            pt_id: Some("PT-1".into()),
            title: title.into(),
            status: "todo".into(),
            score,
        };
        assert!(refuses(
            "Phase 2 of the storage migration program",
            &[cand("Storage migration program: phase 2", 0.9)]
        ));
        assert!(!refuses(
            "Storage migration program: phase 3",
            &[cand("Storage migration program: phase 2", 1.0)]
        ));
        assert!(!refuses(
            "Replace the failed disk in web-02 slot 4",
            &[cand("Replace the failed disk in web-01 slot 4", 0.86)]
        ));
    }

    #[test]
    fn similar_sees_open_and_recently_closed_but_not_merged_away() {
        let (_d, db) = fresh();
        let open = add(&db, "Voice clone pipeline for the narrator");
        let done = add(&db, "Narrator voice clone pipeline");
        tasks::mark_done(&db, &done, &EventCtx::test()).unwrap();
        let other = add(&db, "Renew the office lease");
        let got = similar(&db, "voice clone pipeline", None, DEFAULT_THRESHOLD, 10).unwrap();
        let ids: Vec<&str> = got.iter().map(|c| c.task_uuid.as_str()).collect();
        assert_eq!(ids, [open.id.as_str(), done.id.as_str()], "{got:#?}");
        assert_eq!(got[1].status, "done");
        assert!(!ids.contains(&other.id.as_str()));
        // Excluding the task itself.
        assert_eq!(
            similar(&db, &open.title, Some(&open.id), DEFAULT_THRESHOLD, 10)
                .unwrap()
                .len(),
            1
        );
        // A merged-away duplicate is not offered; its canonical task is.
        let dup = add(&db, "Voice clone pipeline (narrator)");
        merge(&db, &dup.id, &open.id, None, &EventCtx::test()).unwrap();
        let got = similar(&db, "voice clone pipeline", None, DEFAULT_THRESHOLD, 10).unwrap();
        assert!(!got.iter().any(|c| c.task_uuid == dup.id), "{got:#?}");
    }

    #[test]
    fn pairs_lists_open_lookalikes_newest_as_b() {
        let (_d, db) = fresh();
        let a = add(&db, "Nightly export job");
        let b = add(&db, "Nightly export job: phase 2");
        add(&db, "Unrelated work item");
        let p = pairs(&db, DEFAULT_THRESHOLD, 10).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(
            (p[0].a.task_uuid.as_str(), p[0].b.task_uuid.as_str()),
            (a.id.as_str(), b.id.as_str())
        );
    }

    #[test]
    fn indexed_pairs_match_a_brute_force_scan() {
        let (_d, db) = fresh();
        let vocab = [
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
            "juliet", "kilo", "lima", "mike", "november", "oscar",
        ];
        // Deterministic pseudo-random titles of 2..=5 words.
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut titles = Vec::new();
        for _ in 0..80 {
            let mut words = Vec::new();
            let n = 2 + (x % 4) as usize;
            for _ in 0..n {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                words.push(vocab[(x % vocab.len() as u64) as usize]);
            }
            let title = words.join(" ");
            add(&db, &title);
            titles.push(title);
        }
        let got: BTreeSet<(String, String)> = pairs(&db, DEFAULT_THRESHOLD, usize::MAX)
            .unwrap()
            .into_iter()
            .map(|p| (p.a.title, p.b.title))
            .collect();
        let mut want = BTreeSet::new();
        for i in 0..titles.len() {
            for j in i + 1..titles.len() {
                if similarity(&tokens(&titles[i]), &tokens(&titles[j])) >= DEFAULT_THRESHOLD {
                    want.insert((titles[i].clone(), titles[j].clone()));
                }
            }
        }
        assert!(!want.is_empty());
        assert_eq!(got, want);
    }

    #[test]
    fn merge_moves_dependents_prerequisites_labels_and_priority() {
        let (_d, db) = fresh();
        let ctx = EventCtx::test();
        let canon = add(&db, "Archive the old build logs");
        let dup = add(&db, "Old build logs archive");
        let waiter = add(&db, "publish the archive report");
        let blocker = add(&db, "order replacement tapes");
        tasks::add_dependency(&db, &waiter.id, &dup.id, &ctx).unwrap();
        tasks::add_dependency(&db, &dup.id, &blocker.id, &ctx).unwrap();
        tasks::update_priority(&db, &dup.id, 4, &ctx).unwrap();
        tasks::modify_labels(&db, &dup.id, &["domain:mgmt".into()], &[], &ctx).unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET deadline='2026-12-01' WHERE id=?1",
                [&dup.id],
            )?;
            Ok(())
        })
        .unwrap();

        let m = merge(&db, &dup.id, &canon.id, Some("same filing"), &ctx).unwrap();
        assert_eq!(m.dependents_moved, [waiter.pt_id.clone().unwrap()]);
        assert_eq!(m.prerequisites_added, [blocker.pt_id.clone().unwrap()]);
        assert_eq!(m.labels_added, ["domain:mgmt"]);
        assert_eq!(m.priority_raised, Some((2, 4)));
        assert_eq!(m.deadline_set.as_deref(), Some("2026-12-01"));
        assert_eq!(status(&db, &dup.id), "dismissed");

        // The waiter is still blocked: by the canonical task now.
        let blockers = tasks::open_blockers(&db, &waiter.id).unwrap();
        assert_eq!(blockers.len(), 1, "{blockers:?}");
        assert!(
            blockers[0].starts_with(canon.pt_id.as_deref().unwrap()),
            "{blockers:?}"
        );
        assert!(tasks::mark_done(&db, &waiter, &ctx).is_err());
        // The canonical task inherited the duplicate's blocker.
        assert_eq!(tasks::open_blockers(&db, &canon.id).unwrap().len(), 1);

        let l = links(&db, &dup.id).unwrap();
        assert_eq!(l.duplicate_of, canon.pt_id);
        assert_eq!(
            links(&db, &canon.id).unwrap().merged_in,
            [dup.pt_id.clone().unwrap()]
        );
        // Reopening the duplicate drops every merge marker, including the
        // target's merged_in list (the link is the source of truth).
        tasks::reopen(&db, &dup.id, &ctx).unwrap();
        assert_eq!(links(&db, &dup.id).unwrap().duplicate_of, None);
        assert!(links(&db, &canon.id).unwrap().merged_in.is_empty());
    }

    #[test]
    fn merge_refuses_bad_shapes_and_cycles_without_writing() {
        let (_d, db) = fresh();
        let ctx = EventCtx::test();
        let a = add(&db, "a task");
        let b = add(&db, "b task");
        assert!(merge(&db, &a.id, &a.id, None, &ctx).is_err());
        // b depends on a, c depends on b. Merging a into c would make a's
        // dependent b depend on c while c depends on b: a cycle.
        tasks::add_dependency(&db, &b.id, &a.id, &ctx).unwrap();
        let c = add(&db, "c task");
        tasks::add_dependency(&db, &c.id, &b.id, &ctx).unwrap();
        let cursor = crate::event_log::current_cursor(&db).unwrap();
        let err = merge(&db, &a.id, &c.id, None, &ctx).unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
        assert_eq!(crate::event_log::current_cursor(&db).unwrap(), cursor);
        assert_eq!(status(&db, &a.id), "todo");
        assert_eq!(tasks::open_blockers(&db, &b.id).unwrap().len(), 1);
        // A closed duplicate, or a dismissed target, is refused.
        let d = add(&db, "d task");
        tasks::dismiss(&db, &d.id, &ctx).unwrap();
        assert!(
            merge(&db, &d.id, &c.id, None, &ctx)
                .unwrap_err()
                .to_string()
                .contains("reopen")
        );
        assert!(
            merge(&db, &c.id, &d.id, None, &ctx)
                .unwrap_err()
                .to_string()
                .contains("dismissed")
        );
    }
}
