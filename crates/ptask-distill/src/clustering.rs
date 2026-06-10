//! Topical clustering of distilled action items.
//!
//! v0.8.6 — replaces BERTopic with a simpler in-tree algorithm:
//! cosine-threshold connected components + token-frequency keywords. At
//! our scale (≤ a few hundred items per distill cycle) the result is
//! qualitatively similar to BERTopic's HDBSCAN output and side-steps
//! pulling `linfa-clustering` into an already-heavy build matrix.
//!
//! Algorithm:
//!   1. Embed every text with the shared SBERT [`Embedder`].
//!   2. Build a graph `G`: nodes = items, edges = `cosine(v_i, v_j) ≥ τ`.
//!   3. Connected components of `G` = clusters. Singletons land in an
//!      outlier bucket (cluster id = -1) when `min_cluster_size > 1`.
//!   4. Per-cluster keywords: top-6 alphabetic tokens of length ≥ 4
//!      excluding a small stopword set.
//!
//! Same `Cluster` / `clusters_to_json_input` shape as Python so the v0.8.7
//! HAL consolidation prompt is a drop-in.

use crate::embeddings::Embedder;
use crate::semantic_dedup::cosine;
use anyhow::Result;
use std::collections::{HashMap, HashSet, VecDeque};

/// Cosine threshold for "same-cluster" linkage. Looser than the dedup
/// threshold (0.82) — we want topically related items, not paraphrases.
/// MiniLM-L6 short-text baseline cosines on the action-item style sit
/// in the 0.15–0.30 range for same-topic pairs and below 0.10 for
/// cross-topic; 0.15 is the empirical sweet spot for our pipeline.
/// (See `examples/cluster_probe.rs` for the per-pair walk.)
pub const DEFAULT_LINK_THRESHOLD: f32 = 0.15;
/// Minimum items per cluster. Singletons are pushed into the outlier
/// bucket when this is > 1.
pub const DEFAULT_MIN_CLUSTER_SIZE: usize = 2;
/// Outlier cluster id, mirrors Python's `9999` placeholder for `-1`.
pub const OUTLIER_CLUSTER_ID: i64 = -1;

/// One topical cluster. Defined in the always-on [`crate::types`] module
/// (consolidation consumes it without `native-ml`); re-exported here so
/// `clustering::Cluster` keeps working.
pub use crate::types::Cluster;

/// Cluster a batch of texts. `metadata` is an optional parallel slice
/// (per-item source dicts that the consolidation prompt can surface).
pub fn cluster_items(
    embedder: &Embedder,
    texts: &[&str],
    metadata: Option<&[serde_json::Value]>,
    link_threshold: f32,
    min_cluster_size: usize,
) -> Result<Vec<Cluster>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }

    let vecs = embedder.embed(texts)?;
    let n = vecs.len();
    let labels = connected_components(&vecs, link_threshold);

    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &label) in labels.iter().enumerate() {
        groups.entry(label).or_default().push(i);
    }

    let placeholder_meta = serde_json::json!({});
    let mut clusters: Vec<Cluster> = Vec::with_capacity(groups.len());
    let mut outliers: Cluster = Cluster {
        id: OUTLIER_CLUSTER_ID,
        keywords: Vec::new(),
        items: Vec::new(),
        item_sources: Vec::new(),
    };
    let mut next_id: i64 = 0;
    let mut sorted_labels: Vec<usize> = groups.keys().copied().collect();
    sorted_labels.sort_unstable_by_key(|l| std::cmp::Reverse(groups[l].len()));

    for label in sorted_labels {
        let members = &groups[&label];
        let push_to_outliers = members.len() < min_cluster_size;
        let target = if push_to_outliers {
            &mut outliers
        } else {
            clusters.push(Cluster {
                id: next_id,
                keywords: Vec::new(),
                items: Vec::new(),
                item_sources: Vec::new(),
            });
            next_id += 1;
            clusters.last_mut().unwrap()
        };
        for &i in members {
            target.items.push(texts[i].to_string());
            let m = metadata
                .and_then(|s| s.get(i))
                .unwrap_or(&placeholder_meta)
                .clone();
            target.item_sources.push(m);
        }
    }

    for c in &mut clusters {
        c.keywords = extract_keywords(&c.items);
    }
    if !outliers.items.is_empty() {
        outliers.keywords = extract_keywords(&outliers.items);
        clusters.push(outliers);
    }

    // Stable: bigger clusters first; outliers last (already pushed last).
    let _ = n; // silence unused if n becomes unused after compaction.
    Ok(clusters)
}

/// Format clusters as JSON input for the consolidation LLM prompt.
/// Matches Python `clusters_to_json_input` exactly.
pub fn clusters_to_json_input(clusters: &[Cluster]) -> Vec<serde_json::Value> {
    clusters
        .iter()
        .map(|c| {
            let label = if c.keywords.is_empty() {
                format!("Cluster {}", c.id)
            } else {
                c.keywords
                    .iter()
                    .take(4)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" / ")
            };
            serde_json::json!({
                "cluster_id": c.id,
                "topic_keywords": c.keywords.iter().take(6).collect::<Vec<_>>(),
                "topic_label": label,
                "item_count": c.items.len(),
                "items": c.items,
            })
        })
        .collect()
}

// --- connected-components labelling -----------------------------------------

fn connected_components(vecs: &[Vec<f32>], threshold: f32) -> Vec<usize> {
    let n = vecs.len();
    let mut labels = vec![usize::MAX; n];
    let mut next_label = 0usize;

    for start in 0..n {
        if labels[start] != usize::MAX {
            continue;
        }
        let label = next_label;
        next_label += 1;
        labels[start] = label;
        let mut queue: VecDeque<usize> = VecDeque::from([start]);
        while let Some(v) = queue.pop_front() {
            for w in 0..n {
                if labels[w] != usize::MAX || w == v {
                    continue;
                }
                if cosine(&vecs[v], &vecs[w]) >= threshold {
                    labels[w] = label;
                    queue.push_back(w);
                }
            }
        }
    }
    labels
}

// --- keyword extraction -----------------------------------------------------

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "to", "and", "or", "for", "in", "on", "at", "is", "are", "was", "be", "have",
    "has", "with", "from", "this", "that", "it", "we", "you", "they", "of", "not", "no", "by",
    "as", "do", "did", "done", "if", "but", "yet", "via", "after", "before", "during", "into",
    "out", "up", "down", "over", "under",
];

fn extract_keywords(items: &[String]) -> Vec<String> {
    let stop: HashSet<&str> = STOPWORDS.iter().copied().collect();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for text in items {
        for tok in text.split(|c: char| !c.is_ascii_alphabetic()) {
            if tok.len() < 4 {
                continue;
            }
            let lower = tok.to_ascii_lowercase();
            if stop.contains(lower.as_str()) {
                continue;
            }
            *counts.entry(lower).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.into_iter().take(6).map(|(w, _)| w).collect()
}

// --- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn load_or_skip() -> Option<Embedder> {
        let dir = crate::embeddings::cached_model_dir()?;
        Embedder::from_files(
            &dir.join("config.json"),
            &dir.join("tokenizer.json"),
            &dir.join("model.safetensors"),
        )
        .ok()
    }

    #[test]
    fn cluster_empty_input_returns_empty() {
        let Some(e) = load_or_skip() else {
            return;
        };
        let out = cluster_items(
            &e,
            &[],
            None,
            DEFAULT_LINK_THRESHOLD,
            DEFAULT_MIN_CLUSTER_SIZE,
        )
        .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn cluster_groups_topically_similar_items() {
        let Some(e) = load_or_skip() else {
            return;
        };
        // Two clear topics: cluster ops, and personal admin.
        let texts = &[
            "investigate ceph mon quorum failure",
            "rebalance OSD weights after node loss",
            "audit k3s pod evictions on arx1",
            "buy bread tomorrow morning",
            "pick up dry cleaning saturday",
            "schedule dentist appointment for next month",
        ];
        let clusters = cluster_items(
            &e,
            texts,
            None,
            DEFAULT_LINK_THRESHOLD,
            DEFAULT_MIN_CLUSTER_SIZE,
        )
        .unwrap();
        assert!(
            clusters.len() >= 2,
            "expected at least 2 clusters, got {:?}",
            clusters
        );
        // Total assigned items (including outliers) == n.
        let total: usize = clusters.iter().map(|c| c.items.len()).sum();
        assert_eq!(total, texts.len());
    }

    #[test]
    fn singleton_lands_in_outlier_with_min_size_2() {
        let Some(e) = load_or_skip() else {
            return;
        };
        let texts = &[
            "investigate ceph mon quorum failure",
            "rebalance OSD weights after node loss",
            "the price of bitcoin is rising rapidly today",
        ];
        let clusters = cluster_items(&e, texts, None, DEFAULT_LINK_THRESHOLD, 2).unwrap();
        let outlier = clusters.iter().find(|c| c.id == OUTLIER_CLUSTER_ID);
        assert!(
            outlier.is_some(),
            "expected outlier cluster, got {clusters:?}"
        );
        assert!(outlier.unwrap().items.iter().any(|t| t.contains("bitcoin")));
    }

    #[test]
    fn keywords_strip_stopwords_and_short_tokens() {
        let items = vec![
            "the ceph quorum is down on mon1".to_string(),
            "investigate ceph quorum recovery".to_string(),
        ];
        let kws = extract_keywords(&items);
        assert!(kws.contains(&"ceph".to_string()));
        assert!(kws.contains(&"quorum".to_string()));
        assert!(
            !kws.contains(&"the".to_string()),
            "stopword leaked: {kws:?}"
        );
        // No 3-letter or shorter tokens.
        for k in &kws {
            assert!(k.len() >= 4, "short token: {k}");
        }
    }

    #[test]
    fn json_payload_shape_matches_python() {
        let cluster = Cluster {
            id: 0,
            keywords: vec!["ceph".to_string(), "quorum".to_string()],
            items: vec!["x".to_string(), "y".to_string()],
            item_sources: vec![serde_json::json!({}), serde_json::json!({})],
        };
        let j = &clusters_to_json_input(std::slice::from_ref(&cluster))[0];
        assert_eq!(j["cluster_id"], 0);
        assert_eq!(j["topic_label"], "ceph / quorum");
        assert_eq!(j["item_count"], 2);
        assert_eq!(j["items"][0], "x");
        assert_eq!(j["topic_keywords"][0], "ceph");
    }
}
