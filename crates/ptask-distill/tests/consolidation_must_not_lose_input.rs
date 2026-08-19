//! PT-1916 — distill must not consume rows it turned into nothing.
//!
//! Re-confirmed live at ptask HEAD 2026-08-19, `crates/ptask-distill/src/pipeline.rs`:
//!
//! ```ignore
//! let kept = select_kept_texts(&texts, &verdicts)?;
//! if !kept.is_empty() {
//!     let candidates = provider.consolidate(&kept)?;
//!     create_candidates(db, provider, candidates, dedup, st, ctx)?;
//! }
//! // Chunk complete — kept items became candidates; dropped items were
//! // judged noise. Either way they're handled.
//! st.kept += kept.len();
//! st.consumed_ids.extend(items.iter().map(|i| i.id));
//! ```
//!
//! `consumed_ids` is extended unconditionally, and those ids are what marks
//! `raw_items.processed = 1`. The trait's own doc says consolidate returns
//! "0..=4" candidates, so **zero is a documented legal return**. When it
//! happens, `create_candidates` creates nothing and the rows are consumed
//! anyway.
//!
//! The comment above that line asserts "kept items became candidates". That is
//! only true when consolidation returned something. When it returns an empty
//! vec the comment is false, the input is gone, and the run reports success —
//! `st.kept` is even incremented by the number of items that produced nothing.
//!
//! This is the one path where the loss is silent and unrecoverable: a dropped
//! item was judged noise deliberately, a provider ERROR is isolated and
//! retried by `walk_chunk`, but a kept item consolidated into nothing is
//! deleted while being counted as kept.
//!
//! Pure decision logic — no database, no provider, no network.

use ptask_distill::pipeline::{ChunkDisposition, chunk_disposition};

// ── the defect ─────────────────────────────────────────────────────────────

#[test]
fn kept_input_that_produced_no_candidate_is_retained_not_consumed() {
    let disposition = chunk_disposition(3, 0);

    assert_eq!(
        disposition,
        ChunkDisposition::Retain,
        "three items were judged signal and consolidated into nothing; \
         consuming them destroys the input while reporting success"
    );
}

#[test]
fn a_single_kept_item_that_vanished_is_also_retained() {
    assert_eq!(chunk_disposition(1, 0), ChunkDisposition::Retain);
}

// ── the normal paths must keep working ─────────────────────────────────────

#[test]
fn kept_input_that_produced_candidates_is_consumed() {
    assert_eq!(
        chunk_disposition(3, 2),
        ChunkDisposition::Consume,
        "the items became candidates; consuming them is correct and the queue \
         must keep draining"
    );
}

#[test]
fn a_chunk_where_everything_was_judged_noise_is_consumed() {
    // kept == 0 means the classifier deliberately dropped every item. That is
    // a decision, not a loss, and re-processing it forever would wedge the
    // queue on rows that will never become tasks.
    assert_eq!(
        chunk_disposition(0, 0),
        ChunkDisposition::Consume,
        "nothing was kept, so nothing was lost"
    );
}

#[test]
fn more_candidates_than_kept_items_is_still_consumed() {
    // Consolidation may split one memo into several tasks.
    assert_eq!(chunk_disposition(1, 4), ChunkDisposition::Consume);
}

#[test]
fn the_documented_maximum_fan_out_is_consumed() {
    assert_eq!(chunk_disposition(2, 4), ChunkDisposition::Consume);
}

// ── the retained case must be visible, not just skipped ────────────────────

#[test]
fn retention_is_distinguishable_from_ordinary_completion() {
    // A silent retain is only half a fix: the queue would quietly re-present
    // the same rows every run with nobody told why.
    assert_ne!(
        chunk_disposition(3, 0),
        chunk_disposition(3, 1),
        "an empty consolidation must be distinguishable from a successful one"
    );
}
