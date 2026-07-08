//! pTask native distillation orchestrator.
//!
//! `pt distill` is canonical Rust as of v3.0.0: it consumes `raw_items`,
//! classifies and consolidates through [`providers`], deduplicates candidates,
//! writes tasks through `ptask-core`, and records `distill.run` /
//! `distill.failed` events for the existing metrics and alerting surfaces.
//! The retired Python implementation lives only in the archived
//! `~/puretensor-tasks-legacy` tree for historical reference.

pub mod pipeline;
pub mod providers;
pub mod temporal_dedup;
pub mod types;

// The candle-backed embedding stack and its consumers. The production binary
// enables this through ptask-cli's `native-ml` feature; default dev builds keep
// it optional to avoid pulling the largest dependency subtree into every check.
#[cfg(feature = "native-ml")]
pub mod clustering;
#[cfg(feature = "native-ml")]
pub mod embeddings;
#[cfg(feature = "native-ml")]
pub mod semantic_dedup;
