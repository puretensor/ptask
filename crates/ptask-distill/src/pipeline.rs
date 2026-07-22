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

/// One native run. `batch` bounds how many inbox rows are consumed.
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
            provider: provider.name().into(),
            duration_ms: start.elapsed().as_millis(),
        };
        record_run(db, &ctx, &report, true)?;
        return Ok(report);
    }

    let texts: Vec<String> = items.iter().map(|i| i.text.clone()).collect();
    let verdicts = provider.classify_batch(&texts)?;
    let kept = select_kept_texts(&texts, &verdicts)?;

    let mut created = 0usize;
    let mut skipped = 0usize;
    if !kept.is_empty() {
        let candidates = provider.consolidate(&kept)?;

        // Dedup universe: everything active + anything touched in 30 days
        // (incl. done/dismissed — the operator said no once already).
        let cutoff = ptask_core::dates::format_iso(
            &ptask_core::dates::now_in_operator_tz()?
                .checked_sub(ptask_core::jiff::Span::new().days(30))
                .map_err(|e| anyhow::anyhow!("cutoff math: {e}"))?,
        );
        let existing = existing_tasks_since(db, &cutoff)?;

        // v2.5.0: semantic layer over the Jaccard gate. The embedder loads
        // once per run; a load failure degrades to Jaccard-only (fail open).
        #[cfg(feature = "native-ml")]
        let embedder = match crate::embeddings::Embedder::from_hf_cache() {
            Ok(e) => Some(e),
            Err(e) => {
                warn!(target: "ptask::distill", error = %e, "embedder unavailable — Jaccard-only dedup this run");
                None
            }
        };
        #[cfg(feature = "native-ml")]
        let semantic_candidates: Vec<crate::semantic_dedup::Candidate> = existing
            .iter()
            .map(|(id, title)| crate::semantic_dedup::Candidate {
                id: id.clone(),
                title: title.clone(),
            })
            .collect();

        for cand in candidates {
            if existing.iter().any(|(_, t)| title_similar(t, &cand.title)) {
                skipped += 1;
                info!(target: "ptask::distill", title = %cand.title, "dedup skip (jaccard)");
                continue;
            }
            // Exact-hash temporal dedup: the same candidate text distilled
            // twice inside 7 days is a re-ingest, not new work. Only record
            // the candidate after task creation succeeds; otherwise a
            // transient database failure would make the retry disappear.
            match crate::temporal_dedup::is_temporal_duplicate(
                db,
                "distill-candidate",
                &cand.title,
                7,
            ) {
                Ok(true) => {
                    skipped += 1;
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
            if let Some(embedder) = embedder.as_ref() {
                match crate::semantic_dedup::find_duplicate(
                    embedder,
                    &cand.title,
                    &semantic_candidates,
                    crate::semantic_dedup::DEFAULT_THRESHOLD,
                ) {
                    Ok(Some(dup)) => {
                        skipped += 1;
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
                            &ctx,
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
            let new = NewTask {
                title: cand.title.chars().take(200).collect(),
                description: cand.description.clone(),
                priority: cand.priority.clamp(1, 5),
                deadline: None,
                source_type: "distilled".into(),
                ai_confidence: 0.85,
                ai_reasoning: format!("native distill ({})", provider.name()),
            };
            ptask_core::tasks::create_with_extensions(db, new, Extensions::default(), &ctx)?;
            if let Err(e) = crate::temporal_dedup::record_seen(db, "distill-candidate", &cand.title)
            {
                warn!(
                    target: "ptask::distill",
                    error = %e,
                    "task created but temporal dedup marker could not be recorded"
                );
            }
            created += 1;
        }
    }

    // Mark the whole batch consumed — kept items became candidates; dropped
    // items were judged noise. Either way they're handled.
    for item in &items {
        ptask_core::raw_items::mark_processed(db, item.id)?;
    }

    let report = NativeReport {
        consumed: items.len(),
        kept: kept.len(),
        created,
        skipped_dedup: skipped,
        provider: provider.name().into(),
        duration_ms: start.elapsed().as_millis(),
    };
    record_run(db, &ctx, &report, true)?;
    if created > 0
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

    fn fresh_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        (dir, Db::open(&path).unwrap())
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
