//! Speech-act classifier — routes through HAL HTTP instead of Gemini SDK.
//!
//! Port of `~/puretensor-tasks/ingest/classifier.py`. Same five classes,
//! same pre-filter heuristics, same batch shape — only the transport
//! changes. HAL holds the Gemini credentials and the model-routing policy;
//! pTask stays vendor-clean.
//!
//! Wire format:
//!
//! ```text
//! POST $PTASK_HAL_CLASSIFY_URL
//! Content-Type: application/json
//! {
//!   "items":   ["text 1", "text 2", ...],
//!   "context": { ... optional, opaque, passed through to HAL ... }
//! }
//!
//! 200 OK
//! {
//!   "classifications": [
//!     { "class": "REAL_COMMITMENT", "confidence": 0.95, "reasoning": "..." },
//!     ...
//!   ]
//! }
//! ```
//!
//! HAL is responsible for prompting Gemini and parsing back to the canonical
//! `class` set; pTask just defines the wire shape and the pre-filter.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{info, warn};

/// Five canonical speech-act classes. Wire form is SCREAMING_SNAKE_CASE
/// for byte-equivalence with the legacy Python pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SpeechAct {
    /// Speaker commits to a concrete physical/social-world action. KEEP.
    RealCommitment,
    /// Directing an AI assistant / digital agent. DROP.
    AiInstruction,
    /// Info check resolved immediately. DROP.
    TransientQuery,
    /// Vague speculation. DROP.
    MetaThinking,
    /// Past-tense / already done. DROP.
    Historical,
}

impl SpeechAct {
    /// Only `REAL_COMMITMENT` survives the filter — same gate as
    /// Python's `r["class"] == "REAL_COMMITMENT"` predicate.
    pub fn is_kept(self) -> bool {
        matches!(self, SpeechAct::RealCommitment)
    }
}

/// One classification result for one input item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Classification {
    pub class: SpeechAct,
    pub confidence: f64,
    #[serde(default)]
    pub reasoning: String,
}

/// Per-batch HAL request.
#[derive(Debug, Clone, Serialize)]
pub struct ClassifyRequest<'a> {
    pub items: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

/// Per-batch HAL response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifyResponse {
    pub classifications: Vec<Classification>,
}

/// One classified text returned to the pipeline. Index is into the
/// caller's `classify` input slice (pre-filter survivors keep their
/// original index; filtered items get a synthetic placeholder).
#[derive(Debug, Clone)]
pub struct ClassifiedItem {
    pub idx: usize,
    pub text: String,
    pub class: SpeechAct,
    pub confidence: f64,
    pub reasoning: String,
    pub pre_filtered: bool,
}

impl ClassifiedItem {
    pub fn is_kept(&self) -> bool {
        !self.pre_filtered && self.class.is_kept()
    }
}

/// Batched HTTP classifier. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Classifier {
    url: String,
    client: reqwest::Client,
    pub batch_size: usize,
    pub max_workers: usize,
}

/// Mirrors Python `BATCH_SIZE`.
pub const BATCH_SIZE: usize = 30;
/// Mirrors Python `MAX_WORKERS`.
pub const MAX_WORKERS: usize = 4;

impl Classifier {
    /// Build from the canonical env var. Returns `Err` if `PTASK_HAL_CLASSIFY_URL`
    /// is unset — callers can use [`Classifier::fallback`] for offline runs.
    pub fn from_env() -> Result<Self> {
        let url = std::env::var("PTASK_HAL_CLASSIFY_URL")
            .map_err(|_| anyhow!("PTASK_HAL_CLASSIFY_URL not set"))?;
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
            batch_size: BATCH_SIZE,
            max_workers: MAX_WORKERS,
        })
    }

    /// Synthetic classifier that returns every survivor of pre-filter as
    /// REAL_COMMITMENT/0.51. Mirrors Python's "Gemini unavailable" fallback —
    /// keeps the pipeline running without LLM access.
    pub fn fallback() -> FallbackClassifier {
        FallbackClassifier
    }

    /// Classify a batch with parallel HAL POSTs. Pre-filter heuristics run
    /// first; items shorter than 5 words, longer than 60 words, or matching
    /// the AI-prefix list are dropped without an HTTP call.
    pub async fn classify(&self, texts: &[&str]) -> Result<Vec<ClassifiedItem>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Stage 1: pre-filter. We own the survivors so spawned tasks below
        // are 'static — no borrow of the caller's slice.
        let mut survivors: Vec<(usize, String)> = Vec::with_capacity(texts.len());
        let mut filtered: Vec<ClassifiedItem> = Vec::new();
        for (idx, text) in texts.iter().enumerate() {
            if let Some(reason) = pre_filter_reason(text) {
                filtered.push(ClassifiedItem {
                    idx,
                    text: (*text).to_string(),
                    class: SpeechAct::AiInstruction,
                    confidence: 1.0,
                    reasoning: reason.to_string(),
                    pre_filtered: true,
                });
            } else {
                survivors.push((idx, (*text).to_string()));
            }
        }
        if survivors.is_empty() {
            info!(
                target: "ptask::classifier",
                discarded = filtered.len(),
                "all items filtered before HAL"
            );
            return Ok(filtered);
        }

        // Stage 2: batch + dispatch in parallel.
        let batches: Vec<Vec<(usize, String)>> = survivors
            .chunks(self.batch_size)
            .map(<[_]>::to_vec)
            .collect();
        info!(
            target: "ptask::classifier",
            items = survivors.len(),
            batches = batches.len(),
            workers = self.max_workers.min(batches.len()),
            "dispatching to HAL"
        );

        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(self.max_workers));
        let mut joins = Vec::with_capacity(batches.len());
        for batch in batches {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let this = self.clone();
            joins.push(tokio::spawn(async move {
                let _p = permit;
                this.dispatch_batch(batch).await
            }));
        }

        let mut classified = filtered;
        for j in joins {
            match j.await {
                Ok(Ok(rows)) => classified.extend(rows),
                Ok(Err(e)) => warn!(target: "ptask::classifier", error = ?e, "batch failed"),
                Err(e) => warn!(target: "ptask::classifier", error = ?e, "join failed"),
            }
        }
        classified.sort_by_key(|c| c.idx);
        Ok(classified)
    }

    async fn dispatch_batch(&self, batch: Vec<(usize, String)>) -> Result<Vec<ClassifiedItem>> {
        let req = ClassifyRequest {
            items: batch.iter().map(|(_, t)| t.as_str()).collect(),
            context: None,
        };

        let mut attempt = 0u32;
        let max_retries = 4;
        loop {
            let resp = self
                .client
                .post(&self.url)
                .json(&req)
                .send()
                .await
                .context("HAL POST")?;
            let status = resp.status();
            if status.is_success() {
                let body: ClassifyResponse = resp.json().await.context("HAL JSON parse")?;
                return Ok(merge(batch, body.classifications));
            }
            let transient = status.as_u16() == 429 || status.is_server_error();
            if transient && attempt < max_retries {
                let wait_s = (2u64).pow(attempt).min(60);
                warn!(
                    target: "ptask::classifier",
                    status = status.as_u16(),
                    attempt,
                    wait_s,
                    "HAL transient — backing off"
                );
                tokio::time::sleep(Duration::from_secs(wait_s)).await;
                attempt += 1;
                continue;
            }
            let body = resp.text().await.unwrap_or_default();
            warn!(
                target: "ptask::classifier",
                status = status.as_u16(),
                body = %body,
                "HAL non-retriable — keeping batch as REAL_COMMITMENT/0.51"
            );
            return Ok(batch
                .into_iter()
                .map(|(idx, text)| ClassifiedItem {
                    idx,
                    text,
                    class: SpeechAct::RealCommitment,
                    confidence: 0.51,
                    reasoning: format!("HAL HTTP {status}: {body}"),
                    pre_filtered: false,
                })
                .collect());
        }
    }
}

fn merge(batch: Vec<(usize, String)>, rows: Vec<Classification>) -> Vec<ClassifiedItem> {
    batch
        .into_iter()
        .enumerate()
        .map(|(j, (idx, text))| {
            let c = rows.get(j).cloned().unwrap_or(Classification {
                class: SpeechAct::MetaThinking,
                confidence: 0.5,
                reasoning: "not returned by HAL".to_string(),
            });
            ClassifiedItem {
                idx,
                text,
                class: c.class,
                confidence: c.confidence,
                reasoning: c.reasoning,
                pre_filtered: false,
            }
        })
        .collect()
}

/// Synthetic Gemini-down stand-in. Tags every survivor of pre-filter
/// as `REAL_COMMITMENT`/0.51 — same conservative degradation Python's
/// classifier falls back to.
#[derive(Debug, Clone, Copy)]
pub struct FallbackClassifier;

impl FallbackClassifier {
    pub fn classify(&self, texts: &[&str]) -> Vec<ClassifiedItem> {
        texts
            .iter()
            .enumerate()
            .map(|(idx, text)| {
                let reason = pre_filter_reason(text);
                ClassifiedItem {
                    idx,
                    text: (*text).to_string(),
                    class: if reason.is_some() {
                        SpeechAct::AiInstruction
                    } else {
                        SpeechAct::RealCommitment
                    },
                    confidence: if reason.is_some() { 1.0 } else { 0.51 },
                    reasoning: reason
                        .map(str::to_string)
                        .unwrap_or_else(|| "HAL unavailable — passthrough".to_string()),
                    pre_filtered: reason.is_some(),
                }
            })
            .collect()
    }
}

// ---- pre-filter --------------------------------------------------------------

/// Prefixes that mark obvious AI instructions / queries / commands. Cheap to
/// check before paying for an LLM call. Mirrors `_AI_PREFIXES_RE` in
/// `classifier.py` — kept as a list rather than a regex because every entry
/// is a literal prefix and Rust's `str::starts_with` after `to_lowercase`
/// avoids a regex dep.
pub const AI_PREFIXES: &[&str] = &[
    "start ",
    "spawn ",
    "run ",
    "search ",
    "enter plan ",
    "invoke ",
    "generate ",
    "create a report",
    "deploy ",
    "check if ",
    "verify if ",
    "see if ",
    "confirm if ",
    "test if ",
    "look up ",
    "fetch ",
    "query ",
    "get the ",
    "pull the ",
    "monitor ",
    "scan ",
    "index ",
    "train ",
    "evaluate ",
    "benchmark ",
    "set up ",
    "configure ",
    "install ",
    "update ",
    "upgrade ",
    "restart ",
    "reload ",
];

/// Returns `Some(reason)` if the text would be pre-filtered, `None` if it
/// should go to HAL. Mirrors Python: <5 words, >60 words, or `AI_PREFIXES`.
pub fn pre_filter_reason(text: &str) -> Option<&'static str> {
    let words = text.split_whitespace().count();
    if words < 5 {
        return Some("too_short");
    }
    if words > 60 {
        return Some("too_long");
    }
    let lower = text.to_ascii_lowercase();
    if AI_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return Some("ai_prefix");
    }
    None
}

// ---- tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_filter_drops_short() {
        assert_eq!(pre_filter_reason("buy bread"), Some("too_short"));
    }

    #[test]
    fn pre_filter_drops_long() {
        let long = "word ".repeat(70);
        assert_eq!(pre_filter_reason(&long), Some("too_long"));
    }

    #[test]
    fn pre_filter_drops_ai_prefix() {
        assert_eq!(
            pre_filter_reason("Start five agents in plan mode"),
            Some("ai_prefix")
        );
        assert_eq!(
            pre_filter_reason("verify if the deploy worked correctly today"),
            Some("ai_prefix")
        );
        assert_eq!(
            pre_filter_reason("Generate the quarterly report for review tomorrow"),
            Some("ai_prefix")
        );
    }

    #[test]
    fn pre_filter_keeps_real_commitment_shapes() {
        assert!(pre_filter_reason("Call Capital One about the credit line").is_none());
        assert!(pre_filter_reason("File the SR&ED paperwork next monday").is_none());
        assert!(pre_filter_reason("Email Alex Challoner with the OSINT brief").is_none());
    }

    #[test]
    fn speech_act_serde_screaming_snake_case() {
        // Wire format MUST match Python; Python emits "REAL_COMMITMENT" etc.
        let j = serde_json::to_string(&SpeechAct::RealCommitment).unwrap();
        assert_eq!(j, "\"REAL_COMMITMENT\"");
        let s: SpeechAct = serde_json::from_str("\"AI_INSTRUCTION\"").unwrap();
        assert_eq!(s, SpeechAct::AiInstruction);
    }

    #[test]
    fn classification_deserialises_from_hal_shape() {
        let raw = r#"{
            "class": "META_THINKING",
            "confidence": 0.42,
            "reasoning": "vague speculation"
        }"#;
        let c: Classification = serde_json::from_str(raw).unwrap();
        assert_eq!(c.class, SpeechAct::MetaThinking);
        assert!((c.confidence - 0.42).abs() < 1e-9);
        assert_eq!(c.reasoning, "vague speculation");
    }

    #[test]
    fn fallback_passthrough_marks_survivors_as_kept() {
        let out = FallbackClassifier.classify(&[
            "Call Capital One about the credit line",
            "buy bread",
            "Start five agents in plan mode",
        ]);
        assert_eq!(out.len(), 3);
        assert!(out[0].is_kept());
        assert!(!out[1].is_kept());
        assert!(!out[2].is_kept());
        assert_eq!(out[0].class, SpeechAct::RealCommitment);
    }

    // Integration test against a local mock HAL endpoint.
    #[tokio::test]
    async fn classify_round_trip_with_mock_hal() {
        use std::net::SocketAddr;

        // Tiny axum app that returns a fixed classifications array.
        async fn handler(
            axum::Json(req): axum::Json<serde_json::Value>,
        ) -> axum::Json<ClassifyResponse> {
            let items = req["items"].as_array().cloned().unwrap_or_default();
            let mut classifications = Vec::with_capacity(items.len());
            for item in items {
                let text = item.as_str().unwrap_or("");
                let class = if text.to_ascii_lowercase().contains("call") {
                    SpeechAct::RealCommitment
                } else {
                    SpeechAct::MetaThinking
                };
                classifications.push(Classification {
                    class,
                    confidence: 0.92,
                    reasoning: "mock".to_string(),
                });
            }
            axum::Json(ClassifyResponse { classifications })
        }

        let app = axum::Router::new().route("/classify", axum::routing::post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("http://{addr}/classify");
        let c = Classifier::with_url(&url).unwrap();
        // Mix: 1 commitment, 1 vague, 1 short (pre-filter), 1 AI (pre-filter)
        let texts = &[
            "Call Capital One about the credit line",
            "Should look into the new metrics dashboard later",
            "buy bread",
            "Generate the quarterly report for review tomorrow",
        ];
        let out = c.classify(texts).await.unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].class, SpeechAct::RealCommitment);
        assert!(out[0].is_kept());
        assert_eq!(out[1].class, SpeechAct::MetaThinking);
        assert!(!out[1].is_kept());
        assert!(out[2].pre_filtered);
        assert!(out[3].pre_filtered);
    }
}
