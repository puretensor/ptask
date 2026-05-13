//! Probe: print pairwise cosines for the canonical cluster test set,
//! to pick a sensible `DEFAULT_LINK_THRESHOLD`.

use ptask_distill::embeddings::Embedder;
use ptask_distill::semantic_dedup::cosine;

fn main() -> anyhow::Result<()> {
    let texts = &[
        "investigate ceph mon quorum failure",
        "rebalance OSD weights after node loss",
        "audit k3s pod evictions on arx1",
        "buy bread tomorrow morning",
        "pick up dry cleaning saturday",
        "schedule dentist appointment for next month",
    ];
    let e = Embedder::from_hf_cache()?;
    let vecs = e.embed(texts)?;
    println!("pairwise cosines:");
    for i in 0..texts.len() {
        for j in (i + 1)..texts.len() {
            println!(
                "  {:.3}  {:30}  vs  {:30}",
                cosine(&vecs[i], &vecs[j]),
                &texts[i][..30.min(texts[i].len())],
                &texts[j][..30.min(texts[j].len())]
            );
        }
    }
    Ok(())
}
