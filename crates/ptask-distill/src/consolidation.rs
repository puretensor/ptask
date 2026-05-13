//! Stage 5 — cluster JSON → canonical tasks via HAL HTTP.
//!
//! Port of `~/puretensor-tasks/ingest/consolidate.py`. The Python module
//! called Gemini directly; this module sends the same prompt verbatim
//! to HAL, which holds the model routing + credentials. pTask never
//! sees an LLM API key.
//!
//! One cluster per request — same as Python — to keep prompts small and
//! avoid truncation. Up to 3 canonical tasks per cluster, capped at 10
//! total. Failures fall through to the next cluster; we don't drop the
//! whole batch because Gemini hiccups on one prompt.
//!
//! Wire format:
//! ```text
//! POST $PTASK_HAL_CONSOLIDATE_URL
//! {
//!   "prompt":           "<full formatted prompt>",
//!   "cluster_id":       42,
//!   "max_output_tokens": 4096
//! }
//! 200 OK
//! { "tasks": [ { title, priority, type, cluster_id, subtasks,
//!                ai_reasoning, deadline } ] }
//! ```

use crate::clustering::Cluster;
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn};

/// Verbatim prompt from `consolidate.py`. `{cluster_json}` and `{cluster_id}`
/// are filled in per cluster.
pub const CLUSTER_PROMPT: &str = r#"You are distilling raw action items into strategic tasks for a solo technical founder.

This cluster of related action items (already deduplicated):
{cluster_json}

Rules:
1. Output 1-3 tasks that subsume these items into META-GOALS.
2. Each task must be a META-GOAL that subsumes multiple raw items -- NOT atomic actions.
   BAD: "Call bank about address" (too specific)
   GOOD: "Resolve US banking setup" (meta-goal, covers 3 related items)
3. Format: verb-first imperative title ("Resolve", "Build", "Establish", "Fix", "Launch", "Contact").
4. Include the atomic items as subtasks (children), not as separate top-level tasks.
5. Assign priority 1-5:
   5 = blocks revenue/external commitment/has named deadline
   4 = external dependency or legal/financial
   3 = important infrastructure or product work
   2 = normal engineering tasks
   1 = nice-to-have, no deadline
6. type: "strategic" (founder must own it, no delegation) or "operational" (can be batched or delegated).
7. If this cluster has only 1 item and it's clearly a standalone action, include it as-is at the appropriate priority.
8. Ignore items that are observations, past completions (past tense), or pure note-taking.

Output ONLY a JSON array, no commentary, no code fences:
[
  {
    "title": "Resolve US banking setup",
    "priority": 4,
    "type": "operational",
    "cluster_id": {cluster_id},
    "subtasks": ["Call bank about address change", "Update billing address on AWS"],
    "ai_reasoning": "Blocks payroll and vendor payments",
    "deadline": null
  }
]
"#;

/// Max canonical tasks returned for the whole consolidation run.
pub const GLOBAL_MAX_TASKS: usize = 10;
/// Max canonical tasks emitted per cluster.
pub const PER_CLUSTER_MAX: usize = 3;
/// Max characters retained on canonical task titles.
pub const TITLE_TRUNCATE: usize = 200;
/// Max subtask strings carried forward per task.
pub const SUBTASK_LIMIT: usize = 20;
/// HAL response cap (matches Python `max_output_tokens`).
pub const MAX_OUTPUT_TOKENS: u32 = 4096;
/// Sleep between cluster calls. Matches Python `time.sleep(1)` rate-limit pad.
pub const INTER_CLUSTER_DELAY_MS: u64 = 1_000;
/// Retries per cluster on transient HAL failure.
pub const MAX_RETRIES: u32 = 1;

/// Canonical task ready to insert into the `tasks` table. Mirrors the
/// Python `CanonicalTask` dataclass exactly so the v0.8.8 collector
/// path can write to the same schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalTask {
    pub title: String,
    #[serde(default = "default_priority")]
    pub priority: i64,
    #[serde(rename = "type", default = "default_task_type")]
    pub task_type: String,
    #[serde(default)]
    pub subtasks: Vec<String>,
    #[serde(default)]
    pub ai_reasoning: String,
    #[serde(default)]
    pub deadline: Option<String>,
    #[serde(default)]
    pub cluster_keywords: Vec<String>,
    #[serde(default)]
    pub source_files: Vec<String>,
    #[serde(default = "default_ai_confidence")]
    pub ai_confidence: f64,
}

fn default_priority() -> i64 {
    2
}
fn default_task_type() -> String {
    "operational".to_string()
}
fn default_ai_confidence() -> f64 {
    0.85
}

#[derive(Debug, Clone, Serialize)]
struct ConsolidateRequest<'a> {
    prompt: &'a str,
    cluster_id: i64,
    max_output_tokens: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct ConsolidateResponse {
    #[serde(default)]
    tasks: Vec<CanonicalTask>,
}

/// HTTP consolidator. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Consolidator {
    url: String,
    client: reqwest::Client,
}

impl Consolidator {
    pub fn from_env() -> Result<Self> {
        let url = std::env::var("PTASK_HAL_CONSOLIDATE_URL")
            .map_err(|_| anyhow!("PTASK_HAL_CONSOLIDATE_URL not set"))?;
        Self::with_url(&url)
    }

    pub fn with_url(url: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .context("build reqwest client")?;
        Ok(Self {
            url: url.to_string(),
            client,
        })
    }

    /// Consolidate every cluster into a final canonical-task list, capped
    /// at [`GLOBAL_MAX_TASKS`]. `source_map` is an optional `cluster_id →
    /// source file paths` lookup for traceability — mirrors Python.
    pub async fn consolidate(
        &self,
        clusters: &[Cluster],
        source_map: Option<&HashMap<i64, Vec<String>>>,
    ) -> Result<Vec<CanonicalTask>> {
        if clusters.is_empty() {
            warn!(target: "ptask::consolidation", "empty input, returning []");
            return Ok(Vec::new());
        }
        info!(
            target: "ptask::consolidation",
            clusters = clusters.len(),
            "stage 5 begin"
        );

        let mut canonical: Vec<CanonicalTask> = Vec::new();
        let mut failed = 0;

        for (i, cluster) in clusters.iter().enumerate() {
            match self.consolidate_one(cluster, source_map).await {
                Ok(tasks) if !tasks.is_empty() => canonical.extend(tasks),
                Ok(_) => {
                    failed += 1;
                    warn!(
                        target: "ptask::consolidation",
                        cluster_id = cluster.id,
                        "HAL returned no tasks — skipping"
                    );
                }
                Err(e) => {
                    failed += 1;
                    warn!(
                        target: "ptask::consolidation",
                        cluster_id = cluster.id,
                        error = ?e,
                        "consolidation failed — skipping"
                    );
                }
            }
            if clusters.len() > 1 && i + 1 < clusters.len() {
                tokio::time::sleep(Duration::from_millis(INTER_CLUSTER_DELAY_MS)).await;
            }
        }

        if failed > 0 {
            warn!(
                target: "ptask::consolidation",
                failed,
                of = clusters.len(),
                "some clusters failed"
            );
        }

        if canonical.len() > GLOBAL_MAX_TASKS {
            info!(
                target: "ptask::consolidation",
                before = canonical.len(),
                after = GLOBAL_MAX_TASKS,
                "trimming to global cap"
            );
            canonical.sort_by(|a, b| b.priority.cmp(&a.priority));
            canonical.truncate(GLOBAL_MAX_TASKS);
        }

        info!(
            target: "ptask::consolidation",
            clusters = clusters.len(),
            canonical = canonical.len(),
            "stage 5 done"
        );
        Ok(canonical)
    }

    async fn consolidate_one(
        &self,
        cluster: &Cluster,
        source_map: Option<&HashMap<i64, Vec<String>>>,
    ) -> Result<Vec<CanonicalTask>> {
        let cluster_json = cluster_to_prompt_json(cluster);
        let prompt = CLUSTER_PROMPT
            .replace("{cluster_json}", &cluster_json)
            .replace("{cluster_id}", &cluster.id.to_string());
        let req = ConsolidateRequest {
            prompt: &prompt,
            cluster_id: cluster.id,
            max_output_tokens: MAX_OUTPUT_TOKENS,
        };

        let mut attempt = 0u32;
        let resp = loop {
            let r = self.client.post(&self.url).json(&req).send().await;
            match r {
                Ok(r) if r.status().is_success() => break r,
                Ok(r) => {
                    let status = r.status();
                    let body = r.text().await.unwrap_or_default();
                    let transient = status.as_u16() == 429 || status.is_server_error();
                    if transient && attempt < MAX_RETRIES {
                        warn!(
                            target: "ptask::consolidation",
                            status = status.as_u16(),
                            cluster_id = cluster.id,
                            "HAL transient — retry"
                        );
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(anyhow!("HAL HTTP {status}: {body}"));
                }
                Err(e) => {
                    if attempt < MAX_RETRIES {
                        warn!(
                            target: "ptask::consolidation",
                            error = ?e,
                            cluster_id = cluster.id,
                            "HAL transport — retry"
                        );
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(anyhow!("HAL POST failed: {e}"));
                }
            }
        };

        let body: ConsolidateResponse = resp.json().await.context("HAL JSON parse")?;
        let mut out = Vec::with_capacity(body.tasks.len().min(PER_CLUSTER_MAX));
        for raw in body.tasks.into_iter().take(PER_CLUSTER_MAX) {
            if raw.title.trim().is_empty() {
                continue;
            }
            let sources = source_map
                .and_then(|m| m.get(&cluster.id).cloned())
                .unwrap_or_default();
            out.push(CanonicalTask {
                title: truncate(&raw.title, TITLE_TRUNCATE),
                priority: raw.priority.clamp(1, 5),
                task_type: if raw.task_type.is_empty() {
                    "operational".to_string()
                } else {
                    raw.task_type
                },
                subtasks: raw.subtasks.into_iter().take(SUBTASK_LIMIT).collect(),
                ai_reasoning: raw.ai_reasoning,
                deadline: raw.deadline,
                cluster_keywords: cluster.keywords.clone(),
                source_files: sources,
                ai_confidence: if raw.ai_confidence > 0.0 {
                    raw.ai_confidence
                } else {
                    0.85
                },
            });
        }
        Ok(out)
    }
}

fn cluster_to_prompt_json(cluster: &Cluster) -> String {
    let v = serde_json::json!({
        "cluster_id": cluster.id,
        "topic_keywords": cluster.keywords.iter().take(6).collect::<Vec<_>>(),
        "topic_label": if cluster.keywords.is_empty() {
            format!("Cluster {}", cluster.id)
        } else {
            cluster.keywords.iter().take(4).cloned().collect::<Vec<_>>().join(" / ")
        },
        "item_count": cluster.items.len(),
        "items": cluster.items,
    });
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_cluster(id: i64, items: Vec<&str>) -> Cluster {
        Cluster {
            id,
            keywords: vec!["alpha".to_string(), "beta".to_string()],
            items: items.into_iter().map(str::to_string).collect(),
            item_sources: Vec::new(),
        }
    }

    #[test]
    fn canonical_task_serde_round_trip() {
        let raw = r#"{
            "title": "Resolve US banking setup",
            "priority": 4,
            "type": "operational",
            "subtasks": ["a", "b"],
            "ai_reasoning": "blocks payroll",
            "deadline": null
        }"#;
        let t: CanonicalTask = serde_json::from_str(raw).unwrap();
        assert_eq!(t.title, "Resolve US banking setup");
        assert_eq!(t.priority, 4);
        assert_eq!(t.task_type, "operational");
        assert_eq!(t.subtasks, vec!["a", "b"]);
        assert!(t.deadline.is_none());
        // Default fills.
        assert!((t.ai_confidence - 0.85).abs() < 1e-9);
    }

    #[test]
    fn cluster_to_prompt_json_carries_id_and_items() {
        let c = fake_cluster(7, vec!["x", "y"]);
        let s = cluster_to_prompt_json(&c);
        assert!(s.contains("\"cluster_id\": 7"));
        assert!(s.contains("\"item_count\": 2"));
        assert!(s.contains("\"x\""));
    }

    #[test]
    fn cluster_prompt_template_substitutes() {
        let c = fake_cluster(99, vec!["item-1", "item-2", "item-3"]);
        let cj = cluster_to_prompt_json(&c);
        let prompt = CLUSTER_PROMPT
            .replace("{cluster_json}", &cj)
            .replace("{cluster_id}", &c.id.to_string());
        assert!(prompt.contains("\"cluster_id\": 99"));
        assert!(prompt.contains("item-1"));
        // The trailing example dict's cluster_id placeholder is also replaced.
        assert!(prompt.contains("\"cluster_id\": 99,\n    \"subtasks\""));
    }

    #[test]
    fn truncate_respects_char_count() {
        let s = "abcdefghij";
        assert_eq!(truncate(s, 5), "abcde");
        assert_eq!(truncate(s, 100), s);
    }

    #[tokio::test]
    async fn consolidate_round_trip_with_mock_hal() {
        use std::net::SocketAddr;

        async fn handler(
            axum::Json(req): axum::Json<serde_json::Value>,
        ) -> axum::Json<serde_json::Value> {
            let cid = req["cluster_id"].as_i64().unwrap_or(-1);
            // Echo back two tasks regardless.
            axum::Json(serde_json::json!({
                "tasks": [
                    {
                        "title": "Resolve cluster work",
                        "priority": 3,
                        "type": "operational",
                        "cluster_id": cid,
                        "subtasks": ["a", "b"],
                        "ai_reasoning": "fixture"
                    },
                    {
                        "title": "Audit cluster work",
                        "priority": 2,
                        "type": "operational",
                        "cluster_id": cid,
                        "subtasks": [],
                        "ai_reasoning": ""
                    }
                ]
            }))
        }

        let app = axum::Router::new().route("/consolidate", axum::routing::post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("http://{addr}/consolidate");
        let c = Consolidator::with_url(&url).unwrap();
        let clusters = vec![fake_cluster(1, vec!["x", "y"]), fake_cluster(2, vec!["z"])];
        let out = c.consolidate(&clusters, None).await.unwrap();
        // Two clusters × two tasks each = 4. Below GLOBAL_MAX_TASKS so all kept.
        assert_eq!(out.len(), 4);
        // Cluster keywords carried over.
        assert_eq!(out[0].cluster_keywords, vec!["alpha", "beta"]);
        // Priority clamped, type defaulted.
        assert!(out.iter().all(|t| (1..=5).contains(&t.priority)));
    }

    #[tokio::test]
    async fn consolidate_global_cap_enforced() {
        use std::net::SocketAddr;
        async fn handler(_: axum::Json<serde_json::Value>) -> axum::Json<serde_json::Value> {
            // Three tasks per cluster (will hit PER_CLUSTER_MAX).
            axum::Json(serde_json::json!({
                "tasks": (0..3).map(|i| serde_json::json!({
                    "title": format!("task-{i}"),
                    "priority": 5,
                    "type": "strategic",
                    "subtasks": []
                })).collect::<Vec<_>>()
            }))
        }
        let app = axum::Router::new().route("/c", axum::routing::post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("http://{addr}/c");
        let c = Consolidator::with_url(&url).unwrap();
        // 5 clusters × 3 tasks = 15 raw → cap at 10.
        let clusters: Vec<_> = (0..5).map(|i| fake_cluster(i, vec!["x"])).collect();
        let out = c.consolidate(&clusters, None).await.unwrap();
        assert_eq!(out.len(), GLOBAL_MAX_TASKS);
    }
}
