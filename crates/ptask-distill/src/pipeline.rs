//! Native distillation pipeline (v2.1.0) — replaces the legacy Python
//! subprocess for the default `pt distill` path.
//!
//! Delta-driven: consumes `raw_items WHERE processed = 0` in batches
//! instead of re-reading a 60-day window of files every run, which makes
//! hourly micro-runs affordable. Flow per run:
//!
//!   1. preflight the provider (dead credentials abort BEFORE consuming)
//!   2. classify the batch (fail closed on provider errors)
//!   3. consolidate kept items into candidates
//!   4. dedup candidates against the last 30 days of tasks (any status —
//!      recreating something the operator dismissed is the resurrection
//!      bug this replaces)
//!   5. create surviving candidates (attributed to `distill`), mark the
//!      batch processed, and record a manifest event
//!
//! Success records `distill.run` (payload.native = true) so the existing
//! `pt_distill_last_success_age_seconds` gauge and PtaskDistillStale alert
//! keep working unchanged; failures record `distill.failed` and exit
//! non-zero.

use crate::providers::{Classification, LlmProvider};
use anyhow::{Context, Result, bail};
use ptask_core::Db;
use ptask_core::event_log::{self, EventCtx};
use ptask_core::{Extensions, NewTask};
use tracing::{info, warn};

#[derive(Debug, Clone, serde::Serialize)]
pub struct NativeReport {
    pub consumed: usize,
    pub kept: usize,
    pub created: usize,
    pub skipped_dedup: usize,
    /// Rows isolated as unprocessable this run and charged an attempt.
    pub failed: usize,
    /// Rows currently parked out of the queue (attempts exhausted). A
    /// standing count, not a per-run delta — it is the poison-pill gauge.
    pub quarantined: usize,
    pub provider: String,
    pub duration_ms: u128,
}

/// Similarity gate: normalized token overlap (Jaccard on lowercase words).
/// Cheap, deterministic, no model download.
fn title_similar(a: &str, b: &str) -> bool {
    let toks = |s: &str| {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(|w| w.to_string())
            .collect::<std::collections::HashSet<_>>()
    };
    let (ta, tb) = (toks(a), toks(b));
    if ta.is_empty() || tb.is_empty() {
        return false;
    }
    let inter = ta.intersection(&tb).count() as f64;
    let union = ta.union(&tb).count() as f64;
    inter / union >= 0.6
}

/// Select kept inputs only after proving the provider returned a complete,
/// one-to-one index permutation. A response with the right length can still
/// duplicate one index and omit another; silently accepting that drops work.
fn select_kept_texts(texts: &[String], verdicts: &[Classification]) -> Result<Vec<String>> {
    if verdicts.len() != texts.len() {
        bail!(
            "provider returned {} verdicts for {} items — failing closed",
            verdicts.len(),
            texts.len()
        );
    }
    let mut seen = vec![false; texts.len()];
    let mut kept = Vec::new();
    for verdict in verdicts {
        let Some(text) = texts.get(verdict.idx) else {
            bail!(
                "provider returned out-of-range verdict index {} for {} items — failing closed",
                verdict.idx,
                texts.len()
            );
        };
        if std::mem::replace(&mut seen[verdict.idx], true) {
            bail!(
                "provider returned duplicate verdict index {} — failing closed",
                verdict.idx
            );
        }
        if verdict.keep {
            kept.push(text.clone());
        }
    }
    Ok(kept)
}

fn existing_tasks_since(db: &Db, cutoff: &str) -> Result<Vec<(String, String)>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, title FROM tasks
         WHERE status_v2 NOT IN ('done','dismissed')
            OR julianday(updated_at) >= julianday(?1)",
    )?;
    let rows = stmt.query_map([cutoff], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Items handed to the provider in one classify call. The whole
/// `fetch_unprocessed` batch (default 200) used to go in a single request, so
/// one unclassifiable memo failed the run and nothing was ever marked
/// processed — the same oldest-first rows came back forever.
const CHUNK: usize = 25;

/// Ceiling on provider calls per run. Failure isolation halves a failing
/// chunk, so a pathologically bad batch could otherwise fan out to ~2N calls.
/// Hitting the ceiling ends the run early; whatever succeeded is still
/// consumed, so the next run starts from a strictly shorter queue.
const MAX_PROVIDER_CALLS: usize = 64;

/// Dedup universe for one run: everything active + anything touched in 30
/// days (incl. done/dismissed — the operator said no once already). Shared
/// across chunks and extended in place as tasks are created, so two chunks in
/// the same run can't create the same task twice.
struct Dedup {
    existing: Vec<(String, String)>,
    #[cfg(feature = "native-ml")]
    embedder: LazyEmbedder,
}

impl Dedup {
    fn load(db: &Db) -> Result<Self> {
        let cutoff = ptask_core::dates::format_iso(
            &ptask_core::dates::now_in_operator_tz()?
                .checked_sub(ptask_core::jiff::Span::new().days(30))
                .map_err(|e| anyhow::anyhow!("cutoff math: {e}"))?,
        );
        Ok(Self {
            existing: existing_tasks_since(db, &cutoff)?,
            #[cfg(feature = "native-ml")]
            embedder: LazyEmbedder::default(),
        })
    }
}

/// v2.5.0's semantic layer over the Jaccard gate. Loaded on the first kept
/// candidate rather than per chunk (the model load dominates a run); a load
/// failure degrades to Jaccard-only (fail open).
#[cfg(feature = "native-ml")]
#[derive(Default)]
struct LazyEmbedder {
    attempted: bool,
    inner: Option<crate::embeddings::Embedder>,
}

#[cfg(feature = "native-ml")]
impl LazyEmbedder {
    fn get(&mut self) -> Option<&crate::embeddings::Embedder> {
        if !self.attempted {
            self.attempted = true;
            self.inner = match crate::embeddings::Embedder::from_hf_cache() {
                Ok(e) => Some(e),
                Err(e) => {
                    warn!(target: "ptask::distill", error = %e, "embedder unavailable — Jaccard-only dedup this run");
                    None
                }
            };
        }
        self.inner.as_ref()
    }
}

/// Mutable bookkeeping threaded through the chunk walk.
#[derive(Default)]
struct RunState {
    kept: usize,
    created: usize,
    skipped: usize,
    /// Rows whose chunk completed — safe to mark processed.
    consumed_ids: Vec<i64>,
    /// Rows isolated as unprocessable and charged an attempt, with the reason.
    failures: Vec<(i64, String)>,
    /// Rows isolated for a *local* reason (a database fault, not the
    /// capture's content). Retried next run, never charged.
    deferred: usize,
    /// The first failure seen, un-bisected — the one worth reporting.
    first_error: Option<String>,
    calls: usize,
    budget_exhausted: bool,
}

/// Why a chunk failed, and whether the capture may be blamed for it.
struct ChunkError {
    reason: String,
    /// Provider/classification stage: the response could not be turned into
    /// verdicts, so the content is a plausible cause and the row may be
    /// charged an attempt. `preflight` succeeded seconds earlier and the
    /// Gemini client already retries transient transport/5xx failures three
    /// times, so a failure here is much more likely to be the data.
    chargeable: bool,
}

impl ChunkError {
    fn provider(e: anyhow::Error) -> Self {
        Self {
            reason: format!("{e:#}"),
            chargeable: true,
        }
    }

    /// A local database fault is never the capture's fault — charging it
    /// would quarantine good work during an unrelated outage.
    fn local(e: anyhow::Error) -> Self {
        Self {
            reason: format!("{e:#}"),
            chargeable: false,
        }
    }
}

/// Classify one chunk, consolidate what it kept, and create the survivors.
/// Any error here is the chunk's error: the caller isolates it.
fn process_chunk<P: LlmProvider>(
    db: &Db,
    provider: &P,
    items: &[ptask_core::raw_items::RawItem],
    dedup: &mut Dedup,
    st: &mut RunState,
    ctx: &EventCtx,
) -> std::result::Result<(), ChunkError> {
    st.calls += 1;
    let texts: Vec<String> = items.iter().map(|i| i.text.clone()).collect();
    let verdicts = provider
        .classify_batch(&texts)
        .map_err(ChunkError::provider)?;
    let kept = select_kept_texts(&texts, &verdicts).map_err(ChunkError::provider)?;
    if !kept.is_empty() {
        st.calls += 1;
        let candidates = provider.consolidate(&kept).map_err(ChunkError::provider)?;
        create_candidates(db, provider, candidates, dedup, st, ctx).map_err(ChunkError::local)?;
    }
    // Chunk complete — kept items became candidates; dropped items were
    // judged noise. Either way they're handled.
    st.kept += kept.len();
    st.consumed_ids.extend(items.iter().map(|i| i.id));
    Ok(())
}

/// Walk a chunk, halving it on failure so a single unprocessable row is
/// isolated instead of taking its neighbours down with it.
fn walk_chunk<P: LlmProvider>(
    db: &Db,
    provider: &P,
    items: &[ptask_core::raw_items::RawItem],
    dedup: &mut Dedup,
    st: &mut RunState,
    ctx: &EventCtx,
) {
    if items.is_empty() {
        return;
    }
    if st.calls >= MAX_PROVIDER_CALLS {
        st.budget_exhausted = true;
        return;
    }
    let Err(e) = process_chunk(db, provider, items, dedup, st, ctx) else {
        return;
    };
    if st.first_error.is_none() {
        st.first_error = Some(e.reason.clone());
    }
    if items.len() == 1 {
        warn!(
            target: "ptask::distill",
            raw_item = items[0].id,
            chargeable = e.chargeable,
            error = %e.reason,
            "isolated an unprocessable capture"
        );
        if e.chargeable {
            st.failures.push((items[0].id, e.reason));
        } else {
            st.deferred += 1;
        }
        return;
    }
    warn!(
        target: "ptask::distill",
        chunk = items.len(),
        error = %e.reason,
        "chunk failed — bisecting to isolate the offending capture"
    );
    let mid = items.len() / 2;
    walk_chunk(db, provider, &items[..mid], dedup, st, ctx);
    walk_chunk(db, provider, &items[mid..], dedup, st, ctx);
}

/// Create the survivors of one chunk's consolidation, running every dedup
/// gate against the shared run universe.
fn create_candidates<P: LlmProvider>(
    db: &Db,
    provider: &P,
    candidates: Vec<crate::providers::Candidate>,
    dedup: &mut Dedup,
    st: &mut RunState,
    ctx: &EventCtx,
) -> Result<()> {
    for cand in candidates {
        if dedup
            .existing
            .iter()
            .any(|(_, t)| title_similar(t, &cand.title))
        {
            st.skipped += 1;
            info!(target: "ptask::distill", title = %cand.title, "dedup skip (jaccard)");
            continue;
        }
        // Exact-hash temporal dedup: the same candidate text distilled
        // twice inside 7 days is a re-ingest, not new work. Only record
        // the candidate after task creation succeeds; otherwise a
        // transient database failure would make the retry disappear.
        match crate::temporal_dedup::is_temporal_duplicate(db, "distill-candidate", &cand.title, 7)
        {
            Ok(true) => {
                st.skipped += 1;
                info!(target: "ptask::distill", title = %cand.title, "dedup skip (temporal)");
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                warn!(target: "ptask::distill", error = %e, "temporal dedup failed — failing open");
            }
        }
        // Semantic dedup: paraphrases of anything in the 30d universe
        // (including dismissed — that is the resurrection bug) skip.
        #[cfg(feature = "native-ml")]
        {
            let semantic_candidates: Vec<crate::semantic_dedup::Candidate> = dedup
                .existing
                .iter()
                .map(|(id, title)| crate::semantic_dedup::Candidate {
                    id: id.clone(),
                    title: title.clone(),
                })
                .collect();
            if let Some(embedder) = dedup.embedder.get() {
                match crate::semantic_dedup::find_duplicate(
                    embedder,
                    &cand.title,
                    &semantic_candidates,
                    crate::semantic_dedup::DEFAULT_THRESHOLD,
                ) {
                    Ok(Some(dup)) => {
                        st.skipped += 1;
                        info!(
                            target: "ptask::distill",
                            title = %cand.title,
                            matched = %dup.title,
                            score = dup.score,
                            "dedup skip (semantic)"
                        );
                        let ev_uuid = format!(
                            "distill-semantic-dup:{}",
                            crate::temporal_dedup::text_hash(&cand.title)
                        );
                        let payload = serde_json::json!({
                            "candidate_title": cand.title,
                            "matched_task": dup.id,
                            "matched_title": dup.title,
                            "score": dup.score,
                        });
                        if let Err(e) = event_log::record(
                            db,
                            &ev_uuid,
                            Some(&dup.id),
                            "distill.semantic_dedup",
                            &payload,
                            ctx,
                        ) {
                            warn!(target: "ptask::distill", error = %e, "semantic-dedup event failed");
                        }
                        continue;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!(target: "ptask::distill", error = %e, "semantic dedup failed — failing open");
                    }
                }
            }
        }
        let title: String = cand.title.chars().take(200).collect();
        let new = NewTask {
            title: title.clone(),
            description: cand.description.clone(),
            priority: cand.priority.clamp(1, 5),
            deadline: None,
            source_type: "distilled".into(),
            ai_confidence: 0.85,
            ai_reasoning: format!("native distill ({})", provider.name()),
        };
        let created =
            ptask_core::tasks::create_with_extensions(db, new, Extensions::default(), ctx)?;
        if let Err(e) = crate::temporal_dedup::record_seen(db, "distill-candidate", &cand.title) {
            warn!(
                target: "ptask::distill",
                error = %e,
                "task created but temporal dedup marker could not be recorded"
            );
        }
        // A task created by an earlier chunk has to dedup a later chunk's
        // candidates too, or chunking would re-introduce the duplicates the
        // 30-day universe exists to prevent.
        dedup.existing.push((created.id, title));
        st.created += 1;
    }
    Ok(())
}

/// One native run. `batch` bounds how many inbox rows are consumed.
///
/// The batch is walked in chunks with per-chunk failure isolation: a chunk
/// the provider cannot handle is halved until the offending row is alone,
/// that row is charged an attempt, and every other chunk still completes. A
/// row that fails `MAX_DISTILL_ATTEMPTS` times is quarantined out of the
/// queue. Only provider/classification failures are chargeable; a database
/// failure during task creation is not.
///
/// A run in which nothing got through still fails closed. Note what that does
/// NOT mean: an attempt is charged whether or not anything else succeeded this
/// run, so a total provider outage charges every row it bisects down to (~31
/// captures at the current CHUNK / MAX_PROVIDER_CALLS settings). That is
/// bounded and recoverable rather than prevented — quarantined rows are
/// retained and countable via `pt_distill_quarantined_captures`. See the
/// "Poison captures and quarantine" section of docs/operations.md.
pub fn run_native<P: LlmProvider>(db: &Db, provider: &P, batch: usize) -> Result<NativeReport> {
    let start = std::time::Instant::now();
    let ctx = EventCtx::system("distill");

    provider
        .preflight()
        .context("provider preflight failed — nothing consumed")?;

    let items = ptask_core::raw_items::fetch_unprocessed(db, batch)?;
    if items.is_empty() {
        let report = NativeReport {
            consumed: 0,
            kept: 0,
            created: 0,
            skipped_dedup: 0,
            failed: 0,
            quarantined: ptask_core::raw_items::quarantined_count(db)? as usize,
            provider: provider.name().into(),
            duration_ms: start.elapsed().as_millis(),
        };
        record_run(db, &ctx, &report, true)?;
        return Ok(report);
    }

    let mut dedup = Dedup::load(db)?;
    let mut st = RunState::default();
    for chunk in items.chunks(CHUNK) {
        walk_chunk(db, provider, chunk, &mut dedup, &mut st, &ctx);
    }
    if st.budget_exhausted {
        warn!(
            target: "ptask::distill",
            calls = st.calls,
            max = MAX_PROVIDER_CALLS,
            "provider call budget exhausted — remaining rows deferred to the next run"
        );
    }

    for id in &st.consumed_ids {
        ptask_core::raw_items::mark_processed(db, *id)?;
    }
    for (id, reason) in &st.failures {
        match ptask_core::raw_items::record_distill_failure(db, *id, reason) {
            Ok(attempts) if attempts >= ptask_core::raw_items::MAX_DISTILL_ATTEMPTS => {
                warn!(
                    target: "ptask::distill",
                    raw_item = id,
                    attempts,
                    error = %reason,
                    "capture quarantined out of the distill queue"
                );
                let payload = serde_json::json!({
                    "raw_item_id": id,
                    "attempts": attempts,
                    "error": reason,
                });
                if let Err(e) = event_log::record(
                    db,
                    &format!("distill-quarantine:{id}"),
                    None,
                    "distill.quarantined",
                    &payload,
                    &ctx,
                ) {
                    warn!(target: "ptask::distill", error = %e, "quarantine event failed");
                }
            }
            Ok(_) => {}
            Err(e) => {
                warn!(target: "ptask::distill", error = %e, raw_item = id, "could not charge a distill failure");
            }
        }
    }

    // Nothing at all got through. Attempts are charged (above) so the queue
    // still advances, but the run itself stays FAIL CLOSED: the caller
    // records `distill.failed` and exits non-zero, which is what the May-2026
    // silent-zero incident bought us.
    if st.consumed_ids.is_empty() {
        bail!(
            "{}",
            st.first_error
                .unwrap_or_else(|| "provider produced no usable output".into())
        );
    }

    let report = NativeReport {
        consumed: st.consumed_ids.len(),
        kept: st.kept,
        created: st.created,
        skipped_dedup: st.skipped,
        failed: st.failures.len() + st.deferred,
        quarantined: ptask_core::raw_items::quarantined_count(db)? as usize,
        provider: provider.name().into(),
        duration_ms: start.elapsed().as_millis(),
    };
    record_run(db, &ctx, &report, true)?;
    if st.created > 0
        && let Err(e) = ptask_core::scoring::run_once(db, false)
    {
        warn!(target: "ptask::distill", error = %e, "post-run rescore failed");
    }
    info!(
        target: "ptask::distill",
        consumed = report.consumed,
        kept = report.kept,
        created = report.created,
        skipped = report.skipped_dedup,
        failed = report.failed,
        quarantined = report.quarantined,
        "native distill run complete"
    );
    Ok(report)
}

/// Record the manifest event. Success uses `distill.run` so the existing
/// freshness gauge/alerting sees native runs without changes.
pub fn record_run(db: &Db, ctx: &EventCtx, report: &NativeReport, success: bool) -> Result<()> {
    let event_type = if success {
        "distill.run"
    } else {
        "distill.failed"
    };
    let payload = serde_json::json!({
        "native": true,
        "consumed": report.consumed,
        "kept": report.kept,
        "created": report.created,
        "skipped_dedup": report.skipped_dedup,
        "failed": report.failed,
        "quarantined": report.quarantined,
        "provider": report.provider,
        "duration_ms": report.duration_ms,
    });
    let uuid = format!("distill-native:{}", uuid::Uuid::new_v4());
    event_log::record(db, &uuid, None, event_type, &payload, ctx)?;
    Ok(())
}

/// Record a failure manifest (provider/preflight errors happen before a
/// report exists).
pub fn record_failure(db: &Db, provider: &str, error: &str) {
    let ctx = EventCtx::system("distill");
    let payload = serde_json::json!({
        "native": true,
        "provider": provider,
        "error": error,
    });
    let uuid = format!("distill-native:{}", uuid::Uuid::new_v4());
    if let Err(e) = event_log::record(db, &uuid, None, "distill.failed", &payload, &ctx) {
        warn!(target: "ptask::distill", error = %e, "failed to record distill.failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{Candidate, Classification, MockProvider};

    struct IndexedProvider {
        verdicts: Vec<Classification>,
    }

    impl LlmProvider for IndexedProvider {
        fn classify_batch(&self, _texts: &[String]) -> Result<Vec<Classification>> {
            Ok(self.verdicts.clone())
        }

        fn consolidate(&self, _items: &[String]) -> Result<Vec<Candidate>> {
            panic!("malformed verdicts must fail before consolidation")
        }

        fn preflight(&self) -> Result<()> {
            Ok(())
        }

        fn name(&self) -> &'static str {
            "indexed-test"
        }
    }

    /// Fails any classify call whose batch contains `poison` — the shape of a
    /// memo that trips a Gemini safety filter (no `parts[0].text` in the
    /// response), which used to fail the whole 200-row batch.
    struct PoisonProvider {
        poison: &'static str,
    }

    impl LlmProvider for PoisonProvider {
        fn classify_batch(&self, texts: &[String]) -> Result<Vec<Classification>> {
            if texts.iter().any(|t| t.contains(self.poison)) {
                anyhow::bail!("no text part in response");
            }
            Ok(texts
                .iter()
                .enumerate()
                .map(|(idx, _)| Classification {
                    idx,
                    keep: true,
                    confidence: 1.0,
                    reason: "poison-test".into(),
                })
                .collect())
        }

        fn consolidate(&self, items: &[String]) -> Result<Vec<Candidate>> {
            Ok(items
                .iter()
                .map(|t| Candidate {
                    title: t.clone(),
                    priority: 2,
                    description: String::new(),
                })
                .collect())
        }

        fn preflight(&self) -> Result<()> {
            Ok(())
        }

        fn name(&self) -> &'static str {
            "poison-test"
        }
    }

    fn fresh_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        (dir, Db::open(&path).unwrap())
    }

    fn attempts(db: &Db, text: &str) -> i64 {
        db.with_conn(|c| {
            Ok(c.query_row(
                "SELECT distill_attempts FROM raw_items WHERE text = ?1",
                [text],
                |r| r.get(0),
            )?)
        })
        .unwrap()
    }

    fn seed_inbox(db: &Db, texts: &[&str]) {
        for t in texts {
            ptask_core::raw_items::insert(db, t, "test", "test://x").unwrap();
        }
    }

    #[test]
    fn happy_path_creates_deduped_tasks_and_marks_processed() {
        let (_dir, db) = fresh_db();
        seed_inbox(
            &db,
            &[
                "email Alan about the GPU quote",
                "pure noise line",
                "book the flight to Reykjavik",
            ],
        );
        // An existing task that should dedup one candidate away.
        ptask_core::tasks::create(
            &db,
            NewTask::minimal("Email Alan about the GPU quote today"),
            &EventCtx::test(),
        )
        .unwrap();

        let provider = MockProvider {
            broken: false,
            emit: vec![
                Candidate {
                    title: "Email Alan about the GPU quote".into(),
                    priority: 3,
                    description: String::new(),
                },
                Candidate {
                    title: "Book the Reykjavik flight".into(),
                    priority: 2,
                    description: "carry-on only".into(),
                },
            ],
        };
        let report = run_native(&db, &provider, 100).unwrap();
        assert_eq!(report.consumed, 3);
        assert_eq!(report.kept, 2, "noise line dropped by classifier");
        assert_eq!(report.skipped_dedup, 1, "Alan candidate deduped");
        assert_eq!(report.created, 1);
        assert_eq!(
            ptask_core::raw_items::unprocessed_count(&db).unwrap(),
            0,
            "whole batch consumed"
        );
        // Manifest event landed as distill.run (gauge compatibility).
        db.with_conn(|c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM pt_event_log WHERE event_type='distill.run'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(n, 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn broken_provider_fails_closed_and_consumes_nothing() {
        let (_dir, db) = fresh_db();
        seed_inbox(&db, &["a real commitment to call the bank"]);
        let provider = MockProvider {
            broken: true,
            emit: vec![],
        };
        let err = run_native(&db, &provider, 100).unwrap_err();
        assert!(err.to_string().contains("preflight"));
        assert_eq!(
            ptask_core::raw_items::unprocessed_count(&db).unwrap(),
            1,
            "nothing consumed on failure"
        );
    }

    #[test]
    fn malformed_verdict_indices_fail_without_consuming_input() {
        for verdicts in [
            vec![
                Classification {
                    idx: 0,
                    keep: true,
                    confidence: 1.0,
                    reason: String::new(),
                },
                Classification {
                    idx: 0,
                    keep: true,
                    confidence: 1.0,
                    reason: String::new(),
                },
            ],
            vec![
                Classification {
                    idx: 0,
                    keep: true,
                    confidence: 1.0,
                    reason: String::new(),
                },
                Classification {
                    idx: 2,
                    keep: true,
                    confidence: 1.0,
                    reason: String::new(),
                },
            ],
        ] {
            let (_dir, db) = fresh_db();
            seed_inbox(&db, &["first commitment", "second commitment"]);
            let err = run_native(&db, &IndexedProvider { verdicts }, 100).unwrap_err();
            assert!(
                err.to_string().contains("verdict index"),
                "unexpected error: {err:#}"
            );
            assert_eq!(
                ptask_core::raw_items::unprocessed_count(&db).unwrap(),
                2,
                "malformed response must consume nothing"
            );
        }
    }

    #[test]
    fn recent_terminal_task_cutoff_compares_instants_across_offsets() {
        let (_dir, db) = fresh_db();
        let recent =
            ptask_core::tasks::create(&db, NewTask::minimal("recent terminal"), &EventCtx::test())
                .unwrap();
        let old =
            ptask_core::tasks::create(&db, NewTask::minimal("old terminal"), &EventCtx::test())
                .unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET status='done', status_v2='done',
                                  updated_at='2026-07-01T11:30:00Z'
                  WHERE id=?1",
                [&recent.id],
            )?;
            c.execute(
                "UPDATE tasks SET status='done', status_v2='done',
                                  updated_at='2026-07-01T10:30:00Z'
                  WHERE id=?1",
                [&old.id],
            )?;
            Ok(())
        })
        .unwrap();

        // 12:00 BST is 11:00 UTC. The recent row is 30 minutes newer even
        // though both UTC hour fields sort below the cutoff's local hour.
        let rows = existing_tasks_since(&db, "2026-07-01T12:00:00+01:00").unwrap();
        let ids: Vec<&str> = rows.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&recent.id.as_str()));
        assert!(!ids.contains(&old.id.as_str()));
    }

    #[test]
    fn failed_task_creation_does_not_poison_temporal_dedup() {
        let (_dir, db) = fresh_db();
        seed_inbox(&db, &["call the supplier about the replacement part"]);
        let provider = MockProvider {
            broken: false,
            emit: vec![Candidate {
                title: "Call the supplier".into(),
                priority: 3,
                description: String::new(),
            }],
        };

        db.with_conn(|c| {
            c.execute_batch(
                "CREATE TRIGGER reject_distilled_task
                 BEFORE INSERT ON tasks
                 WHEN NEW.source_type = 'distilled'
                 BEGIN
                   SELECT RAISE(ABORT, 'simulated task insert failure');
                 END;",
            )?;
            Ok(())
        })
        .unwrap();

        let error = run_native(&db, &provider, 100).unwrap_err();
        assert!(error.to_string().contains("simulated task insert failure"));
        assert_eq!(ptask_core::raw_items::unprocessed_count(&db).unwrap(), 1);
        assert!(
            !crate::temporal_dedup::is_temporal_duplicate(
                &db,
                "distill-candidate",
                "Call the supplier",
                7,
            )
            .unwrap(),
            "a failed create must remain eligible for retry"
        );

        db.with_conn(|c| {
            c.execute_batch("DROP TRIGGER reject_distilled_task;")?;
            Ok(())
        })
        .unwrap();

        let report = run_native(&db, &provider, 100).unwrap();
        assert_eq!(report.created, 1);
        assert_eq!(ptask_core::raw_items::unprocessed_count(&db).unwrap(), 0);
        assert!(
            crate::temporal_dedup::is_temporal_duplicate(
                &db,
                "distill-candidate",
                "Call the supplier",
                7,
            )
            .unwrap(),
            "a successful create should record the temporal marker"
        );
    }

    /// Regression (#37.3): the whole `fetch_unprocessed` batch went to the
    /// provider in one call, so a single unclassifiable capture failed the
    /// run, nothing was marked processed, and the same oldest-first rows were
    /// re-served forever. The batch is now chunked and a failing chunk is
    /// halved until the offender is alone — everything else still lands.
    #[test]
    fn one_poison_capture_does_not_wedge_the_rest_of_the_batch() {
        let (_dir, db) = fresh_db();
        seed_inbox(
            &db,
            &[
                "call the bank about the mandate",
                "REDACTED trips the safety filter",
                "book the Reykjavik flight",
                "renew the office lease",
            ],
        );

        let report = run_native(&db, &PoisonProvider { poison: "REDACTED" }, 100).unwrap();

        assert_eq!(report.consumed, 3, "three good captures still processed");
        assert_eq!(report.created, 3);
        assert_eq!(report.failed, 1);
        assert_eq!(report.quarantined, 0, "one strike is not a quarantine yet");
        assert_eq!(
            ptask_core::raw_items::unprocessed_count(&db).unwrap(),
            1,
            "only the poison row is left"
        );
        assert_eq!(attempts(&db, "REDACTED trips the safety filter"), 1);
    }

    /// The poison row must not become a permanent head-of-queue block either:
    /// once it has failed in isolation `MAX_DISTILL_ATTEMPTS` times it stops
    /// being served, and a later capture behind it distills normally.
    #[test]
    fn a_repeatedly_unprocessable_capture_is_quarantined_out_of_the_queue() {
        let (_dir, db) = fresh_db();
        seed_inbox(&db, &["REDACTED trips the safety filter"]);
        let provider = PoisonProvider { poison: "REDACTED" };

        for _ in 0..ptask_core::raw_items::MAX_DISTILL_ATTEMPTS {
            // Nothing gets through while it is the only row, so the run still
            // fails closed — but each run charges the row an attempt.
            assert!(run_native(&db, &provider, 100).is_err());
        }
        assert_eq!(
            attempts(&db, "REDACTED trips the safety filter"),
            ptask_core::raw_items::MAX_DISTILL_ATTEMPTS
        );
        assert_eq!(ptask_core::raw_items::quarantined_count(&db).unwrap(), 1);

        // A capture that arrives afterwards is no longer stuck behind it.
        seed_inbox(&db, &["email Alan the revised quote"]);
        let report = run_native(&db, &provider, 100).unwrap();
        assert_eq!(report.consumed, 1);
        assert_eq!(report.created, 1);
        assert_eq!(report.quarantined, 1, "the poison row stays parked");
    }

    /// A *local* failure (database fault) is never the capture's fault:
    /// charging it would quarantine good work during an unrelated outage.
    #[test]
    fn a_database_failure_is_not_charged_to_the_capture() {
        let (_dir, db) = fresh_db();
        seed_inbox(&db, &["call the supplier about the replacement part"]);
        let provider = MockProvider {
            broken: false,
            emit: vec![Candidate {
                title: "Call the supplier".into(),
                priority: 3,
                description: String::new(),
            }],
        };
        db.with_conn(|c| {
            c.execute_batch(
                "CREATE TRIGGER reject_distilled_task
                 BEFORE INSERT ON tasks
                 WHEN NEW.source_type = 'distilled'
                 BEGIN
                   SELECT RAISE(ABORT, 'simulated task insert failure');
                 END;",
            )?;
            Ok(())
        })
        .unwrap();

        assert!(run_native(&db, &provider, 100).is_err());
        assert_eq!(
            attempts(&db, "call the supplier about the replacement part"),
            0,
            "a database fault must not push a good capture toward quarantine"
        );
    }

    /// Chunking must not resurrect the duplicates the 30-day dedup universe
    /// exists to prevent: a task created by one chunk has to dedup the next
    /// chunk's candidates too.
    #[test]
    fn a_task_created_by_an_earlier_chunk_dedups_a_later_one() {
        let (_dir, db) = fresh_db();
        // Two chunks' worth of rows, all consolidating to the same title.
        let filler: Vec<String> = (0..CHUNK + 1)
            .map(|i| format!("renew the office lease reminder {i}"))
            .collect();
        for t in &filler {
            ptask_core::raw_items::insert(&db, t, "test", "test://x").unwrap();
        }
        let provider = MockProvider {
            broken: false,
            emit: vec![Candidate {
                title: "Renew the office lease".into(),
                priority: 2,
                description: String::new(),
            }],
        };

        let report = run_native(&db, &provider, 200).unwrap();
        assert_eq!(report.consumed, CHUNK + 1);
        assert_eq!(report.created, 1, "the second chunk deduped, not recreated");
        assert!(report.skipped_dedup >= 1);
    }

    #[test]
    fn title_similarity_gate() {
        assert!(title_similar(
            "Email Alan about the GPU quote",
            "email alan about that gpu quote today"
        ));
        assert!(!title_similar(
            "Renew the office lease",
            "Book flights to Reykjavik"
        ));
        assert!(!title_similar(
            "Email Alan about the GPU quote",
            "Email Alan about the VAT return"
        ));
        assert!(title_similar("Pay VAT tax", "pay vat tax"));
        assert!(!title_similar(
            "Send the report to the tax team",
            "Send the invoice to the ops team"
        ));
    }
}
