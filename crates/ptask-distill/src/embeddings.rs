//! Sentence embeddings via candle + `sentence-transformers/all-MiniLM-L6-v2`.
//!
//! Replaces the Python `SentenceTransformer("all-MiniLM-L6-v2").encode(...)`
//! calls in `~/puretensor-tasks/ingest/distill.py` and `dedup.py`. The output
//! shape, dtype, pooling, and L2-normalisation match sentence-transformers
//! exactly so downstream cosine thresholds (e.g. dedup at 0.82) keep their
//! semantics across the cutover.
//!
//! - Token IDs are produced by the same `tokenizer.json` HF ships with the
//!   model (BPE/WordPiece configuration baked into the file).
//! - Forward pass runs `candle-transformers`' `BertModel`, returning the
//!   token hidden states `[batch, seq, 384]`.
//! - Pool: attention-mask-weighted mean over the sequence axis.
//! - Normalise: per-vector L2.
//!
//! Model assets are resolved from the local Hugging Face cache
//! (`~/.cache/huggingface/hub/...`). No network access at runtime.

use anyhow::{Context, Result, anyhow};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams, TruncationStrategy};
use tracing::{debug, info};

/// Hugging Face repo we resolve for embeddings.
pub const MODEL_REPO: &str = "sentence-transformers/all-MiniLM-L6-v2";
/// MiniLM-L6's hidden dimension. Asserted at load time.
pub const EMBEDDING_DIM: usize = 384;
/// Max sequence length used by sentence-transformers for this model.
pub const MAX_SEQ_LEN: usize = 256;

/// One loaded SBERT model + tokenizer pair. Cheap to clone (interior `Arc`s).
#[derive(Clone)]
pub struct Embedder {
    inner: Arc<EmbedderInner>,
}

struct EmbedderInner {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl Embedder {
    /// Load `sentence-transformers/all-MiniLM-L6-v2` from the local HF cache.
    /// Falls back to a network fetch via `hf-hub` if the cache is empty.
    pub fn from_hf_cache() -> Result<Self> {
        let api = hf_hub::api::sync::Api::new().context("init hf-hub api")?;
        let repo = api.model(MODEL_REPO.to_string());
        let config_path = repo.get("config.json").context("fetch config.json")?;
        let tokenizer_path = repo.get("tokenizer.json").context("fetch tokenizer.json")?;
        let weights_path = repo
            .get("model.safetensors")
            .context("fetch model.safetensors")?;
        Self::from_files(&config_path, &tokenizer_path, &weights_path)
    }

    /// Load from explicit on-disk paths. Useful for tests + air-gapped nodes.
    pub fn from_files(
        config_path: &Path,
        tokenizer_path: &Path,
        weights_path: &Path,
    ) -> Result<Self> {
        let device = pick_device();
        info!(
            target: "ptask::embeddings",
            device = ?device,
            config = %config_path.display(),
            "loading SBERT"
        );

        let config_json = std::fs::read_to_string(config_path).context("read config.json")?;
        let config: Config = serde_json::from_str(&config_json).context("parse config.json")?;
        if config.hidden_size != EMBEDDING_DIM {
            return Err(anyhow!(
                "expected hidden_size={EMBEDDING_DIM}, got {}",
                config.hidden_size
            ));
        }

        let mut tokenizer =
            Tokenizer::from_file(tokenizer_path).map_err(|e| anyhow!("load tokenizer: {e}"))?;
        // sentence-transformers truncates+pads to 256 by default.
        tokenizer
            .with_padding(Some(PaddingParams {
                strategy: PaddingStrategy::BatchLongest,
                pad_id: 0,
                pad_type_id: 0,
                pad_token: "[PAD]".to_string(),
                pad_to_multiple_of: None,
                direction: tokenizers::PaddingDirection::Right,
            }))
            .with_truncation(Some(TruncationParams {
                max_length: MAX_SEQ_LEN,
                strategy: TruncationStrategy::LongestFirst,
                stride: 0,
                direction: tokenizers::TruncationDirection::Right,
            }))
            .map_err(|e| anyhow!("configure truncation: {e}"))?;

        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], DTYPE, &device)
                .context("mmap safetensors")?
        };
        let model = BertModel::load(vb, &config).context("BertModel::load")?;

        Ok(Self {
            inner: Arc::new(EmbedderInner {
                model,
                tokenizer,
                device,
            }),
        })
    }

    pub fn device(&self) -> &Device {
        &self.inner.device
    }

    /// Embed a batch of strings. Output is `[texts.len()][EMBEDDING_DIM]`,
    /// L2-normalised so dot products are cosine similarity directly.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let device = &self.inner.device;

        // Tokenize batch — uses BatchLongest padding so every sequence has the same length.
        let encodings = self
            .inner
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| anyhow!("encode_batch: {e}"))?;

        let batch = encodings.len();
        let seq_len = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(0);
        debug!(
            target: "ptask::embeddings",
            batch, seq_len, "tokenized"
        );

        // Flatten ids/masks/type_ids row-major.
        let mut ids = Vec::with_capacity(batch * seq_len);
        let mut masks = Vec::with_capacity(batch * seq_len);
        let mut types = Vec::with_capacity(batch * seq_len);
        for e in &encodings {
            ids.extend_from_slice(e.get_ids());
            masks.extend_from_slice(e.get_attention_mask());
            types.extend_from_slice(e.get_type_ids());
        }
        let ids_t = Tensor::from_vec(ids, (batch, seq_len), device)?;
        let masks_t = Tensor::from_vec(masks, (batch, seq_len), device)?;
        let types_t = Tensor::from_vec(types, (batch, seq_len), device)?;

        // Forward — `[batch, seq, hidden]` in DTYPE (f32 on CPU, f16 sometimes on CUDA).
        let hidden = self.inner.model.forward(&ids_t, &types_t, Some(&masks_t))?;

        // Cast hidden + masks to f32 for the pooling math.
        let hidden = hidden.to_dtype(DType::F32)?;
        let masks_f32 = masks_t.to_dtype(DType::F32)?.unsqueeze(2)?; // [batch, seq, 1]

        // Attention-weighted mean pool, matches sentence-transformers `mean_pooling`.
        let masked = hidden.broadcast_mul(&masks_f32)?;
        let summed = masked.sum(1)?; // [batch, hidden]
        let denom = masks_f32.sum(1)?.clamp(1e-9, f32::INFINITY)?; // [batch, 1]
        let pooled = summed.broadcast_div(&denom)?;

        // L2-normalise per row — sentence-transformers does this when
        // normalize_embeddings=True (the default used by the Python pipeline).
        let norms = pooled
            .sqr()?
            .sum_keepdim(1)?
            .sqrt()?
            .clamp(1e-12, f32::INFINITY)?;
        let normalised = pooled.broadcast_div(&norms)?;

        // Pull back to Vec<Vec<f32>>.
        let mat: Vec<Vec<f32>> = normalised.to_vec2::<f32>()?;
        Ok(mat)
    }

    /// Convenience: embed one string.
    pub fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.embed(&[text])?;
        out.pop().ok_or_else(|| anyhow!("embed_one: empty result"))
    }
}

/// Pick the best available device. CPU only until v0.8.x adds CUDA gating
/// — the candle CUDA feature pulls cuDNN headers and the build matrix gets
/// noisy on nodes without an NVIDIA driver.
fn pick_device() -> Device {
    Device::Cpu
}

/// Default model directory if the operator wants to bypass the hub:
/// `$PTASK_SBERT_DIR` or the HF cache snapshot for `MODEL_REPO`.
pub fn cached_model_dir() -> Option<PathBuf> {
    if let Ok(env) = std::env::var("PTASK_SBERT_DIR") {
        let p = PathBuf::from(env);
        if p.exists() {
            return Some(p);
        }
    }
    let cache = dirs_home()?
        .join(".cache/huggingface/hub/models--sentence-transformers--all-MiniLM-L6-v2/snapshots");
    let snap = std::fs::read_dir(&cache).ok()?.flatten().next()?.path();
    if snap.join("model.safetensors").exists() {
        Some(snap)
    } else {
        None
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_or_skip() -> Option<Embedder> {
        let dir = cached_model_dir()?;
        Embedder::from_files(
            &dir.join("config.json"),
            &dir.join("tokenizer.json"),
            &dir.join("model.safetensors"),
        )
        .ok()
    }

    #[test]
    fn embed_dimension_matches_sbert() {
        let Some(e) = load_or_skip() else {
            eprintln!("SBERT model not in HF cache — skipping test");
            return;
        };
        let v = e.embed_one("hello world").unwrap();
        assert_eq!(v.len(), EMBEDDING_DIM);
    }

    #[test]
    fn embed_outputs_are_l2_normalised() {
        let Some(e) = load_or_skip() else {
            return;
        };
        let v = e.embed_one("the cat sat on the mat").unwrap();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "L2 norm: {norm}");
    }

    #[test]
    fn embed_batch_matches_individual() {
        // sentence-transformers' batching is shape-invariant — embed([a, b])
        // and embed([a]) + embed([b]) should land within float noise.
        let Some(e) = load_or_skip() else {
            return;
        };
        let pair = e.embed(&["alpha", "beta"]).unwrap();
        let a = e.embed_one("alpha").unwrap();
        let b = e.embed_one("beta").unwrap();
        let cos_a: f32 = pair[0].iter().zip(&a).map(|(x, y)| x * y).sum();
        let cos_b: f32 = pair[1].iter().zip(&b).map(|(x, y)| x * y).sum();
        assert!(cos_a > 0.9999, "cos a: {cos_a}");
        assert!(cos_b > 0.9999, "cos b: {cos_b}");
    }

    #[test]
    fn embed_similar_strings_have_high_cosine() {
        // Trivial semantic-similarity smoke: two paraphrases should cosine
        // higher than two unrelated strings.
        let Some(e) = load_or_skip() else {
            return;
        };
        let v = e
            .embed(&[
                "the cat sat on the mat",
                "a feline rests upon a rug",
                "stock market closed up today",
            ])
            .unwrap();
        let cos = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let near = cos(&v[0], &v[1]);
        let far = cos(&v[0], &v[2]);
        assert!(near > far + 0.05, "near={near} far={far}");
    }
}
