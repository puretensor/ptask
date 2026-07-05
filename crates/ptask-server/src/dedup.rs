//! Semantic dedup for the capture fast lane (feature `semantic-dedup`).
//!
//! Fail-open by contract: any error — model missing, embed failure, feature
//! disabled — returns `None` and the caller creates the task normally. A
//! genuine sev>=3 incident must never be delayed or dropped by dedup.

#[cfg(feature = "semantic-dedup")]
mod inner {
    use ptask_distill::embeddings::Embedder;
    use ptask_distill::semantic_dedup::{self, Candidate};
    use std::sync::{Arc, OnceLock};

    static EMBEDDER: OnceLock<Option<Arc<Embedder>>> = OnceLock::new();

    fn embedder() -> Option<Arc<Embedder>> {
        EMBEDDER
            .get_or_init(|| match Embedder::from_hf_cache() {
                Ok(e) => Some(Arc::new(e)),
                Err(e) => {
                    tracing::warn!(
                        target: "ptask::capture",
                        error = %e,
                        "semantic dedup unavailable — embedder load failed, failing open"
                    );
                    None
                }
            })
            .clone()
    }

    /// Best semantic match >= `threshold` among `(uuid, title)` candidates.
    pub fn best_match(
        title: &str,
        candidates: &[(String, String)],
        threshold: f32,
    ) -> Option<(String, f32)> {
        if candidates.is_empty() {
            return None;
        }
        let embedder = embedder()?;
        let cands: Vec<Candidate> = candidates
            .iter()
            .map(|(id, t)| Candidate {
                id: id.clone(),
                title: t.clone(),
            })
            .collect();
        match semantic_dedup::find_duplicate(&embedder, title, &cands, threshold) {
            Ok(Some(d)) => Some((d.id, d.score)),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(
                    target: "ptask::capture",
                    error = %e,
                    "semantic dedup failed — failing open"
                );
                None
            }
        }
    }
}

#[cfg(feature = "semantic-dedup")]
pub use inner::best_match;

#[cfg(not(feature = "semantic-dedup"))]
pub fn best_match(
    _title: &str,
    _candidates: &[(String, String)],
    _threshold: f32,
) -> Option<(String, f32)> {
    None
}

/// Cosine threshold for treating two incident titles as the same incident.
/// Mirrors `ptask_distill::semantic_dedup::DEFAULT_THRESHOLD`.
pub const CAPTURE_THRESHOLD: f32 = 0.82;
