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
    /// Candidates the provider returned without `sources` this run. They
    /// cover nothing, so their captures bisect and burn calls: a non-zero
    /// value means the model is ignoring the schema.
    pub sourceless_candidates: usize,
    pub provider: String,
    pub duration_ms: u128,
}

/// Whether a completed chunk's raw rows can be marked processed.
#[derive(Debug, PartialEq)]
pub enum ChunkDisposition {
    Consume,
    Retain,
}

/// Keep signal-bearing input when consolidation unexpectedly covers none of
/// it; consuming it would silently destroy work while reporting it as
/// successfully kept. Noise is still consumed. `covered_len` is the number of
/// kept captures some created or deduped candidate covers — when it is
/// non-zero the covered captures are consumed and the uncovered remainder is
/// walked again (see `process_chunk`).
pub fn chunk_disposition(kept_len: usize, covered_len: usize) -> ChunkDisposition {
    if kept_len > 0 && covered_len == 0 {
        ChunkDisposition::Retain
    } else {
        ChunkDisposition::Consume
    }
}

/// Lowercase word set for the similarity gates. Date/time tokens and month
/// names are left out — they are compared by value through [`DateFacts`]
/// instead (see [`identifiers_conflict`]): "… by 5pm" is the same task.
fn title_tokens(s: &str) -> std::collections::HashSet<String> {
    analyse_title(s).0.into_iter().collect()
}

/// Two titles name different things when their identifier tokens — any
/// token containing a digit (`4411`, `n1` from `fox-n1`, `host7`, `v2`) —
/// differ. Word overlap and embeddings both score "pay invoice 4411" and
/// "pay invoice 4412" as near-identical, which deduped real new work away
/// (even against done tasks). Fails toward creating a possible duplicate.
///
/// Date and time tokens are not identifiers, but they are not ignored
/// either: a date/time value present in only ONE title is ignored ("… by
/// 5pm" is the same task), while differing values of the same kind in BOTH
/// titles are a conflict — 2025 vs 2026, 3pm vs 4pm, 5 Oct vs 6 Oct, March
/// vs April, 2026-10-06 vs 2026-11-06, "12 for March" vs "13 for March". The
/// dedup universe includes done tasks, so a missed conflict silently loses
/// this year's commitment to last year's; when in doubt, keep both tasks.
fn identifiers_conflict(a: &str, b: &str) -> bool {
    let (ta, fa) = analyse_title(a);
    let (tb, fb) = analyse_title(b);
    let ids = |t: &[String]| {
        t.iter()
            .filter(|w| w.chars().any(|c| c.is_numeric()))
            .cloned()
            .collect::<std::collections::HashSet<_>>()
    };
    ids(&ta) != ids(&tb) || fa.conflicts(&fb)
}

/// Normalised date/time values in a title, by kind.
#[derive(Default)]
struct DateFacts {
    years: std::collections::BTreeSet<u16>,
    /// (hour 0-23, minute)
    times: std::collections::BTreeSet<(u8, u8)>,
    months: std::collections::BTreeSet<u8>,
    /// (month, day) — a day next to a month name, or from a numeric date.
    days: std::collections::BTreeSet<(u8, u8)>,
    /// Ordinal days with no month ("on the 5th").
    bare_days: std::collections::BTreeSet<u8>,
    /// Every day-like number dropped from the tokens because a month name
    /// is nearby ("12 for March"), so dropping it never hides a difference.
    date_numbers: std::collections::BTreeSet<u8>,
}

impl DateFacts {
    /// Both sides carry a value of the same kind and the values differ.
    fn conflicts(&self, other: &Self) -> bool {
        fn differ<T: Ord>(
            a: &std::collections::BTreeSet<T>,
            b: &std::collections::BTreeSet<T>,
        ) -> bool {
            !a.is_empty() && !b.is_empty() && a != b
        }
        differ(&self.years, &other.years)
            || differ(&self.times, &other.times)
            || differ(&self.months, &other.months)
            || differ(&self.days, &other.days)
            || differ(&self.bare_days, &other.bare_days)
            || differ(&self.date_numbers, &other.date_numbers)
    }
}

/// `yyyy-mm-dd`, `dd/mm/yyyy`, `dd/mm/yy` or `d/m` (day first: the operator
/// is in the UK) → (year, month, day).
fn numeric_date(w: &str) -> Option<(Option<u16>, u8, u8)> {
    let valid = |m: u8, d: u8| (1..=12).contains(&m) && (1..=31).contains(&d);
    let num = |p: &str| -> Option<u16> {
        (!p.is_empty() && p.len() <= 4 && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse().ok())
            .flatten()
    };
    let parts: Vec<&str> = w.split('-').collect();
    if parts.len() == 3 && is_year(parts[0]) {
        let (y, m, d) = (num(parts[0])?, num(parts[1])? as u8, num(parts[2])? as u8);
        return valid(m, d).then_some((Some(y), m, d));
    }
    let parts: Vec<&str> = w.split('/').collect();
    if (2..=3).contains(&parts.len()) && parts[..2].iter().all(|p| is_small_number(p)) {
        let (d, m) = (num(parts[0])? as u8, num(parts[1])? as u8);
        let y = match parts.get(2) {
            None => None,
            Some(y) if is_year(y) => Some(num(y)?),
            Some(y) if y.len() == 2 => Some(2000 + num(y)?),
            Some(_) => return None,
        };
        return valid(m, d).then_some((y, m, d));
    }
    None
}

/// Similarity tokens plus the date/time values taken out of them.
fn analyse_title(s: &str) -> (Vec<String>, DateFacts) {
    let mut facts = DateFacts::default();
    let mut rest = String::new();
    for word in s.split_whitespace() {
        let core = word.trim_matches(|c: char| !c.is_alphanumeric());
        if let Some((year, month, day)) = numeric_date(core) {
            facts.years.extend(year);
            facts.months.insert(month);
            facts.days.insert((month, day));
            continue;
        }
        rest.push_str(word);
        rest.push(' ');
    }
    let raw = raw_tokens(&rest);
    let month_at = |j: usize| raw.get(j).and_then(|w| month_number(w));
    let day_of = |t: &str| -> Option<u8> {
        let digits = ["st", "nd", "rd", "th"]
            .iter()
            .find_map(|x| t.strip_suffix(x))
            .unwrap_or(t);
        is_small_number(digits)
            .then(|| digits.parse().ok())
            .flatten()
            .filter(|d| (1..=31).contains(d))
    };
    let mut tokens = Vec::new();
    for (i, t) in raw.iter().enumerate() {
        if is_year(t) {
            facts.years.extend(t.parse::<u16>().ok());
            continue;
        }
        if let Some(time) = normalise_time(t) {
            facts.times.insert(time);
            continue;
        }
        if let Some(m) = month_number(t) {
            facts.months.insert(m);
            continue;
        }
        // "5 Oct", "Oct 5", "October 5th", "5th of October".
        let adjacent_month = month_at(i + 1)
            .or_else(|| i.checked_sub(1).and_then(month_at))
            .or_else(|| {
                (raw.get(i + 1).is_some_and(|w| w == "of"))
                    .then(|| month_at(i + 2))
                    .flatten()
            });
        let near_month = adjacent_month.is_some()
            || (i.saturating_sub(2)..=i + 2).any(|j| j != i && month_at(j).is_some());
        if let Some(day) = day_of(t).filter(|_| is_ordinal(t) || near_month) {
            if near_month {
                facts.date_numbers.insert(day);
            }
            match adjacent_month {
                Some(m) => {
                    facts.days.insert((m, day));
                }
                None if is_ordinal(t) && !near_month => {
                    facts.bare_days.insert(day);
                }
                None => {}
            }
            continue;
        }
        tokens.extend(t.split(':').filter(|w| !w.is_empty()).map(str::to_string));
    }
    (tokens, facts)
}

fn month_number(t: &str) -> Option<u8> {
    if !MONTHS.contains(&t) {
        return None;
    }
    const ORDER: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    ORDER
        .iter()
        .position(|m| t.starts_with(m))
        .map(|i| i as u8 + 1)
}

/// `5pm` → (17, 0), `9:30am` → (9, 30), `17:00` → (17, 0).
fn normalise_time(t: &str) -> Option<(u8, u8)> {
    if !is_time(t) {
        return None;
    }
    let (clock, pm) = match (t.strip_suffix("am"), t.strip_suffix("pm")) {
        (Some(c), _) => (c, Some(false)),
        (_, Some(c)) => (c, Some(true)),
        _ => (t, None),
    };
    let (h, m) = clock.split_once(':').unwrap_or((clock, "0"));
    let (mut h, m): (u8, u8) = (h.parse().ok()?, m.parse().ok()?);
    match pm {
        Some(true) if h < 12 => h += 12,
        Some(false) if h == 12 => h = 0,
        _ => {}
    }
    (h < 24 && m < 60).then_some((h, m))
}

/// Lowercase tokens; `:` is kept inside a token so a clock time stays whole.
fn raw_tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == ':'))
        .map(|w| w.trim_matches(':').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

const MONTHS: &[&str] = &[
    "jan",
    "january",
    "feb",
    "february",
    "mar",
    "march",
    "apr",
    "april",
    "may",
    "jun",
    "june",
    "jul",
    "july",
    "aug",
    "august",
    "sep",
    "sept",
    "september",
    "oct",
    "october",
    "nov",
    "november",
    "dec",
    "december",
];

fn is_year(t: &str) -> bool {
    t.len() == 4
        && t.bytes().all(|b| b.is_ascii_digit())
        && (t.starts_with("19") || t.starts_with("20"))
}

fn is_small_number(t: &str) -> bool {
    (1..=2).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit())
}

/// `5pm`, `9:30am`, `17:00`.
fn is_time(t: &str) -> bool {
    let (clock, meridiem) = match t.strip_suffix("am").or_else(|| t.strip_suffix("pm")) {
        Some(rest) => (rest, true),
        None => (t, false),
    };
    match clock.split_once(':') {
        Some((h, m)) => is_small_number(h) && m.len() == 2 && m.bytes().all(|b| b.is_ascii_digit()),
        None => meridiem && is_small_number(clock),
    }
}

/// `1st`, `22nd`, `3rd`, `5th`.
fn is_ordinal(t: &str) -> bool {
    ["st", "nd", "rd", "th"]
        .iter()
        .any(|suffix| t.strip_suffix(suffix).is_some_and(is_small_number))
}

/// Similarity gate: normalized token overlap (Jaccard on lowercase words),
/// never across distinct identifiers. Cheap, deterministic, no model download.
fn title_similar(a: &str, b: &str) -> bool {
    let (ta, tb) = (title_tokens(a), title_tokens(b));
    if ta.is_empty() || tb.is_empty() || identifiers_conflict(a, b) {
        return false;
    }
    let inter = ta.intersection(&tb).count() as f64;
    let union = ta.union(&tb).count() as f64;
    inter / union >= 0.6
}

/// Select kept inputs only after proving the provider returned a complete,
/// one-to-one index permutation. A response with the right length can still
/// duplicate one index and omit another; silently accepting that drops work.
fn select_kept(texts: &[String], verdicts: &[Classification]) -> Result<Vec<usize>> {
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
        if verdict.idx >= texts.len() {
            bail!(
                "provider returned out-of-range verdict index {} for {} items — failing closed",
                verdict.idx,
                texts.len()
            );
        }
        if std::mem::replace(&mut seen[verdict.idx], true) {
            bail!(
                "provider returned duplicate verdict index {} — failing closed",
                verdict.idx
            );
        }
        if verdict.keep {
            kept.push(verdict.idx);
        }
    }
    // Input order, so consolidate's item numbers follow the capture order.
    kept.sort_unstable();
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

/// Most captures one candidate may cover. Coverage is the model's word
/// alone, so without a cap a single over-merged answer (or one deduped title)
/// could consume a whole 25-row chunk. Eight comfortably fits a genuine merge
/// of repeated memos about one commitment; anything beyond is covered up to
/// the cap and the remainder walked again. That walk always covers at least
/// one more capture (so it terminates), and a repeated answer is deduped
/// against the task just created and covers the next eight, so the cap costs
/// calls, never permanent non-consumption.
const MAX_SOURCES_PER_CANDIDATE: usize = 8;

/// Ceiling on provider calls per run. Failure isolation halves a failing
/// chunk, so a pathologically bad batch could otherwise fan out to ~2N calls.
/// Hitting the ceiling ends the run early; whatever succeeded is still
/// consumed, so the next run starts from a strictly shorter queue.
const MAX_PROVIDER_CALLS: usize = 64;

/// Wall-clock ceiling on the chunk walk. A provider call can take ~92s (three
/// 30s attempts plus backoff), so 64 slow calls ran ~97 min, while the unit
/// kills the run at 30 min, before any row was marked processed, which threw
/// the finished chunks away. No new call starts past this budget. The one in
/// flight plus a consolidate still finishes well inside the unit timeout.
const RUN_WALL_BUDGET: std::time::Duration = std::time::Duration::from_secs(20 * 60);

/// Dedup universe for one run: everything active + anything touched in 30
/// days (incl. done/dismissed — the operator said no once already). Shared
/// across chunks and extended in place as tasks are created, so two chunks in
/// the same run can't create the same task twice.
struct Dedup {
    existing: Vec<(String, String)>,
    #[cfg(feature = "native-ml")]
    embedder: LazyEmbedder,
    /// Embeddings of `existing`, index-aligned, computed once per run on the
    /// first semantic check. Re-embedding the whole 30-day universe for every
    /// candidate cost candidates x N MiniLM passes (one padded N-row batch
    /// each) per run.
    #[cfg(feature = "native-ml")]
    existing_vecs: Option<Vec<Vec<f32>>>,
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
            #[cfg(feature = "native-ml")]
            existing_vecs: None,
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
            // Cache-only, like the capture fast lane (PT-2121/6): the
            // downloading loader has no timeout, so a cold cache and a stalled
            // huggingface.co hung the run until the unit's TimeoutStartSec
            // instead of degrading to Jaccard.
            self.inner = match crate::embeddings::Embedder::from_local_hf_cache() {
                Ok(e) => Some(e),
                Err(e) => {
                    warn!(target: "ptask::distill", error = %e, "embedder unavailable — Jaccard-only dedup this run");
                    None
                }
            };
        }
        self.inner.as_ref()
    }

    /// Stop semantic dedup for the rest of the run (an embed call failed).
    fn disable(&mut self) {
        self.inner = None;
    }
}

/// Embed batch for the per-run universe: bounds the padded attention
/// tensor (batch x heads x L^2) instead of one N-row batch.
#[cfg(feature = "native-ml")]
const EMBED_BATCH: usize = 64;

/// Closest existing task to a title, plus the title's own vector (kept so a
/// created task can join the universe without a re-embed).
#[cfg(feature = "native-ml")]
#[derive(Default)]
struct SemanticCheck {
    /// Index into `Dedup::existing` and its cosine score.
    best: Option<(usize, f32)>,
    vec: Option<Vec<f32>>,
}

/// Empty when no embedder is available.
#[cfg(feature = "native-ml")]
fn semantic_match(dedup: &mut Dedup, title: &str) -> Result<SemanticCheck> {
    let Some(embedder) = dedup.embedder.get() else {
        return Ok(SemanticCheck::default());
    };
    if dedup.existing_vecs.is_none() {
        let titles: Vec<&str> = dedup.existing.iter().map(|(_, t)| t.as_str()).collect();
        let mut vecs = Vec::with_capacity(titles.len());
        for batch in titles.chunks(EMBED_BATCH) {
            vecs.extend(embedder.embed(batch)?);
        }
        dedup.existing_vecs = Some(vecs);
    }
    let vec = embedder
        .embed(&[title])?
        .pop()
        .ok_or_else(|| anyhow::anyhow!("embedder returned no vector"))?;
    let best = dedup
        .existing_vecs
        .as_deref()
        .and_then(|vecs| crate::semantic_dedup::best_match(&vec, vecs));
    Ok(SemanticCheck {
        best,
        vec: Some(vec),
    })
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
    /// Candidates returned without `sources` (see `NativeReport`).
    sourceless: usize,
    /// No new provider call starts after this instant.
    stop_at: Option<std::time::Instant>,
    budget_exhausted: bool,
    /// The provider became unavailable mid-run (see `ProviderUnavailable`):
    /// no further call starts and nothing more is charged.
    aborted: bool,
    /// The one failing path a server-class error may be blamed on this run:
    /// the ids of the most recent chunk on it. Set after a healthy
    /// re-preflight and narrowed as bisection follows the failure down.
    suspect: Option<std::collections::HashSet<i64>>,
    /// Charges from that path, applied only if the run does not abort.
    provisional_failures: Vec<(i64, String)>,
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
    /// The provider itself is down/rate-limited/timing out. Bisecting would
    /// only multiply calls against the outage, so the run stops instead.
    abort: bool,
    /// The outage class, when the provider reported one.
    outage: Option<crate::providers::FailureClass>,
    /// Blamed on the input only provisionally (a server-class error after a
    /// healthy re-preflight): the charge is dropped if the run later aborts.
    provisional: bool,
}

impl ChunkError {
    fn provider(e: anyhow::Error) -> Self {
        let outage = e
            .downcast_ref::<crate::providers::ProviderUnavailable>()
            .map(|p| p.class);
        Self {
            reason: format!("{e:#}"),
            chargeable: outage.is_none(),
            abort: outage.is_some(),
            outage,
            provisional: false,
        }
    }

    /// A local database fault is never the capture's fault — charging it
    /// would quarantine good work during an unrelated outage.
    fn local(e: anyhow::Error) -> Self {
        Self {
            reason: format!("{e:#}"),
            chargeable: false,
            abort: false,
            outage: None,
            provisional: false,
        }
    }
}

/// Classify one chunk, consolidate what it kept, and create the survivors.
/// Any error here is the chunk's error: the caller isolates it.
///
/// On success, returns the kept captures no created or deduped candidate
/// covers. They are NOT consumed — the caller walks them again as a smaller
/// chunk — so a model that merges too eagerly or stops early can never make
/// a commitment disappear while the run reports it as handled.
fn process_chunk<P: LlmProvider + ?Sized>(
    db: &Db,
    provider: &P,
    items: &[ptask_core::raw_items::RawItem],
    dedup: &mut Dedup,
    st: &mut RunState,
    ctx: &EventCtx,
) -> std::result::Result<Vec<ptask_core::raw_items::RawItem>, ChunkError> {
    st.calls += 1;
    let texts: Vec<String> = items.iter().map(|i| i.text.clone()).collect();
    let verdicts = provider
        .classify_batch(&texts)
        .map_err(ChunkError::provider)?;
    let kept = select_kept(&texts, &verdicts).map_err(ChunkError::provider)?;
    let mut covered = vec![false; kept.len()];
    if !kept.is_empty() {
        st.calls += 1;
        let kept_texts: Vec<String> = kept.iter().map(|&i| texts[i].clone()).collect();
        let mut candidates = provider
            .consolidate(&kept_texts)
            .map_err(ChunkError::provider)?;
        for cand in &mut candidates {
            if cand.sources.is_empty() {
                st.sourceless += 1;
            }
            // With one kept capture there is nothing else a task can be from.
            // Applied before the range check, so a model that numbers from 1
            // still resolves lone captures (bisection gets there) instead of
            // quarantining everything.
            if kept.len() == 1 {
                cand.sources = vec![0];
                continue;
            }
            if let Some(&bad) = cand.sources.iter().find(|&&i| i >= kept.len()) {
                return Err(ChunkError::provider(anyhow::anyhow!(
                    "provider returned out-of-range source index {bad} for {} kept items — failing closed",
                    kept.len()
                )));
            }
            cand.sources.sort_unstable();
            cand.sources.dedup();
            if cand.sources.len() > MAX_SOURCES_PER_CANDIDATE {
                warn!(
                    target: "ptask::distill",
                    title = %cand.title,
                    claimed = cand.sources.len(),
                    cap = MAX_SOURCES_PER_CANDIDATE,
                    "candidate claims more captures than one task plausibly merges — \
                     covering only the cap; the rest go round again"
                );
                cand.sources.truncate(MAX_SOURCES_PER_CANDIDATE);
            }
        }
        create_candidates(db, provider, candidates, &mut covered, dedup, st, ctx)
            .map_err(ChunkError::local)?;
    }
    let covered_len = covered.iter().filter(|&&c| c).count();
    if chunk_disposition(kept.len(), covered_len) == ChunkDisposition::Retain {
        // Preserve the input, but use the same isolation and bounded retry
        // path as other provider failures. Returning success leaves the
        // oldest captures eligible forever and can starve the queue.
        // Nothing is consumed here: bisection reclassifies each child,
        // including noise, and accounts for it exactly once.
        return Err(ChunkError::provider(anyhow::anyhow!(
            "consolidation covered none of {} kept captures",
            kept.len()
        )));
    }
    // Noise was deliberately dropped and covered captures became (or match)
    // tasks: both are handled. Uncovered kept captures go round again.
    let mut uncovered = Vec::new();
    let mut kept_pos = kept.iter().zip(&covered).peekable();
    for (idx, item) in items.iter().enumerate() {
        match kept_pos.peek() {
            Some(&(&k, &is_covered)) if k == idx => {
                kept_pos.next();
                if is_covered {
                    st.kept += 1;
                    st.consumed_ids.push(item.id);
                } else {
                    uncovered.push(item.clone());
                }
            }
            _ => st.consumed_ids.push(item.id),
        }
    }
    Ok(uncovered)
}

/// Walk a chunk, halving it on failure so a single unprocessable row is
/// isolated instead of taking its neighbours down with it.
fn walk_chunk<P: LlmProvider + ?Sized>(
    db: &Db,
    provider: &P,
    items: &[ptask_core::raw_items::RawItem],
    dedup: &mut Dedup,
    st: &mut RunState,
    ctx: &EventCtx,
) {
    if items.is_empty() || st.aborted {
        return;
    }
    if st.calls >= MAX_PROVIDER_CALLS || st.stop_at.is_some_and(|t| std::time::Instant::now() >= t)
    {
        st.budget_exhausted = true;
        return;
    }
    let e = match process_chunk(db, provider, items, dedup, st, ctx) {
        Ok(uncovered) if uncovered.is_empty() => return,
        Ok(uncovered) => {
            // Strictly smaller than `items` (something was covered), so this
            // terminates; the call/wall budget bounds it as well.
            warn!(
                target: "ptask::distill",
                chunk = items.len(),
                uncovered = uncovered.len(),
                "consolidation left kept captures uncovered — walking them again"
            );
            walk_chunk(db, provider, &uncovered, dedup, st, ctx);
            return;
        }
        Err(e) => e,
    };
    let mut e = e;
    if let Some(class) = e.outage {
        // A server-class error (500/502/504…) can be deterministic for one
        // input — Gemini 500s on some content, a local server 500s on context
        // overflow — and aborting on it would stall the oldest-first queue
        // forever. So it may be blamed on input, but only along ONE failing
        // path per run: the first time, a healthy re-preflight opens the
        // path; after that only a sub-chunk of the path's latest chunk (the
        // bisection following one poison row down) may fail. Any other
        // failure — a second, disjoint failing chunk, or both halves failing
        // — means the provider is flapping, not the data: abort uncharged
        // and drop the path's provisional charges. Every other class
        // (rate limit, overload, timeout, transport, auth) describes the
        // provider and always aborts.
        let ids: std::collections::HashSet<i64> = items.iter().map(|i| i.id).collect();
        let input_specific = class == crate::providers::FailureClass::Server
            && match &st.suspect {
                None => {
                    st.calls += 1;
                    match provider.preflight() {
                        Ok(()) => true,
                        Err(p) => {
                            e.reason = format!("{} (preflight also failed: {p:#})", e.reason);
                            false
                        }
                    }
                }
                Some(path) => ids.is_subset(path),
            };
        if input_specific {
            warn!(
                target: "ptask::distill",
                chunk = items.len(),
                error = %e.reason,
                "server error with a healthy provider — treating it as input-specific"
            );
            st.suspect = Some(ids);
            e.abort = false;
            e.chargeable = true;
            e.provisional = true;
        } else if !st.provisional_failures.is_empty() {
            warn!(
                target: "ptask::distill",
                dropped = st.provisional_failures.len(),
                "a second provider failure this run — the earlier one was the provider too; \
                 dropping its provisional charges"
            );
            st.provisional_failures.clear();
        }
    }
    if st.first_error.is_none() || e.abort {
        st.first_error = Some(e.reason.clone());
    }
    if e.abort {
        warn!(
            target: "ptask::distill",
            chunk = items.len(),
            error = %e.reason,
            "provider unavailable — aborting the run; remaining rows deferred, uncharged"
        );
        st.aborted = true;
        return;
    }
    if items.len() == 1 {
        warn!(
            target: "ptask::distill",
            raw_item = items[0].id,
            chargeable = e.chargeable,
            error = %e.reason,
            "isolated an unprocessable capture"
        );
        if e.chargeable && e.provisional {
            st.provisional_failures.push((items[0].id, e.reason));
        } else if e.chargeable {
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
/// gate against the shared run universe. A candidate that is created or
/// deduped (it already exists as a task) marks its `sources` as `covered`.
fn create_candidates<P: LlmProvider + ?Sized>(
    db: &Db,
    provider: &P,
    candidates: Vec<crate::providers::Candidate>,
    covered: &mut [bool],
    dedup: &mut Dedup,
    st: &mut RunState,
    ctx: &EventCtx,
) -> Result<()> {
    let mut cover = |sources: &[usize]| {
        for &i in sources {
            covered[i] = true;
        }
    };
    for mut cand in candidates {
        // A blank title is not a task. It is skipped without covering its
        // sources, so those captures go round again instead of being
        // consumed — and it never reaches the temporal hash, where every
        // later blank would "dedup" against the first.
        let trimmed = cand.title.trim();
        if trimmed.is_empty() {
            warn!(target: "ptask::distill", sources = ?cand.sources, "candidate with a blank title — skipped");
            continue;
        }
        if trimmed.len() != cand.title.len() {
            cand.title = trimmed.to_string();
        }
        if cand.sources.is_empty() {
            warn!(target: "ptask::distill", title = %cand.title, "candidate names no source captures — it covers none");
        }
        if dedup
            .existing
            .iter()
            .any(|(_, t)| title_similar(t, &cand.title))
        {
            st.skipped += 1;
            cover(&cand.sources);
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
                cover(&cand.sources);
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
        let cand_vec = match semantic_match(dedup, &cand.title) {
            Ok(SemanticCheck {
                best: Some((idx, score)),
                ..
            }) if score >= crate::semantic_dedup::DEFAULT_THRESHOLD
                && !identifiers_conflict(&dedup.existing[idx].1, &cand.title) =>
            {
                let (dup_id, dup_title) = dedup.existing[idx].clone();
                st.skipped += 1;
                cover(&cand.sources);
                info!(
                    target: "ptask::distill",
                    title = %cand.title,
                    matched = %dup_title,
                    score,
                    "dedup skip (semantic)"
                );
                let ev_uuid = format!(
                    "distill-semantic-dup:{}",
                    crate::temporal_dedup::text_hash(&cand.title)
                );
                let payload = serde_json::json!({
                    "candidate_title": cand.title,
                    "matched_task": dup_id,
                    "matched_title": dup_title,
                    "score": score,
                });
                if let Err(e) = event_log::record(
                    db,
                    &ev_uuid,
                    Some(&dup_id),
                    "distill.semantic_dedup",
                    &payload,
                    ctx,
                ) {
                    warn!(target: "ptask::distill", error = %e, "semantic-dedup event failed");
                }
                continue;
            }
            Ok(check) => check.vec,
            Err(e) => {
                warn!(target: "ptask::distill", error = %e, "semantic dedup failed — Jaccard-only for the rest of this run");
                dedup.embedder.disable();
                dedup.existing_vecs = None;
                None
            }
        };
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
        #[cfg(feature = "native-ml")]
        match (dedup.existing_vecs.as_mut(), cand_vec) {
            (Some(vecs), Some(vec)) => vecs.push(vec),
            // Keep the cache index-aligned with `existing` or drop it.
            (Some(_), None) => dedup.existing_vecs = None,
            (None, _) => {}
        }
        dedup.existing.push((created.id, title));
        st.created += 1;
        cover(&cand.sources);
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
/// A provider *outage* (rate limit, 5xx, timeout, unreachable, rejected
/// credentials — `ProviderUnavailable`) is different: it aborts the run at
/// once, without bisecting and without charging anything, and the run fails
/// closed after marking the chunks that finished. The one exception is a
/// server-class 5xx with a healthy re-preflight, which may be blamed on input
/// along a single bisection path per run (provisionally — see `walk_chunk`).
///
/// A run in which nothing got through still fails closed. Note what that does
/// NOT mean: an attempt is charged whether or not anything else succeeded this
/// run, so a provider that keeps *answering* with unusable output (a schema
/// regression, a bad model deploy) charges every row it bisects down to (~31
/// captures at the current CHUNK / MAX_PROVIDER_CALLS settings). That is
/// bounded and recoverable rather than prevented — quarantined rows are
/// retained and countable via `pt_distill_quarantined_captures`. See the
/// "Poison captures and quarantine" section of docs/operations.md.
pub fn run_native<P: LlmProvider + ?Sized>(
    db: &Db,
    provider: &P,
    batch: usize,
) -> Result<NativeReport> {
    run_native_within(db, provider, batch, RUN_WALL_BUDGET)
}

/// Another distill run holds the run lock; this one consumed nothing.
#[derive(Debug)]
pub struct DistillBusy(pub String);

impl std::fmt::Display for DistillBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "another distill run is already running (lock {}) — nothing consumed",
            self.0
        )
    }
}

impl std::error::Error for DistillBusy {}

/// Exclusive per-database run lock (`<db>.distill.lock`). Nothing claims
/// `raw_items` rows, so two concurrent runs would fetch, classify and create
/// tasks from the same captures. An OS file lock is released by the kernel
/// when its holder exits or is killed, so a crashed run can never wedge the
/// next one the way a lease row could.
fn acquire_run_lock(db: &Db) -> Result<Option<std::fs::File>> {
    match db_file_path(db.path()) {
        Some(file) => lock_at(&lock_path_for(&file), &file, open_lock_rw, open_ro).map(Some),
        None => Ok(None),
    }
}

/// Where the run lock lives, or `None` when there is no database file to
/// sit beside: `:memory:` and `file:` memory URIs are private to this
/// process, so there is nothing to serialise against. A `file:` URI locks
/// beside the file it names (query stripped), never as a literal
/// `file:…` name in the working directory.
#[cfg(test)]
fn run_lock_path(db_path: &std::path::Path) -> Option<std::path::PathBuf> {
    db_file_path(db_path).map(|f| lock_path_for(&f))
}

/// `%XX` → byte, as SQLite does for `file:` URI paths. Malformed escapes are
/// kept literally.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn lock_path_for(db_file: &std::path::Path) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}.distill.lock", db_file.display()))
}

fn open_ro(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// The database file on disk, or `None` for an in-memory database.
fn db_file_path(db_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let raw = db_path.to_string_lossy();
    let file = match raw.strip_prefix("file:") {
        Some(uri) => {
            let (path, query) = uri.split_once('?').unwrap_or((uri, ""));
            if query.split('&').any(|kv| kv == "mode=memory") {
                return None;
            }
            // file:///abs → /abs; file://localhost/abs → /abs; file:rel → rel
            let path = path
                .strip_prefix("//localhost")
                .or_else(|| path.strip_prefix("//"))
                .unwrap_or(path);
            percent_decode(path)
        }
        None => raw.into_owned(),
    };
    if file.is_empty() || file == ":memory:" {
        return None;
    }
    Some(std::path::PathBuf::from(file))
}

fn open_lock_rw(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
}

fn lock_at(
    path: &std::path::Path,
    db_file: &std::path::Path,
    open_rw: impl Fn(&std::path::Path) -> std::io::Result<std::fs::File>,
    open_ro: impl Fn(&std::path::Path) -> std::io::Result<std::fs::File>,
) -> Result<std::fs::File> {
    let path_s = path.display().to_string();
    let denied = |e: &std::io::Error| e.kind() == std::io::ErrorKind::PermissionDenied;
    let file = match open_rw(path) {
        Ok(file) => {
            // fchmod ignores the umask: a file first created under umask 077
            // (root, say) must stay openable by the timer user. Best effort —
            // we may not own an existing file.
            use std::os::unix::fs::PermissionsExt;
            let _ = file.set_permissions(std::fs::Permissions::from_mode(0o644));
            file
        }
        // Created by another user (e.g. one `sudo pt distill`): flock does
        // not need write access, so lock a read-only descriptor instead of
        // failing every run from now on.
        Err(e) if denied(&e) => match open_ro(path) {
            Ok(file) => file,
            // Not even readable (mode 0600 from another user): lock the
            // database file itself, opened read-only. Safe on a local
            // filesystem: flock(2) locks are independent of the fcntl(2)
            // byte-range locks SQLite uses, so this neither blocks nor is
            // blocked by database access — it only excludes other runs
            // taking the same fallback or the same flock.
            Err(e2) if denied(&e2) => open_ro(db_file).with_context(|| {
                format!(
                    "distill run lock {path_s} is unreadable ({e2}) and the database \
                     {} cannot be opened to lock instead",
                    db_file.display()
                )
            })?,
            Err(e2) => {
                return Err(anyhow::Error::new(e2).context(format!(
                    "open distill run lock {path_s} read-only (not writable: {e})"
                )));
            }
        },
        Err(e) => {
            return Err(anyhow::Error::new(e).context(format!("open distill run lock {path_s}")));
        }
    };
    let path = path_s;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(anyhow::Error::new(DistillBusy(path))),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(anyhow::Error::new(e).context(format!("lock distill run lock {path}")))
        }
    }
}

fn run_native_within<P: LlmProvider + ?Sized>(
    db: &Db,
    provider: &P,
    batch: usize,
    wall_budget: std::time::Duration,
) -> Result<NativeReport> {
    let start = std::time::Instant::now();
    let ctx = EventCtx::system("distill");
    // Held until this function returns (dropping the File unlocks it).
    let _run_lock = acquire_run_lock(db)?;

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
            sourceless_candidates: 0,
            provider: provider.name().into(),
            duration_ms: start.elapsed().as_millis(),
        };
        record_run(db, &ctx, &report, true)?;
        return Ok(report);
    }

    let mut dedup = Dedup::load(db)?;
    let mut st = RunState {
        stop_at: Some(std::time::Instant::now() + wall_budget),
        ..RunState::default()
    };
    for chunk in items.chunks(CHUNK) {
        walk_chunk(db, provider, chunk, &mut dedup, &mut st, &ctx);
    }
    if st.sourceless > 0 {
        warn!(
            target: "ptask::distill",
            sourceless = st.sourceless,
            "provider returned candidates without sources — their captures cannot be \
             credited and are re-walked (burning calls); the model is ignoring the schema"
        );
    }
    if st.budget_exhausted {
        warn!(
            target: "ptask::distill",
            calls = st.calls,
            max_calls = MAX_PROVIDER_CALLS,
            elapsed_s = start.elapsed().as_secs(),
            "run budget exhausted — remaining rows deferred to the next run"
        );
    }

    for id in &st.consumed_ids {
        ptask_core::raw_items::mark_processed(db, *id)?;
    }
    // Charges blamed on input after a server error stand only if the run
    // never concluded the provider itself was failing.
    if !st.aborted {
        let provisional = std::mem::take(&mut st.provisional_failures);
        st.failures.extend(provisional);
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

    // The provider went away mid-run. What finished is kept (marked above)
    // and nothing was charged for the outage, but the run fails closed so
    // the outage is reported rather than looking like a short queue.
    if st.aborted {
        bail!(
            "{} ({} capture(s) completed before the outage; the rest are deferred, uncharged)",
            st.first_error
                .unwrap_or_else(|| "provider unavailable".into()),
            st.consumed_ids.len()
        );
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
        sourceless_candidates: st.sourceless,
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
        "sourceless_candidates": report.sourceless_candidates,
        "provider": report.provider,
        "duration_ms": report.duration_ms,
    });
    let uuid = format!("distill-native:{}", uuid::Uuid::new_v4());
    event_log::record(db, &uuid, None, event_type, &payload, ctx)?;
    Ok(())
}

/// Record a failure manifest (provider/preflight errors happen before a
/// report exists). Takes the error itself, not a string, and stores the
/// alternate (`{:#}`) form: the outermost context alone ("provider preflight
/// failed") hides the provider's actual answer underneath it.
pub fn record_failure(db: &Db, provider: &str, error: &anyhow::Error) {
    let ctx = EventCtx::system("distill");
    let payload = serde_json::json!({
        "native": true,
        "provider": provider,
        "error": format!("{error:#}"),
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
                .enumerate()
                .map(|(i, t)| Candidate {
                    title: t.clone(),
                    priority: 2,
                    description: String::new(),
                    sources: vec![i],
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

    struct EmptyConsolidationProvider;

    impl LlmProvider for EmptyConsolidationProvider {
        fn classify_batch(&self, texts: &[String]) -> Result<Vec<Classification>> {
            Ok(texts
                .iter()
                .enumerate()
                .map(|(idx, text)| Classification {
                    idx,
                    keep: text != "noise",
                    confidence: 1.0,
                    reason: String::new(),
                })
                .collect())
        }
        fn consolidate(&self, items: &[String]) -> Result<Vec<Candidate>> {
            if items.iter().any(|text| text == "EMPTY") {
                return Ok(vec![]);
            }
            Ok(items
                .iter()
                .enumerate()
                .map(|(i, text)| Candidate {
                    title: text.clone(),
                    priority: 2,
                    description: String::new(),
                    sources: vec![i],
                })
                .collect())
        }
        fn preflight(&self) -> Result<()> {
            Ok(())
        }
        fn name(&self) -> &'static str {
            "empty-consolidation-test"
        }
    }

    #[test]
    fn empty_consolidation_isolates_failed_capture_and_counts_noise_once() {
        for with_noise in [false, true] {
            let (_dir, db) = fresh_db();
            seed_inbox(&db, &["EMPTY"]);
            if with_noise {
                seed_inbox(&db, &["noise"]);
            }
            seed_inbox(&db, &["renew the office lease"]);
            let report = run_native(&db, &EmptyConsolidationProvider, 100).unwrap();
            assert_eq!(report.created, 1);
            assert_eq!(report.consumed, if with_noise { 2 } else { 1 });
            assert_eq!(report.failed, 1);
            assert_eq!(attempts(&db, "EMPTY"), 1);
            assert_eq!(ptask_core::raw_items::unprocessed_count(&db).unwrap(), 1);
        }
    }

    #[test]
    fn empty_consolidation_cannot_permanently_block_later_captures() {
        let (_dir, db) = fresh_db();
        seed_inbox(&db, &["EMPTY", "renew the office lease"]);
        for attempt in 1..=ptask_core::raw_items::MAX_DISTILL_ATTEMPTS {
            assert!(run_native(&db, &EmptyConsolidationProvider, 1).is_err());
            assert_eq!(attempts(&db, "EMPTY"), attempt);
        }
        let report = run_native(&db, &EmptyConsolidationProvider, 1).unwrap();
        assert_eq!(report.created, 1);
        assert_eq!(report.consumed, 1);
        assert_eq!(report.quarantined, 1);
        db.with_conn(|c| {
            let processed: i64 = c.query_row(
                "SELECT processed FROM raw_items WHERE text='EMPTY'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(processed, 0, "quarantined input stays recoverable");
            Ok(())
        })
        .unwrap();
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
                    sources: vec![],
                },
                Candidate {
                    title: "Book the Reykjavik flight".into(),
                    priority: 2,
                    description: "carry-on only".into(),
                    sources: vec![],
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
                sources: vec![],
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

    /// Regression (DIST-1): the consolidate prompt capped output at "1-4
    /// tasks" (and the code at 8) while the whole chunk of up to 25 kept
    /// captures was marked processed — every commitment past the cap was
    /// silently lost. Kept captures are now consumed only when a candidate
    /// covers them; the rest go round again.
    #[test]
    fn captures_past_the_consolidation_cap_are_not_lost() {
        /// Emits one candidate per item, but never more than four.
        struct CappedProvider;
        impl LlmProvider for CappedProvider {
            fn classify_batch(&self, texts: &[String]) -> Result<Vec<Classification>> {
                PoisonProvider { poison: "\u{0}" }.classify_batch(texts)
            }
            fn consolidate(&self, items: &[String]) -> Result<Vec<Candidate>> {
                Ok(PoisonProvider { poison: "\u{0}" }
                    .consolidate(items)?
                    .into_iter()
                    .take(4)
                    .collect())
            }
            fn preflight(&self) -> Result<()> {
                Ok(())
            }
            fn name(&self) -> &'static str {
                "capped-test"
            }
        }
        let (_dir, db) = fresh_db();
        let texts: Vec<String> = [
            "call the bank about the mandate",
            "book the Reykjavik flight",
            "renew the office lease",
            "email Alan the revised quote",
            "file the VAT return",
            "order replacement fans for the rack",
            "send the board pack to Sigrid",
            "cancel the unused colo cross-connect",
            "update the insurance policy address",
            "pay the electricity bill",
        ]
        .map(String::from)
        .to_vec();
        for t in &texts {
            ptask_core::raw_items::insert(&db, t, "test", "test://x").unwrap();
        }

        let report = run_native(&db, &CappedProvider, 100).unwrap();
        assert_eq!(report.created, 10, "every kept commitment became a task");
        assert_eq!(report.consumed, 10);
        assert_eq!(report.kept, 10);
        assert_eq!(report.failed, 0);
        assert_eq!(ptask_core::raw_items::unprocessed_count(&db).unwrap(), 0);
    }

    /// Consolidates each item into its own candidate, with sources mangled by
    /// `map` — the shapes real models produce.
    struct SourcesProvider {
        map: fn(usize) -> Vec<usize>,
        consolidations: std::cell::Cell<usize>,
        one_candidate: bool,
    }
    impl LlmProvider for SourcesProvider {
        fn classify_batch(&self, texts: &[String]) -> Result<Vec<Classification>> {
            PoisonProvider { poison: "\u{0}" }.classify_batch(texts)
        }
        fn consolidate(&self, items: &[String]) -> Result<Vec<Candidate>> {
            self.consolidations.set(self.consolidations.get() + 1);
            if self.one_candidate {
                return Ok(vec![Candidate {
                    title: "Reboot the fox-n1 node".into(),
                    priority: 2,
                    description: String::new(),
                    sources: (0..items.len()).collect(),
                }]);
            }
            Ok(PoisonProvider { poison: "\u{0}" }
                .consolidate(items)?
                .into_iter()
                .enumerate()
                .map(|(i, mut c)| {
                    c.sources = (self.map)(i);
                    c
                })
                .collect())
        }
        fn preflight(&self) -> Result<()> {
            Ok(())
        }
        fn name(&self) -> &'static str {
            "sources-test"
        }
    }

    fn distinct_captures(db: &Db, n: usize) -> Vec<String> {
        let texts: Vec<String> = (0..n)
            .map(|i| format!("distinct capture number {i} about topic {}", i * 7919))
            .collect();
        for t in &texts {
            ptask_core::raw_items::insert(db, t, "test", "test://x").unwrap();
        }
        texts
    }

    /// Regression (round 2, DIST-1a): a model answering with 1-based
    /// sources failed the range check before the lone-capture override ran,
    /// so even a single capture could never succeed: everything quarantined.
    #[test]
    fn one_based_sources_do_not_quarantine_lone_captures() {
        let (_dir, db) = fresh_db();
        let texts = distinct_captures(&db, 3);
        let provider = SourcesProvider {
            map: |i| vec![i + 1],
            consolidations: std::cell::Cell::new(0),
            one_candidate: false,
        };
        let report = run_native(&db, &provider, 100).unwrap();
        assert_eq!(report.consumed, 3);
        assert_eq!(report.failed, 0);
        assert!(texts.iter().all(|t| attempts(&db, t) == 0));
    }

    /// Regression (round 2, DIST-1b): a model that omits `sources` left its
    /// chunks uncovered and bisecting, silently burning the call budget. It
    /// is now counted, reported and recorded in the run manifest.
    #[test]
    fn candidates_without_sources_are_counted_and_reported() {
        let (_dir, db) = fresh_db();
        distinct_captures(&db, 4);
        let provider = SourcesProvider {
            map: |_| vec![],
            consolidations: std::cell::Cell::new(0),
            one_candidate: false,
        };
        let report = run_native(&db, &provider, 100).unwrap();
        assert!(report.sourceless_candidates > 0, "{report:?}");
        assert_eq!(report.consumed, 4, "lone captures still resolve");
        let recorded: i64 = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT json_extract(payload, '$.sourceless_candidates') FROM pt_event_log
                      WHERE event_type='distill.run'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(recorded as usize, report.sourceless_candidates);
    }

    /// Regression (round 2, DIST-1c): one candidate (or one deduped title)
    /// could claim every capture in a chunk, so a single over-merged answer
    /// consumed 25 commitments at once. A candidate now covers at most
    /// `MAX_SOURCES_PER_CANDIDATE`; the rest go round again (and still all
    /// resolve, so nothing is left permanently unconsumed).
    #[test]
    fn one_candidate_cannot_claim_a_whole_chunk() {
        let (_dir, db) = fresh_db();
        distinct_captures(&db, 20);
        let provider = SourcesProvider {
            map: |_| vec![],
            consolidations: std::cell::Cell::new(0),
            one_candidate: true,
        };
        let report = run_native(&db, &provider, 100).unwrap();
        assert_eq!(report.consumed, 20, "everything still resolves in the run");
        assert_eq!(report.created, 1);
        assert!(
            provider.consolidations.get() >= 20usize.div_ceil(8),
            "one answer covered {} captures in {} call(s)",
            20,
            provider.consolidations.get()
        );
    }

    /// A kept capture no candidate covers is retained, not consumed, even when
    /// its neighbours were covered — and it is charged only once isolated.
    #[test]
    fn an_uncovered_capture_is_retained_while_covered_ones_are_consumed() {
        struct SkipsOne;
        impl LlmProvider for SkipsOne {
            fn classify_batch(&self, texts: &[String]) -> Result<Vec<Classification>> {
                PoisonProvider { poison: "\u{0}" }.classify_batch(texts)
            }
            fn consolidate(&self, items: &[String]) -> Result<Vec<Candidate>> {
                Ok(PoisonProvider { poison: "\u{0}" }
                    .consolidate(items)?
                    .into_iter()
                    .filter(|c| !c.title.contains("IGNORED"))
                    .collect())
            }
            fn preflight(&self) -> Result<()> {
                Ok(())
            }
            fn name(&self) -> &'static str {
                "skips-one-test"
            }
        }
        let (_dir, db) = fresh_db();
        seed_inbox(
            &db,
            &[
                "call the bank about the mandate",
                "IGNORED commitment the model drops",
                "renew the office lease",
            ],
        );
        let report = run_native(&db, &SkipsOne, 100).unwrap();
        assert_eq!(report.created, 2);
        assert_eq!(report.consumed, 2);
        assert_eq!(report.failed, 1);
        assert_eq!(ptask_core::raw_items::unprocessed_count(&db).unwrap(), 1);
        assert_eq!(attempts(&db, "IGNORED commitment the model drops"), 1);
    }

    /// Regression (DIST-3): a blank/whitespace candidate title created a
    /// task (and consumed its capture); the next blank was then "deduped" by
    /// the temporal hash of the empty string, consuming that capture too.
    #[test]
    fn a_blank_candidate_title_creates_nothing_and_consumes_nothing() {
        struct BlankForBank;
        impl LlmProvider for BlankForBank {
            fn classify_batch(&self, texts: &[String]) -> Result<Vec<Classification>> {
                PoisonProvider { poison: "\u{0}" }.classify_batch(texts)
            }
            fn consolidate(&self, items: &[String]) -> Result<Vec<Candidate>> {
                Ok(PoisonProvider { poison: "\u{0}" }
                    .consolidate(items)?
                    .into_iter()
                    .map(|mut c| {
                        if c.title.contains("bank") {
                            c.title = " \t\u{a0} ".into();
                        }
                        c
                    })
                    .collect())
            }
            fn preflight(&self) -> Result<()> {
                Ok(())
            }
            fn name(&self) -> &'static str {
                "blank-test"
            }
        }
        let (_dir, db) = fresh_db();
        seed_inbox(
            &db,
            &[
                "call the bank about the mandate",
                "book the Reykjavik flight",
            ],
        );
        let report = run_native(&db, &BlankForBank, 100).unwrap();
        assert_eq!(report.created, 1);
        assert_eq!(report.consumed, 1, "the blank's capture is retained");
        assert_eq!(report.failed, 1);
        seed_inbox(&db, &["ring the bank again about the loan"]);
        let report = run_native(&db, &BlankForBank, 100).unwrap_err();
        assert!(report.to_string().contains("covered none"), "{report:#}");
        let blank_tasks: i64 = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM tasks WHERE trim(title, ' ' || char(9) || char(160)) = ''",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(blank_tasks, 0, "no task may have a blank title");
        assert_eq!(ptask_core::raw_items::unprocessed_count(&db).unwrap(), 2);
    }

    /// Regression (DIST-12): nothing claimed the rows, so two concurrent runs
    /// (the hourly timer and a manual `pt distill`) both fetched, classified
    /// and created tasks from the same captures.
    /// Regression (round 2, DIST-12): after one `sudo pt distill` the lock
    /// file belonged to root, and every later run as the timer user failed
    /// with "Permission denied" opening it for write. Locking a read-only
    /// descriptor works just as well (flock does not need write access).
    #[test]
    fn a_lock_file_we_cannot_write_is_still_usable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db.distill.lock");
        std::fs::write(&path, b"").unwrap();
        // Tests run as root here, so chmod cannot produce EACCES: inject it.
        let denied = |_: &std::path::Path| -> std::io::Result<std::fs::File> {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        };
        let db_file = dir.path().join("tasks.db");
        let held =
            lock_at(&path, &db_file, denied, open_ro).expect("fell back to a read-only lock");
        // It is a real lock: a second taker is refused as busy.
        let err = lock_at(&path, &db_file, denied, open_ro).unwrap_err();
        assert!(err.is::<DistillBusy>(), "{err:#}");
        drop(held);
        lock_at(&path, &db_file, denied, open_ro).unwrap();
    }

    /// Regression (round 3, DIST-12): a lock file created under umask 077
    /// (mode 0600, e.g. by root) could not even be opened read-only by the
    /// timer user, so every run failed with "Permission denied".
    #[test]
    fn a_lock_file_we_cannot_read_falls_back_and_new_ones_are_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let db_file = dir.path().join("tasks.db");
        std::fs::write(&db_file, b"").unwrap();
        let path = dir.path().join("tasks.db.distill.lock");

        // A lock file we can create gets 0644 whatever the umask.
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        drop(lock_at(&path, &db_file, open_lock_rw, open_ro).unwrap());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "lock file left at {mode:o}");

        // Neither writable nor readable (as root, chmod cannot deny us, so
        // inject EACCES): lock the database file itself, read-only.
        let denied = |_: &std::path::Path| -> std::io::Result<std::fs::File> {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        };
        let ro_denied_for_lock = |p: &std::path::Path| -> std::io::Result<std::fs::File> {
            if p.extension().is_some_and(|e| e == "lock") {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                std::fs::File::open(p)
            }
        };
        let held = lock_at(&path, &db_file, denied, ro_denied_for_lock)
            .expect("fell back to locking the database file");
        let err = lock_at(&path, &db_file, denied, ro_denied_for_lock).unwrap_err();
        assert!(err.is::<DistillBusy>(), "{err:#}");
        drop(held);

        // file: URIs are percent-decoded.
        assert_eq!(
            run_lock_path(std::path::Path::new("file:/srv/my%20db/tasks.db?mode=rwc")),
            Some(std::path::PathBuf::from("/srv/my db/tasks.db.distill.lock"))
        );
    }

    /// In-memory databases have no file to put a lock beside (and are
    /// private to the process anyway); `file:` URIs lock beside the real
    /// file, never as a junk `file:…` name in the working directory.
    #[test]
    fn the_run_lock_path_follows_the_database_kind() {
        use std::path::{Path, PathBuf};
        assert_eq!(run_lock_path(Path::new(":memory:")), None);
        assert_eq!(run_lock_path(Path::new("")), None);
        assert_eq!(run_lock_path(Path::new("file::memory:?cache=shared")), None);
        assert_eq!(
            run_lock_path(Path::new("file:x?mode=memory&cache=shared")),
            None
        );
        assert_eq!(
            run_lock_path(Path::new("file:/srv/pt/tasks.db?mode=rwc")),
            Some(PathBuf::from("/srv/pt/tasks.db.distill.lock"))
        );
        assert_eq!(
            run_lock_path(Path::new("file:///srv/pt/tasks.db")),
            Some(PathBuf::from("/srv/pt/tasks.db.distill.lock"))
        );
        assert_eq!(
            run_lock_path(Path::new("/srv/pt/tasks.db")),
            Some(PathBuf::from("/srv/pt/tasks.db.distill.lock"))
        );
    }

    #[test]
    fn a_concurrent_run_is_refused_without_touching_the_queue() {
        let (_dir, db) = fresh_db();
        seed_inbox(&db, &["call the bank about the mandate"]);
        let lock_path = format!("{}.distill.lock", db.path().display());
        let held = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .unwrap();
        held.lock().unwrap();

        let err = run_native(&db, &PoisonProvider { poison: "\u{0}" }, 100).unwrap_err();
        assert!(err.to_string().contains("already running"), "{err:#}");
        assert_eq!(ptask_core::raw_items::unprocessed_count(&db).unwrap(), 1);

        held.unlock().unwrap();
        let report = run_native(&db, &PoisonProvider { poison: "\u{0}" }, 100).unwrap();
        assert_eq!(report.consumed, 1, "the lock is released with its holder");
    }

    /// Healthy preflight, but every batch fails with `class` — a flapping or
    /// overloaded provider.
    struct Flapping {
        class: crate::providers::FailureClass,
        classify_calls: std::cell::Cell<usize>,
        preflights: std::cell::Cell<usize>,
    }
    impl Flapping {
        fn new(class: crate::providers::FailureClass) -> Self {
            Self {
                class,
                classify_calls: std::cell::Cell::new(0),
                preflights: std::cell::Cell::new(0),
            }
        }
    }
    impl LlmProvider for Flapping {
        fn classify_batch(&self, _texts: &[String]) -> Result<Vec<Classification>> {
            self.classify_calls.set(self.classify_calls.get() + 1);
            Err(anyhow::Error::new(
                crate::providers::ProviderUnavailable::new(
                    self.class,
                    "request failed after 3 attempt(s)",
                ),
            ))
        }
        fn consolidate(&self, _items: &[String]) -> Result<Vec<Candidate>> {
            unreachable!("classification never succeeds")
        }
        fn preflight(&self) -> Result<()> {
            self.preflights.set(self.preflights.get() + 1);
            Ok(())
        }
        fn name(&self) -> &'static str {
            "flapping-test"
        }
    }

    /// Regression (round 3, DIST-5): with a healthy preflight but every
    /// batch failing (503s from an overloaded provider), the re-preflight
    /// "proved" each failure input-specific: ~32 classify calls and 33
    /// preflights per run, and 15 healthy captures quarantined every 3 runs.
    /// Non-server classes now always abort, and a server-class failure may
    /// be blamed on input along one bisection path at most.
    #[test]
    fn a_flapping_provider_charges_nothing_and_stays_bounded() {
        use crate::providers::FailureClass;
        for class in [
            FailureClass::Overloaded,
            FailureClass::RateLimited,
            FailureClass::Timeout,
            FailureClass::Server,
        ] {
            let (_dir, db) = fresh_db();
            let texts = distinct_captures(&db, 60);
            for _ in 0..ptask_core::raw_items::MAX_DISTILL_ATTEMPTS {
                let provider = Flapping::new(class);
                assert!(run_native(&db, &provider, 300).is_err());
                let calls = provider.classify_calls.get() + provider.preflights.get();
                let bound = if class == FailureClass::Server { 12 } else { 2 };
                assert!(
                    calls <= bound,
                    "{class:?}: {calls} provider calls in one run"
                );
            }
            assert!(
                texts.iter().all(|t| attempts(&db, t) == 0),
                "{class:?}: healthy captures were charged for a flapping provider"
            );
            assert_eq!(ptask_core::raw_items::quarantined_count(&db).unwrap(), 0);
        }
    }

    /// Regression (round 2, DIST-5): a capture that deterministically gets a
    /// 5xx/408 (Gemini 500 on certain inputs, a local server 500 on context
    /// overflow) was treated as an outage, aborting every run before
    /// bisection. Rows are served oldest first, so the queue stalled forever
    /// with nothing charged. A healthy preflight now proves the failure is
    /// input-specific and the normal isolate/charge/quarantine path runs.
    #[test]
    fn a_poison_5xx_with_a_healthy_provider_is_isolated_not_an_outage() {
        struct Poison5xx {
            preflights: std::cell::Cell<usize>,
        }
        impl LlmProvider for Poison5xx {
            fn classify_batch(&self, texts: &[String]) -> Result<Vec<Classification>> {
                if texts.iter().any(|t| t.contains("POISON")) {
                    return Err(anyhow::Error::new(
                        crate::providers::ProviderUnavailable::new(
                            crate::providers::FailureClass::Server,
                            "local llm request failed after 3 attempt(s): http 500",
                        ),
                    ));
                }
                PoisonProvider { poison: "\u{0}" }.classify_batch(texts)
            }
            fn consolidate(&self, items: &[String]) -> Result<Vec<Candidate>> {
                PoisonProvider { poison: "\u{0}" }.consolidate(items)
            }
            fn preflight(&self) -> Result<()> {
                self.preflights.set(self.preflights.get() + 1);
                Ok(())
            }
            fn name(&self) -> &'static str {
                "poison-5xx-test"
            }
        }
        let (_dir, db) = fresh_db();
        seed_inbox(
            &db,
            &[
                "POISON memo that overflows the context",
                "call the bank about the mandate",
                "book the Reykjavik flight",
                "renew the office lease",
            ],
        );
        let provider = Poison5xx {
            preflights: std::cell::Cell::new(0),
        };
        let report = run_native(&db, &provider, 100).unwrap();
        assert_eq!(report.consumed, 3, "the queue moves past the poison row");
        assert_eq!(report.failed, 1);
        assert_eq!(attempts(&db, "POISON memo that overflows the context"), 1);
        assert!(
            provider.preflights.get() >= 2,
            "re-checked liveness before blaming input"
        );

        // Alone in the queue it is still charged each run, then quarantined.
        for _ in 1..ptask_core::raw_items::MAX_DISTILL_ATTEMPTS {
            assert!(run_native(&db, &provider, 100).is_err());
        }
        assert_eq!(ptask_core::raw_items::quarantined_count(&db).unwrap(), 1);
    }

    /// Regression (DIST-5): a 429/5xx/timeout was charged to the captures as
    /// a chargeable failure, and bisection multiplied the calls against the
    /// outage (7 classify calls and 4 charges for a 4-row batch).
    #[test]
    fn a_provider_outage_aborts_without_charging_or_bisecting() {
        struct OutageProvider {
            calls: std::cell::Cell<usize>,
            healthy_calls: usize,
        }
        impl LlmProvider for OutageProvider {
            fn classify_batch(&self, texts: &[String]) -> Result<Vec<Classification>> {
                self.calls.set(self.calls.get() + 1);
                if self.calls.get() > self.healthy_calls {
                    return Err(anyhow::Error::new(
                        crate::providers::ProviderUnavailable::new(
                            crate::providers::FailureClass::Server,
                            "http 502: bad gateway",
                        ),
                    ));
                }
                PoisonProvider { poison: "\u{0}" }.classify_batch(texts)
            }
            fn consolidate(&self, items: &[String]) -> Result<Vec<Candidate>> {
                PoisonProvider { poison: "\u{0}" }.consolidate(items)
            }
            /// A real outage fails the liveness check too (once it has begun).
            fn preflight(&self) -> Result<()> {
                if self.calls.get() > self.healthy_calls {
                    return Err(anyhow::Error::new(
                        crate::providers::ProviderUnavailable::new(
                            crate::providers::FailureClass::Server,
                            "http 502: bad gateway",
                        ),
                    ));
                }
                Ok(())
            }
            fn name(&self) -> &'static str {
                "outage-test"
            }
        }

        // Total outage after preflight: nothing charged, one call, fail closed.
        let (_dir, db) = fresh_db();
        let texts = [
            "call the bank about the mandate",
            "book the Reykjavik flight",
            "renew the office lease",
            "email Alan the revised quote",
        ];
        seed_inbox(&db, &texts);
        let provider = OutageProvider {
            calls: std::cell::Cell::new(0),
            healthy_calls: 0,
        };
        let err = run_native(&db, &provider, 100).unwrap_err();
        assert!(err.to_string().contains("provider unavailable"), "{err:#}");
        assert_eq!(provider.calls.get(), 1, "an outage must not be bisected");
        for t in texts {
            assert_eq!(attempts(&db, t), 0, "{t} was charged for an outage");
        }
        assert_eq!(ptask_core::raw_items::unprocessed_count(&db).unwrap(), 4);

        // Outage mid-run: the finished chunk is kept, the rest waits uncharged.
        let (_dir, db) = fresh_db();
        let many: Vec<String> = (0..CHUNK * 3)
            .map(|i| format!("distinct capture number {i} about topic {}", i * 7919))
            .collect();
        for t in &many {
            ptask_core::raw_items::insert(&db, t, "test", "test://x").unwrap();
        }
        let provider = OutageProvider {
            calls: std::cell::Cell::new(0),
            healthy_calls: 1,
        };
        assert!(run_native(&db, &provider, 200).is_err());
        assert_eq!(provider.calls.get(), 2, "the run stops at the first outage");
        assert_eq!(
            ptask_core::raw_items::unprocessed_count(&db).unwrap(),
            (CHUNK * 2) as i64,
            "the first chunk landed before the outage"
        );
        assert!(many.iter().all(|t| attempts(&db, t) == 0));
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
                sources: vec![],
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

    /// Past the wall-clock budget no new chunk starts, and the chunks that
    /// finished are still marked processed before the run returns.
    #[test]
    fn the_wall_clock_budget_defers_later_chunks_and_keeps_finished_ones() {
        struct SlowProvider(PoisonProvider);
        impl LlmProvider for SlowProvider {
            fn classify_batch(&self, texts: &[String]) -> Result<Vec<Classification>> {
                std::thread::sleep(std::time::Duration::from_millis(300));
                self.0.classify_batch(texts)
            }
            fn consolidate(&self, items: &[String]) -> Result<Vec<Candidate>> {
                self.0.consolidate(items)
            }
            fn preflight(&self) -> Result<()> {
                Ok(())
            }
            fn name(&self) -> &'static str {
                "slow-test"
            }
        }
        let (_dir, db) = fresh_db();
        let texts: Vec<String> = (0..CHUNK * 2)
            .map(|i| format!("distinct capture number {i} about topic {}", i * 7919))
            .collect();
        for t in &texts {
            ptask_core::raw_items::insert(&db, t, "test", "test://x").unwrap();
        }
        let provider = SlowProvider(PoisonProvider { poison: "\u{0}" });

        let report =
            run_native_within(&db, &provider, 200, std::time::Duration::from_millis(100)).unwrap();
        assert_eq!(report.consumed, CHUNK, "only the first chunk ran");
        assert_eq!(
            ptask_core::raw_items::unprocessed_count(&db).unwrap(),
            CHUNK as i64,
            "the deferred chunk waits for the next run, uncharged"
        );
        assert_eq!(attempts(&db, &texts[CHUNK]), 0);
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
                sources: vec![],
            }],
        };

        let report = run_native(&db, &provider, 200).unwrap();
        assert_eq!(report.consumed, CHUNK + 1);
        assert_eq!(report.created, 1, "the second chunk deduped, not recreated");
        assert!(report.skipped_dedup >= 1);
    }

    /// Regression (DIST-10): the failure manifest stored `e.to_string()`,
    /// which is only the outermost context, so `distill.failed` said
    /// "classify failed" and dropped the actual provider error underneath.
    #[test]
    fn record_failure_keeps_the_whole_error_chain() {
        let (_dir, db) = fresh_db();
        let e = anyhow::anyhow!("http 400: context length exceeded")
            .context("provider preflight failed — nothing consumed");
        record_failure(&db, "mock", &e);
        let stored: String = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT json_extract(payload, '$.error') FROM pt_event_log
                      WHERE event_type='distill.failed'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert!(stored.contains("preflight failed"), "{stored}");
        assert!(stored.contains("context length exceeded"), "{stored}");
    }

    /// Regression (round 3b, DIST-2): numbers dropped as "dates" were never
    /// compared, numeric dates were not parsed, and a number merely near a
    /// year counted as a date — so these lost real work by deduping. And
    /// month names stayed in the similarity tokens, so some true duplicates
    /// were missed.
    #[test]
    fn date_numbers_and_numeric_dates_are_compared_not_discarded() {
        for (a, b) in [
            ("Pay invoice 12 for March", "Pay invoice 13 for March"),
            ("Deadline 2026-10-06 filing", "Deadline 2026-11-06 filing"),
            ("Submit form by 06/10/2026", "Submit form by 07/10/2026"),
            ("Order 10 GPUs 2026", "Order 20 GPUs 2026"),
            ("Ship 40 units in March", "Ship 45 units in March"),
        ] {
            assert!(!title_similar(a, b), "{a:?} vs {b:?} must not dedup");
        }
        for (a, b) in [
            ("renew domain by 15 Oct", "renew domain"),
            ("Book the venue for Oct 5", "Book the venue for October 5th"),
            ("Deadline 2026-10-06 filing", "Deadline filing"),
            ("Submit form by 6/10", "Submit form"),
        ] {
            assert!(title_similar(a, b), "{a:?} vs {b:?} should dedup");
        }
        // A number near a month that cannot be a day stays an identifier.
        assert!(identifiers_conflict(
            "Ship 40 units in March",
            "Ship units in March"
        ));
    }

    /// Regression (round 3, DIST-2): dropping date/time tokens on both sides
    /// made titles that differ only by year dedup — and the universe holds
    /// done tasks, so "File VAT return 2026" was swallowed by last year's
    /// completed "File VAT return 2025" and never created. A date/time value
    /// on one side only is ignored; differing values of the same kind on
    /// both sides block the match.
    #[test]
    fn differing_dates_and_times_on_both_sides_block_a_match() {
        for (a, b) in [
            ("File VAT return 2025", "File VAT return 2026"),
            ("Call Bob at 3pm", "Call Bob at 4pm"),
            ("Call Bob at 15:00", "Call Bob at 9:30am"),
            ("Book the venue for 5 Oct", "Book the venue for 6 Oct"),
            ("Book the venue in March", "Book the venue in April"),
        ] {
            assert!(!title_similar(a, b), "{a:?} vs {b:?} must not dedup");
        }
        for (a, b) in [
            ("Email Alan the quote", "Email Alan the quote by 5pm"),
            ("File VAT return 2026", "File VAT return for 2026"),
            ("Call Bob at 3pm", "call Bob at 15:00"),
            // Same day written two ways is the same value.
            ("Book the venue for Oct 5", "Book the venue for October 5th"),
        ] {
            assert!(title_similar(a, b), "{a:?} vs {b:?} should dedup");
        }

        // End to end: last year's done task does not swallow this year's.
        let (_dir, db) = fresh_db();
        let done = ptask_core::tasks::create(
            &db,
            NewTask::minimal("File VAT return 2025"),
            &EventCtx::test(),
        )
        .unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET status='done', status_v2='done' WHERE id=?1",
                [&done.id],
            )?;
            Ok(())
        })
        .unwrap();
        seed_inbox(&db, &["file the 2026 VAT return before the deadline"]);
        let provider = MockProvider {
            broken: false,
            emit: vec![Candidate {
                title: "File VAT return 2026".into(),
                priority: 3,
                description: String::new(),
                sources: vec![],
            }],
        };
        let report = run_native(&db, &provider, 100).unwrap();
        assert_eq!(report.created, 1, "this year's return is new work");
        assert_eq!(report.skipped_dedup, 0);
    }

    /// Regression (round 2, DIST-2): every digit-bearing token counted as an
    /// identifier, so a genuine duplicate that only adds a date or time
    /// ("… by 5pm", "… for 2026") was no longer deduped.
    #[test]
    fn date_and_time_tokens_are_not_identifiers() {
        for (a, b) in [
            ("Email Alan the quote", "Email Alan the quote by 5pm"),
            ("File VAT return", "File VAT return for 2026"),
            ("Call the bank at 9:30am", "call the bank"),
            ("Renew the lease on the 5th", "Renew the lease"),
            ("Book the venue for 12 March", "Book the venue"),
            ("Book the venue for March 12", "Book the venue for March"),
            ("Ship the release 2026-10-06", "Ship the release"),
            ("Pay rent at 17:00", "pay rent"),
        ] {
            assert!(!identifiers_conflict(a, b), "{a:?} vs {b:?}");
            assert!(title_similar(a, b), "{a:?} vs {b:?}");
        }
        // Real identifiers still block a match.
        assert!(!title_similar(
            "Pay invoice 4411 to Acme",
            "Pay invoice 4412 to Acme"
        ));
        assert!(!title_similar(
            "Reboot the fox-n1 node",
            "Reboot the fox-n3 node"
        ));
        assert!(!title_similar(
            "Renew cert for host7 today",
            "Renew cert for host9 today"
        ));
        // A bare small number with no month nearby is still an identifier.
        assert!(identifiers_conflict(
            "Replace disk 12 in the rack",
            "Replace disk 14 in the rack"
        ));
    }

    /// Regression (DIST-2): the 0.6 Jaccard gate ignored identifiers, so a
    /// different invoice number or host deduped against the old task —
    /// including a *done* one, so the new commitment was never created.
    #[test]
    fn distinct_identifiers_never_dedup() {
        assert!(!title_similar(
            "Pay invoice 4411 to Acme",
            "Pay invoice 4412 to Acme"
        ));
        assert!(!title_similar(
            "Reboot the fox-n1 node",
            "Reboot the fox-n3 node"
        ));
        assert!(!title_similar(
            "Renew cert for host7 today",
            "Renew cert for host9 today"
        ));
        assert!(!title_similar(
            "Pay invoice 4411 to Acme",
            "Pay invoice to Acme"
        ));
        // Same identifiers still dedup.
        assert!(title_similar(
            "Pay invoice 4411 to Acme",
            "pay Acme invoice 4411"
        ));
        assert!(title_similar(
            "Reboot the fox-n1 node",
            "reboot fox-n1 node now"
        ));

        let (_dir, db) = fresh_db();
        let done = ptask_core::tasks::create(
            &db,
            NewTask::minimal("Pay invoice 4411 to Acme"),
            &EventCtx::test(),
        )
        .unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET status='done', status_v2='done' WHERE id=?1",
                [&done.id],
            )?;
            Ok(())
        })
        .unwrap();
        seed_inbox(&db, &["pay Acme invoice 4412 by Friday"]);
        let provider = MockProvider {
            broken: false,
            emit: vec![Candidate {
                title: "Pay invoice 4412 to Acme".into(),
                priority: 3,
                description: String::new(),
                sources: vec![],
            }],
        };
        let report = run_native(&db, &provider, 100).unwrap();
        assert_eq!(report.created, 1, "invoice 4412 is new work, not 4411");
        assert_eq!(report.skipped_dedup, 0);
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
