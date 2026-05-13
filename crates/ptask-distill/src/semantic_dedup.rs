//! Cosine-similarity deduplication over SBERT embeddings.
//!
//! Port of `~/puretensor-tasks/ingest/dedup.py`. Same model, same threshold,
//! same semantics — given a candidate title and a slice of existing tasks,
//! find an existing one whose title is semantically near-equivalent and
//! return its id + the matching score.
//!
//! L2-normalised SBERT vectors mean cosine similarity is a dot product —
//! no division, no allocations beyond the embedding call itself.
//!
//! Complexity: `O((m + n) · embed)` to embed + `O(m · n)` for the cosine
//! sweep, where `m = new titles` and `n = candidates`. At N ≈ 10k tasks
//! and m ≈ 100 new candidates per distill cycle this is ~1M f32 mults
//! ≪ 1s on CPU, so a brute-force scan is fine. ANN (usearch-rs / hora /
//! ndarray-based KD-tree) is a re-attack lever for v0.9.x if N crosses 100k.

use crate::embeddings::{EMBEDDING_DIM, Embedder};
use anyhow::{Result, anyhow};

/// Default cosine threshold ported from Python `_THRESHOLD`.
pub const DEFAULT_THRESHOLD: f32 = 0.82;

/// One existing task we might collide with.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub title: String,
}

/// Result of a dedup hit.
#[derive(Debug, Clone, PartialEq)]
pub struct Duplicate {
    pub id: String,
    pub title: String,
    pub score: f32,
}

/// Look for an existing candidate whose title is ≥ `threshold` cosine-similar
/// to `new_title`. Returns the highest-scoring match or `None`.
pub fn find_duplicate(
    embedder: &Embedder,
    new_title: &str,
    candidates: &[Candidate],
    threshold: f32,
) -> Result<Option<Duplicate>> {
    if candidates.is_empty() {
        return Ok(None);
    }
    let mut texts = Vec::with_capacity(candidates.len() + 1);
    texts.push(new_title);
    texts.extend(candidates.iter().map(|c| c.title.as_str()));
    let vecs = embedder.embed(&texts)?;
    if vecs.is_empty() || vecs[0].len() != EMBEDDING_DIM {
        return Err(anyhow!("embedder returned unexpected shape"));
    }
    let new_vec = &vecs[0];
    let mut best: Option<(usize, f32)> = None;
    for (i, candidate_vec) in vecs.iter().enumerate().skip(1) {
        let score = cosine(new_vec, candidate_vec);
        match best {
            Some((_, b)) if b >= score => {}
            _ => best = Some((i - 1, score)),
        }
    }
    Ok(best.and_then(|(idx, score)| {
        if score >= threshold {
            Some(Duplicate {
                id: candidates[idx].id.clone(),
                title: candidates[idx].title.clone(),
                score,
            })
        } else {
            None
        }
    }))
}

/// Bulk variant — for each new title, return its best duplicate (or `None`)
/// against the candidate set. Embeds the new titles + candidates in a single
/// batch and reuses the candidate vectors across the sweep.
pub fn find_duplicates(
    embedder: &Embedder,
    new_titles: &[&str],
    candidates: &[Candidate],
    threshold: f32,
) -> Result<Vec<Option<Duplicate>>> {
    if new_titles.is_empty() {
        return Ok(Vec::new());
    }
    if candidates.is_empty() {
        return Ok(vec![None; new_titles.len()]);
    }
    let mut texts = Vec::with_capacity(new_titles.len() + candidates.len());
    texts.extend_from_slice(new_titles);
    texts.extend(candidates.iter().map(|c| c.title.as_str()));
    let vecs = embedder.embed(&texts)?;

    let n_new = new_titles.len();
    let cand_vecs = &vecs[n_new..];

    let mut out = Vec::with_capacity(n_new);
    for new_vec in vecs.iter().take(n_new) {
        let mut best: Option<(usize, f32)> = None;
        for (i, cand_vec) in cand_vecs.iter().enumerate() {
            let score = cosine(new_vec, cand_vec);
            match best {
                Some((_, b)) if b >= score => {}
                _ => best = Some((i, score)),
            }
        }
        out.push(best.and_then(|(idx, score)| {
            if score >= threshold {
                Some(Duplicate {
                    id: candidates[idx].id.clone(),
                    title: candidates[idx].title.clone(),
                    score,
                })
            } else {
                None
            }
        }));
    }
    Ok(out)
}

/// Cosine similarity for L2-normalised vectors (= dot product). Falls back to
/// the full formula if either vector isn't unit length (defensive — should
/// be a no-op for output from `Embedder::embed`).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if (na - 1.0).abs() < 1e-3 && (nb - 1.0).abs() < 1e-3 {
        dot
    } else {
        dot / (na.max(1e-12) * nb.max(1e-12))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, title: &str) -> Candidate {
        Candidate {
            id: id.to_string(),
            title: title.to_string(),
        }
    }

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
    fn find_duplicate_empty_candidates_returns_none() {
        let Some(e) = load_or_skip() else {
            return;
        };
        let out = find_duplicate(&e, "hello", &[], DEFAULT_THRESHOLD).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn find_duplicate_paraphrase_hits() {
        let Some(e) = load_or_skip() else {
            return;
        };
        let cands = vec![
            cand("a", "buy a loaf of bread"),
            cand("b", "investigate ceph mon quorum"),
            cand("c", "ship the quarterly report"),
        ];
        // Paraphrase of candidate "a".
        let out =
            find_duplicate(&e, "purchase a loaf of bread", &cands, DEFAULT_THRESHOLD).unwrap();
        assert!(out.is_some(), "expected a hit");
        let hit = out.unwrap();
        assert_eq!(hit.id, "a");
        assert!(hit.score >= DEFAULT_THRESHOLD);
    }

    #[test]
    fn find_duplicate_unrelated_misses() {
        let Some(e) = load_or_skip() else {
            return;
        };
        let cands = vec![cand("a", "stock market closed up today")];
        let out = find_duplicate(
            &e,
            "buy a loaf of bread tomorrow morning",
            &cands,
            DEFAULT_THRESHOLD,
        )
        .unwrap();
        assert!(out.is_none(), "did not expect a hit: {:?}", out);
    }

    #[test]
    fn find_duplicate_exact_match_score_near_one() {
        let Some(e) = load_or_skip() else {
            return;
        };
        let cands = vec![cand("a", "buy a loaf of bread tomorrow morning")];
        let out = find_duplicate(
            &e,
            "buy a loaf of bread tomorrow morning",
            &cands,
            DEFAULT_THRESHOLD,
        )
        .unwrap();
        assert!(out.is_some());
        let hit = out.unwrap();
        assert!(hit.score > 0.999, "got {}", hit.score);
    }

    #[test]
    fn find_duplicates_batch_finds_per_new_title() {
        let Some(e) = load_or_skip() else {
            return;
        };
        let cands = vec![
            cand("a", "buy a loaf of bread"),
            cand("b", "investigate ceph mon quorum"),
        ];
        let news = &["purchase a loaf of bread", "stock market closed up today"];
        let out = find_duplicates(&e, news, &cands, DEFAULT_THRESHOLD).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out[0].is_some());
        assert_eq!(out[0].as_ref().unwrap().id, "a");
        assert!(out[1].is_none());
    }

    #[test]
    fn cosine_unit_vectors_returns_dot_product() {
        let a = vec![0.6f32, 0.8, 0.0];
        let b = vec![0.0f32, 1.0, 0.0];
        // Both unit; dot = 0.8
        assert!((cosine(&a, &b) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn cosine_non_unit_vectors_normalised() {
        let a = vec![3.0f32, 4.0, 0.0]; // norm 5
        let b = vec![6.0f32, 8.0, 0.0]; // norm 10
        // Parallel → cos = 1.0
        assert!((cosine(&a, &b) - 1.0).abs() < 1e-6);
    }
}
