//! Compare Rust embeddings against a Python-produced JSON dump.
//!
//! Run: `python3 /tmp/sbert_parity.py && cargo run --release -p ptask-distill --example sbert_parity`

use ptask_distill::embeddings::Embedder;
use std::collections::HashMap;

fn main() -> anyhow::Result<()> {
    let py: HashMap<String, Vec<f32>> =
        serde_json::from_slice(&std::fs::read("/tmp/sbert_py.json")?)?;
    let e = Embedder::from_hf_cache()?;
    let phrases: Vec<&str> = py.keys().map(String::as_str).collect();
    let rs = e.embed(&phrases)?;
    for (i, p) in phrases.iter().enumerate() {
        let py_v = &py[*p];
        let rs_v = &rs[i];
        let cos: f32 = py_v.iter().zip(rs_v).map(|(a, b)| a * b).sum();
        let max_abs_delta = py_v
            .iter()
            .zip(rs_v)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        println!("{p:60} cos={cos:.6} max_abs_delta={max_abs_delta:.6}");
    }
    Ok(())
}
