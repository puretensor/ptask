//! Throughput benchmark for the v0.8.2 SBERT embedder.
//!
//! Run with: `cargo run --release -p ptask-distill --example sbert_bench`
//!
//! Same 100-string corpus as `/tmp/sbert_bench.py`. Lets the operator
//! verify the ≥0.5× Python throughput gate before phase-9 cutover.

use ptask_distill::embeddings::Embedder;
use std::time::Instant;

const PHRASES: &[&str] = &[
    "buy bread tomorrow morning",
    "ship the quarterly report",
    "review pull request #42 from codex",
    "investigate ceph mon quorum",
    "draft email to alex challoner",
    "audit kubernetes node failures",
    "rebalance OSD weights",
    "upgrade k3s on fox-n1",
    "post bretalon announcement",
    "tag v1.0.0 release",
];

fn main() -> anyhow::Result<()> {
    let corpus: Vec<String> = (0..100)
        .map(|i| format!("task {i}: {}", PHRASES[i % PHRASES.len()]))
        .collect();
    let corpus_ref: Vec<&str> = corpus.iter().map(String::as_str).collect();

    let e = Embedder::from_hf_cache()?;
    // Warmup pass.
    let _ = e.embed(&corpus_ref[..5])?;

    let start = Instant::now();
    let vecs = e.embed(&corpus_ref)?;
    let elapsed = start.elapsed().as_secs_f64();
    let rate = corpus_ref.len() as f64 / elapsed;
    println!(
        "rs: {} strings in {:.3}s = {:.1} strings/sec",
        corpus_ref.len(),
        elapsed,
        rate
    );
    println!("rs: vec dim = {}x{}", vecs.len(), vecs[0].len());
    Ok(())
}
