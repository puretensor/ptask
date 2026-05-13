//! Composite priority scoring.
//!
//! Port of `~/puretensor-tasks/api/scoring.py`. The formulas, weights, and
//! column writes match the Python implementation so the cutover is a
//! systemd-unit swap, not a behaviour change.
//!
//! ```text
//! composite = 0.30·urgency + 0.20·dependency + 0.20·neglect + 0.30·manual
//! ```
//!
//! Component definitions:
//!
//! - **Urgency** — with a deadline, a sigmoid steep at 7 days out
//!   (`1 / (1 + exp((days_until − 7) / 2))`). Without, an age-driven decay
//!   peaking at 0.7 and halving roughly every 21 days
//!   (`0.7 · exp(−age_days / 21)`).
//! - **Dependency centrality** — Brandes betweenness centrality
//!   (directed, normalised by `(n−1)(n−2)`, NetworkX default) + `0.1` per
//!   transitive descendant. Clamped to `1.0`. petgraph 0.8 doesn't ship a
//!   betweenness implementation, so we bring our own.
//! - **Neglect** — over the last 14 days of `interactions`,
//!   `(0.3·views + 0.5·reopens) / max(1, 0.5·recent_count)`, clamped to `1.0`.
//!   "Reopen" = `action='status_change'` with `'pending'` somewhere in `details`.
//! - **Manual** — `(priority − 1) / 4` for `priority ∈ {1..=5}`.
//!
//! The graph mirrors Python `_build_dependency_graph` exactly: every task in
//! the scoring set is a node; for each entry in `tasks.depends_on` we add an
//! edge `dep → task`. UUIDs in `depends_on` that aren't in the scoring set
//! are silently added (matches `nx.DiGraph.add_edge`) — i.e. a done/dismissed
//! predecessor of a still-active task contributes to centrality identically
//! to Python.

use crate::Db;
use crate::dates::parse_iso_to_utc;
use crate::error::Result;
use jiff::Zoned;
use rusqlite::params;
use std::collections::{HashMap, HashSet, VecDeque};
use tracing::info;

/// Composite weights — mirror Python.
pub const W_URGENCY: f64 = 0.30;
pub const W_DEPENDENCY: f64 = 0.20;
pub const W_NEGLECT: f64 = 0.20;
pub const W_MANUAL: f64 = 0.30;

// -------- component formulas --------------------------------------------------

/// Urgency in `[0.0, 1.0]`.
pub fn urgency_score(deadline: Option<&Zoned>, created_at: &Zoned, now: &Zoned) -> f64 {
    let raw = if let Some(d) = deadline {
        let days_until =
            (d.timestamp().as_second() - now.timestamp().as_second()) as f64 / 86_400.0;
        1.0 / (1.0 + ((days_until - 7.0) / 2.0).exp())
    } else {
        let age_days = ((now.timestamp().as_second() - created_at.timestamp().as_second()) as f64
            / 86_400.0)
            .max(0.0);
        0.7 * (-age_days / 21.0).exp()
    };
    raw.clamp(0.0, 1.0)
}

/// Manual weight in `[0.0, 1.0]`. Priority is clamped to `[1, 5]`.
pub fn manual_score(priority: i64) -> f64 {
    let p = priority.clamp(1, 5);
    (p - 1) as f64 / 4.0
}

/// One row from the `interactions` table.
#[derive(Debug, Clone)]
pub struct Interaction {
    pub action: String,
    pub details: String,
    pub ts: Option<Zoned>,
}

/// Neglect in `[0.0, 1.0]`. Returns 0 if no interactions fall in the 14-day window.
pub fn neglect_score(interactions: &[Interaction], now: &Zoned) -> f64 {
    let mut total = 0i64;
    let mut views = 0i64;
    let mut reopens = 0i64;
    for i in interactions {
        let age_days = match &i.ts {
            // Floor matches Python `timedelta.days`.
            Some(t) => {
                ((now.timestamp().as_second() - t.timestamp().as_second()) as f64 / 86_400.0)
                    .floor() as i64
            }
            None => 0,
        };
        if age_days > 14 {
            continue;
        }
        total += 1;
        if i.action == "view" {
            views += 1;
        } else if i.action == "status_change" && i.details.contains("pending") {
            reopens += 1;
        }
    }
    if total == 0 {
        return 0.0;
    }
    let denom = (total as f64 * 0.5).max(1.0);
    ((views as f64 * 0.3 + reopens as f64 * 0.5) / denom).min(1.0)
}

/// Dependency score: `min(1.0, betweenness + 0.1·descendants)`.
pub fn dependency_score(centrality: f64, descendants_count: i64) -> f64 {
    (centrality + descendants_count as f64 * 0.1).min(1.0)
}

/// Composite — clamped to `[0.0, 1.0]`.
pub fn composite_score(urgency: f64, dependency: f64, neglect: f64, manual: f64) -> f64 {
    (W_URGENCY * urgency + W_DEPENDENCY * dependency + W_NEGLECT * neglect + W_MANUAL * manual)
        .clamp(0.0, 1.0)
}

// -------- dependency graph ---------------------------------------------------

/// Directed dependency graph (`dep → task`).
#[derive(Debug, Default)]
pub struct DepGraph {
    nodes: Vec<String>,
    adj: HashMap<String, Vec<String>>,
}

impl DepGraph {
    /// Build from `(task_uuid, depends_on_uuids[])` pairs. Mirrors Python
    /// `_build_dependency_graph`: tasks become nodes; each `(dep, task)` is an
    /// edge `dep → task`; UUIDs absent from the task set are silently added
    /// (matches `nx.DiGraph.add_edge`).
    pub fn from_pairs(pairs: &[(String, Vec<String>)]) -> Self {
        let mut g = DepGraph::default();
        let mut seen: HashSet<String> = HashSet::new();
        for (id, _) in pairs {
            if seen.insert(id.clone()) {
                g.nodes.push(id.clone());
                g.adj.insert(id.clone(), Vec::new());
            }
        }
        for (id, deps) in pairs {
            for dep in deps {
                if seen.insert(dep.clone()) {
                    g.nodes.push(dep.clone());
                    g.adj.insert(dep.clone(), Vec::new());
                }
                let targets = g.adj.entry(dep.clone()).or_default();
                if !targets.iter().any(|target| target == id) {
                    targets.push(id.clone());
                }
            }
        }
        g
    }

    pub fn nodes(&self) -> &[String] {
        &self.nodes
    }

    /// Brandes' algorithm — unweighted directed betweenness centrality,
    /// normalised by `(n − 1)(n − 2)` per NetworkX defaults. For `n < 3` the
    /// normaliser is undefined; NetworkX returns zeros and so do we.
    pub fn betweenness_centrality(&self) -> HashMap<String, f64> {
        let n = self.nodes.len();
        let mut cb: HashMap<String, f64> = self.nodes.iter().map(|n| (n.clone(), 0.0)).collect();
        if n < 3 {
            return cb;
        }
        for s in &self.nodes {
            let mut sigma: HashMap<String, f64> =
                self.nodes.iter().map(|n| (n.clone(), 0.0)).collect();
            sigma.insert(s.clone(), 1.0);
            let mut dist: HashMap<String, i32> =
                self.nodes.iter().map(|n| (n.clone(), -1)).collect();
            dist.insert(s.clone(), 0);
            let mut pred: HashMap<String, Vec<String>> =
                self.nodes.iter().map(|n| (n.clone(), Vec::new())).collect();
            let mut stack: Vec<String> = Vec::new();
            let mut queue: VecDeque<String> = VecDeque::from([s.clone()]);
            while let Some(v) = queue.pop_front() {
                stack.push(v.clone());
                let dv = *dist.get(&v).unwrap();
                if let Some(neighbours) = self.adj.get(&v) {
                    for w in neighbours {
                        let dw = *dist.get(w).unwrap();
                        if dw < 0 {
                            dist.insert(w.clone(), dv + 1);
                            queue.push_back(w.clone());
                        }
                        if *dist.get(w).unwrap() == dv + 1 {
                            let sv = *sigma.get(&v).unwrap();
                            *sigma.get_mut(w).unwrap() += sv;
                            pred.get_mut(w).unwrap().push(v.clone());
                        }
                    }
                }
            }
            let mut delta: HashMap<String, f64> =
                self.nodes.iter().map(|n| (n.clone(), 0.0)).collect();
            while let Some(w) = stack.pop() {
                let sw = *sigma.get(&w).unwrap();
                let dw = *delta.get(&w).unwrap();
                let preds = pred.get(&w).cloned().unwrap_or_default();
                for v in &preds {
                    let sv = *sigma.get(v).unwrap();
                    *delta.get_mut(v).unwrap() += (sv / sw) * (1.0 + dw);
                }
                if w != *s {
                    *cb.get_mut(&w).unwrap() += dw;
                }
            }
        }
        let norm = ((n - 1) * (n - 2)) as f64;
        for v in cb.values_mut() {
            *v /= norm;
        }
        cb
    }

    /// Number of nodes reachable from `node` along outbound edges, excluding
    /// `node` itself. Matches `nx.descendants(G, node)`.
    pub fn descendants_count(&self, node: &str) -> i64 {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        if let Some(neighbours) = self.adj.get(node) {
            for w in neighbours {
                if visited.insert(w.clone()) {
                    queue.push_back(w.clone());
                }
            }
        }
        while let Some(v) = queue.pop_front() {
            if let Some(neighbours) = self.adj.get(&v) {
                for w in neighbours {
                    if visited.insert(w.clone()) {
                        queue.push_back(w.clone());
                    }
                }
            }
        }
        visited.remove(node);
        visited.len() as i64
    }
}

// -------- run loop -----------------------------------------------------------

/// Report returned by `run_once_at` / `run_once`.
#[derive(Debug, Default, Clone)]
pub struct ScoringReport {
    pub tasks_scored: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
struct ScoringRow {
    id: String,
    title: String,
    priority: i64,
    created_at: String,
    deadline: Option<String>,
    depends_on: String,
}

#[derive(Debug, Clone)]
struct ScoreWrite {
    id: String,
    composite: f64,
    urgency: f64,
    dependency: f64,
    neglect: f64,
}

/// Recompute scores for every task with `status NOT IN ('done', 'dismissed')`,
/// writing back `priority_score`, `score_urgency`, `score_dependency`,
/// `score_neglect`. With `dry_run=true` the computation runs and is logged
/// but no DB writes happen — mirrors the dry-run semantics fixed in v0.7.1
/// for accountability.
pub fn run_once_at(db: &Db, dry_run: bool, now: &Zoned) -> Result<ScoringReport> {
    let rows = load_scoring_rows(db)?;
    if rows.is_empty() {
        return Ok(ScoringReport {
            tasks_scored: 0,
            dry_run,
        });
    }

    let pairs: Vec<(String, Vec<String>)> = rows
        .iter()
        .map(|r| {
            let deps: Vec<String> = serde_json::from_str(&r.depends_on).unwrap_or_default();
            (r.id.clone(), deps)
        })
        .collect();
    let graph = DepGraph::from_pairs(&pairs);
    let centrality = graph.betweenness_centrality();

    let mut scored = 0usize;
    let mut writes = Vec::with_capacity(rows.len());
    for row in &rows {
        let created_at = parse_iso_to_utc(&row.created_at).unwrap_or_else(|| now.clone());
        let deadline = row.deadline.as_deref().and_then(parse_iso_to_utc);

        let interactions = load_interactions_14d(db, &row.id)?;

        let urgency = urgency_score(deadline.as_ref(), &created_at, now);
        let manual = manual_score(row.priority);
        let neglect = neglect_score(&interactions, now);
        let cent = *centrality.get(&row.id).unwrap_or(&0.0);
        let dep = dependency_score(cent, graph.descendants_count(&row.id));
        let composite = composite_score(urgency, dep, neglect, manual);

        info!(
            target: "ptask::scoring",
            task_uuid = %row.id,
            title = %row.title.chars().take(60).collect::<String>(),
            urgency = urgency,
            dependency = dep,
            neglect = neglect,
            manual = manual,
            composite = composite,
            dry_run = dry_run,
            "scored"
        );

        if !dry_run {
            writes.push(ScoreWrite {
                id: row.id.clone(),
                composite,
                urgency,
                dependency: dep,
                neglect,
            });
        }
        scored += 1;
    }

    if !dry_run {
        write_scores_batch(db, &writes)?;
    }

    Ok(ScoringReport {
        tasks_scored: scored,
        dry_run,
    })
}

/// `run_once_at` anchored at `Zoned::now()` (operator TZ).
pub fn run_once(db: &Db, dry_run: bool) -> Result<ScoringReport> {
    let now = crate::dates::now_in_operator_tz()?;
    run_once_at(db, dry_run, &now)
}

// -------- DB helpers ---------------------------------------------------------

fn load_scoring_rows(db: &Db) -> Result<Vec<ScoringRow>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, title, COALESCE(priority, 2), created_at, deadline,
                COALESCE(depends_on, '[]')
         FROM tasks
         WHERE status NOT IN ('done', 'dismissed')
         ORDER BY id",
    )?;
    let it = stmt.query_map([], |r| {
        Ok(ScoringRow {
            id: r.get(0)?,
            title: r.get(1)?,
            priority: r.get(2)?,
            created_at: r.get(3)?,
            deadline: r.get(4)?,
            depends_on: r.get(5)?,
        })
    })?;
    let mut v = Vec::new();
    for row in it {
        v.push(row?);
    }
    Ok(v)
}

fn load_interactions_14d(db: &Db, task_id: &str) -> Result<Vec<Interaction>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT action, COALESCE(details, ''), ts FROM interactions
         WHERE task_id = ?1 AND ts >= datetime('now', '-14 days')
         ORDER BY ts DESC",
    )?;
    let it = stmt.query_map(params![task_id], |r| {
        let ts_str: Option<String> = r.get(2).ok();
        Ok(Interaction {
            action: r.get(0)?,
            details: r.get(1)?,
            ts: ts_str.as_deref().and_then(parse_iso_to_utc),
        })
    })?;
    let mut v = Vec::new();
    for row in it {
        v.push(row?);
    }
    Ok(v)
}

fn write_scores_batch(db: &Db, rows: &[ScoreWrite]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "UPDATE tasks
             SET priority_score = ?1,
                 score_urgency = ?2,
                 score_dependency = ?3,
                 score_neglect = ?4
             WHERE id = ?5",
        )?;
        for row in rows {
            stmt.execute(params![
                row.composite,
                row.urgency,
                row.dependency,
                row.neglect,
                row.id
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

// -------- tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;

    fn z(s: &str) -> Zoned {
        s.parse::<Timestamp>()
            .unwrap()
            .to_zoned(jiff::tz::TimeZone::UTC)
    }

    fn now() -> Zoned {
        z("2026-05-14T12:00:00Z")
    }

    // ---- urgency ----------------------------------------------------------

    #[test]
    fn urgency_no_deadline_decays_with_age() {
        let n = now();
        // Same instant: age=0, urgency=0.7
        let u0 = urgency_score(None, &n, &n);
        assert!((u0 - 0.7).abs() < 1e-9, "got {u0}");

        // 21 days old: 0.7 * e^-1 ≈ 0.2575
        let created = z("2026-04-23T12:00:00Z");
        let u21 = urgency_score(None, &created, &n);
        assert!((u21 - 0.7 * (-1.0f64).exp()).abs() < 1e-9, "got {u21}");
    }

    #[test]
    fn urgency_deadline_sigmoid_zero_at_far_future() {
        let n = now();
        let far = z("2027-05-14T12:00:00Z"); // 365 days away
        let u = urgency_score(Some(&far), &n, &n);
        assert!(u < 1e-3, "got {u}");
    }

    #[test]
    fn urgency_deadline_sigmoid_one_at_overdue() {
        let n = now();
        let overdue = z("2026-04-01T12:00:00Z"); // already past
        let u = urgency_score(Some(&overdue), &n, &n);
        assert!(u > 0.99, "got {u}");
    }

    #[test]
    fn urgency_deadline_at_seven_days_is_half() {
        let n = now();
        let in_7d = z("2026-05-21T12:00:00Z");
        let u = urgency_score(Some(&in_7d), &n, &n);
        assert!((u - 0.5).abs() < 1e-9, "got {u}");
    }

    // ---- manual -----------------------------------------------------------

    #[test]
    fn manual_score_table() {
        assert_eq!(manual_score(1), 0.0);
        assert_eq!(manual_score(2), 0.25);
        assert_eq!(manual_score(3), 0.5);
        assert_eq!(manual_score(4), 0.75);
        assert_eq!(manual_score(5), 1.0);
        // Clamp.
        assert_eq!(manual_score(-5), 0.0);
        assert_eq!(manual_score(99), 1.0);
    }

    // ---- neglect ----------------------------------------------------------

    fn ix(action: &str, days_ago: i64, details: &str) -> Interaction {
        let n = now().timestamp().as_second() - days_ago * 86_400;
        Interaction {
            action: action.to_string(),
            details: details.to_string(),
            ts: Some(
                Timestamp::from_second(n)
                    .unwrap()
                    .to_zoned(jiff::tz::TimeZone::UTC),
            ),
        }
    }

    #[test]
    fn neglect_no_interactions_is_zero() {
        assert_eq!(neglect_score(&[], &now()), 0.0);
    }

    #[test]
    fn neglect_old_interactions_excluded() {
        let too_old = vec![ix("view", 30, ""), ix("view", 20, "")];
        assert_eq!(neglect_score(&too_old, &now()), 0.0);
    }

    #[test]
    fn neglect_view_only_capped() {
        // 4 views in window → (4*0.3) / max(1, 4*0.5) = 1.2 / 2 = 0.6
        let v = vec![
            ix("view", 1, ""),
            ix("view", 2, ""),
            ix("view", 3, ""),
            ix("view", 4, ""),
        ];
        let s = neglect_score(&v, &now());
        assert!((s - 0.6).abs() < 1e-9, "got {s}");
    }

    #[test]
    fn neglect_status_change_to_pending_counts_as_reopen() {
        // 1 reopen, 1 view → (1*0.3 + 1*0.5) / max(1, 2*0.5) = 0.8 / 1 = 0.8
        let v = vec![
            ix("status_change", 1, "status → pending"),
            ix("view", 2, ""),
        ];
        let s = neglect_score(&v, &now());
        assert!((s - 0.8).abs() < 1e-9, "got {s}");
    }

    #[test]
    fn neglect_clamped_to_one() {
        // 10 reopens → ((10*0.5) / max(1, 10*0.5)) = 1.0
        let v: Vec<_> = (0..10).map(|d| ix("status_change", d, "pending")).collect();
        let s = neglect_score(&v, &now());
        assert!((s - 1.0).abs() < 1e-9, "got {s}");
    }

    #[test]
    fn neglect_future_interaction_matches_python_timedelta_days() {
        let future = now().checked_add(jiff::Span::new().hours(1)).unwrap();
        let v = vec![Interaction {
            action: "view".to_string(),
            details: String::new(),
            ts: Some(future),
        }];
        let s = neglect_score(&v, &now());
        assert!((s - 0.3).abs() < 1e-9, "got {s}");
    }

    // ---- dependency / Brandes --------------------------------------------

    fn pair(id: &str, deps: &[&str]) -> (String, Vec<String>) {
        (id.to_string(), deps.iter().map(|s| s.to_string()).collect())
    }

    fn assert_close(actual: &HashMap<String, f64>, key: &str, expected: f64) {
        let got = actual[key];
        assert!(
            (got - expected).abs() < 1e-12,
            "{key}: got {got}, expected {expected}; full={actual:?}"
        );
    }

    #[test]
    fn graph_lt_3_nodes_returns_zero_centrality() {
        let g = DepGraph::from_pairs(&[pair("a", &[]), pair("b", &["a"])]);
        let c = g.betweenness_centrality();
        for v in c.values() {
            assert_eq!(*v, 0.0);
        }
    }

    #[test]
    fn graph_path_three_nodes_middle_centrality() {
        // a → b → c, n=3, only (a,c) pair, σ_a,c(b) = 1 / σ_a,c = 1 → 1.0
        // Normalised: 1.0 / ((3-1)(3-2)) = 0.5
        let g = DepGraph::from_pairs(&[pair("a", &[]), pair("b", &["a"]), pair("c", &["b"])]);
        let c = g.betweenness_centrality();
        assert!((c["a"]).abs() < 1e-9, "{:?}", c);
        assert!((c["b"] - 0.5).abs() < 1e-9, "{:?}", c);
        assert!((c["c"]).abs() < 1e-9, "{:?}", c);
    }

    #[test]
    fn graph_diamond_centrality_matches_networkx() {
        // NetworkX DiGraph: a→b, a→c, b→d, c→d. b and c each carry
        // half of the shortest paths from a to d, normalised by 6.
        let g = DepGraph::from_pairs(&[
            pair("a", &[]),
            pair("b", &["a"]),
            pair("c", &["a"]),
            pair("d", &["b", "c"]),
        ]);
        let c = g.betweenness_centrality();
        assert_close(&c, "a", 0.0);
        assert_close(&c, "b", 1.0 / 12.0);
        assert_close(&c, "c", 1.0 / 12.0);
        assert_close(&c, "d", 0.0);
    }

    #[test]
    fn graph_sparse_dag_centrality_matches_networkx() {
        // Expected values from nx.betweenness_centrality on the same directed graph.
        let g = DepGraph::from_pairs(&[
            pair("a", &[]),
            pair("b", &[]),
            pair("c", &["a", "b"]),
            pair("d", &["b"]),
            pair("e", &["c", "d"]),
            pair("f", &["e", "c"]),
        ]);
        let c = g.betweenness_centrality();
        assert_close(&c, "a", 0.0);
        assert_close(&c, "b", 0.0);
        assert_close(&c, "c", 0.175);
        assert_close(&c, "d", 0.025);
        assert_close(&c, "e", 0.05);
        assert_close(&c, "f", 0.0);
    }

    #[test]
    fn graph_duplicate_dep_edges_match_networkx_digraph() {
        // NetworkX DiGraph.add_edge is idempotent. Duplicate depends_on entries
        // must not overweight one side of a diamond.
        let g = DepGraph::from_pairs(&[
            pair("a", &[]),
            pair("b", &["a", "a"]),
            pair("c", &["a"]),
            pair("d", &["b", "c"]),
        ]);
        let c = g.betweenness_centrality();
        assert_close(&c, "b", 1.0 / 12.0);
        assert_close(&c, "c", 1.0 / 12.0);
    }

    #[test]
    fn graph_descendants_count() {
        // a → b, a → c, b → d
        let g = DepGraph::from_pairs(&[
            pair("a", &[]),
            pair("b", &["a"]),
            pair("c", &["a"]),
            pair("d", &["b"]),
        ]);
        assert_eq!(g.descendants_count("a"), 3);
        assert_eq!(g.descendants_count("b"), 1);
        assert_eq!(g.descendants_count("c"), 0);
        assert_eq!(g.descendants_count("d"), 0);
    }

    #[test]
    fn graph_descendants_excludes_source_on_cycles_and_self_loops() {
        let g = DepGraph::from_pairs(&[pair("a", &["a"]), pair("b", &["a"])]);
        assert_eq!(g.descendants_count("a"), 1);
        assert_eq!(g.descendants_count("b"), 0);

        let cycle = DepGraph::from_pairs(&[pair("a", &["b"]), pair("b", &["a"])]);
        assert_eq!(cycle.descendants_count("a"), 1);
        assert_eq!(cycle.descendants_count("b"), 1);
    }

    #[test]
    fn graph_silently_adds_unknown_deps() {
        // Task "x" depends on a UUID not present in the scoring set ("ghost"). The graph
        // must still include "ghost" as a node — same as nx.DiGraph.add_edge.
        let g = DepGraph::from_pairs(&[pair("x", &["ghost"])]);
        assert!(g.nodes().iter().any(|n| n == "ghost"));
    }

    #[test]
    fn dependency_score_clamped_at_one() {
        // 20 descendants × 0.1 = 2.0 → clamped to 1.0
        assert_eq!(dependency_score(0.0, 20), 1.0);
    }

    #[test]
    fn dependency_score_combines_centrality_and_descendants() {
        // 0.3 betweenness + 5 descendants × 0.1 = 0.8
        assert!((dependency_score(0.3, 5) - 0.8).abs() < 1e-9);
    }

    // ---- composite --------------------------------------------------------

    #[test]
    fn composite_weights_sum_to_one() {
        let c = composite_score(1.0, 1.0, 1.0, 1.0);
        assert!((c - 1.0).abs() < 1e-9);
    }

    #[test]
    fn composite_clamped() {
        assert_eq!(composite_score(-5.0, -5.0, -5.0, -5.0), 0.0);
        assert_eq!(composite_score(5.0, 5.0, 5.0, 5.0), 1.0);
    }

    // ---- end-to-end run_once_at ------------------------------------------

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

    fn insert_task(
        db: &Db,
        id: &str,
        title: &str,
        priority: i64,
        deadline: Option<&str>,
        deps: &[&str],
    ) {
        let deps_json =
            serde_json::to_string(&deps.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO tasks (id, title, priority, status, created_at, updated_at, deadline, depends_on)
                 VALUES (?1, ?2, ?3, 'pending', '2026-05-01T00:00:00+00:00', '2026-05-01T00:00:00+00:00', ?4, ?5)",
                params![id, title, priority, deadline, deps_json],
            )?;
            Ok(())
        })
        .unwrap();
    }

    fn read_score(db: &Db, id: &str) -> (f64, f64, f64, f64) {
        db.with_conn(|c| {
            let row = c.query_row(
                "SELECT priority_score, score_urgency, score_dependency, score_neglect
                 FROM tasks WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, f64>(0)?,
                        r.get::<_, f64>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, f64>(3)?,
                    ))
                },
            )?;
            Ok(row)
        })
        .unwrap()
    }

    #[test]
    fn run_once_at_writes_all_four_columns() {
        let (_d, db) = fresh_db();
        insert_task(&db, "task-1", "alpha", 3, None, &[]);
        let r = run_once_at(&db, false, &now()).unwrap();
        assert_eq!(r.tasks_scored, 1);
        assert!(!r.dry_run);
        let (composite, urgency, dep, neglect) = read_score(&db, "task-1");
        assert!(composite > 0.0);
        assert!(urgency > 0.0);
        assert_eq!(dep, 0.0);
        assert_eq!(neglect, 0.0);
    }

    #[test]
    fn run_once_at_skips_done_and_dismissed() {
        let (_d, db) = fresh_db();
        insert_task(&db, "task-active", "alpha", 3, None, &[]);
        insert_task(&db, "task-done", "beta", 3, None, &[]);
        insert_task(&db, "task-dismissed", "gamma", 3, None, &[]);
        db.with_conn(|c| {
            c.execute("UPDATE tasks SET status='done' WHERE id='task-done'", [])?;
            c.execute(
                "UPDATE tasks SET status='dismissed' WHERE id='task-dismissed'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let r = run_once_at(&db, false, &now()).unwrap();
        assert_eq!(r.tasks_scored, 1);
    }

    #[test]
    fn run_once_at_dry_run_does_not_write() {
        let (_d, db) = fresh_db();
        insert_task(&db, "task-1", "alpha", 5, None, &[]);
        let before = read_score(&db, "task-1");
        let r = run_once_at(&db, true, &now()).unwrap();
        assert_eq!(r.tasks_scored, 1);
        assert!(r.dry_run);
        let after = read_score(&db, "task-1");
        assert_eq!(before, after, "dry-run must not mutate score columns");
    }

    #[test]
    fn run_once_at_dependency_chain_centrality_lands_in_db() {
        let (_d, db) = fresh_db();
        // Three-node path: a → b → c. b is the only intermediary.
        insert_task(&db, "a", "alpha", 3, None, &[]);
        insert_task(&db, "b", "beta", 3, None, &["a"]);
        insert_task(&db, "c", "gamma", 3, None, &["b"]);
        run_once_at(&db, false, &now()).unwrap();
        let dep_a = read_score(&db, "a").2; // descendants_count(a) = 2 → dep = min(1, 0 + 0.2) = 0.2
        let dep_b = read_score(&db, "b").2; // betweenness(b)=0.5, descendants(b)=1 → dep = 0.6
        let dep_c = read_score(&db, "c").2; // dep = 0
        assert!((dep_a - 0.2).abs() < 1e-9, "got {dep_a}");
        assert!((dep_b - 0.6).abs() < 1e-9, "got {dep_b}");
        assert!((dep_c).abs() < 1e-9, "got {dep_c}");
    }

    #[test]
    fn run_once_at_empty_returns_zero() {
        let (_d, db) = fresh_db();
        let r = run_once_at(&db, false, &now()).unwrap();
        assert_eq!(r.tasks_scored, 0);
    }
}
