//! Accountability state machine + notification dispatch.
//!
//! Port of `~/puretensor-tasks/accountability/engine.py`. The state machine,
//! notification budget, quiet-hour rules, and message templates mirror the
//! Python implementation exactly so the cutover is a swap of the systemd
//! unit, not a behaviour change.
//!
//! Levels:
//!
//!   0  new        — no reminder yet
//!   1  reminded   — telegram only
//!   2  deferred   — telegram only
//!   3  escalated  — telegram + email
//!   4  critical   — telegram + email
//!   5  final      — email only; the ladder stops here. Status is NOT
//!                   changed: auto-flipping to 'blocked' hid real work from
//!                   the pending views (33 tasks, 2026-07..09).
//!
//! Transitions (time at the current level, from `level_changed_at`):
//!
//!   0 → 1  task age ≥ 2 days (age of the current occurrence: completing a
//!          recurring task or reopening one restarts the ladder at 0)
//!   1 → 2  ≥ 3 days at level 1
//!   2 → 3  ≥ 4 days at level 2
//!   3 → 4  ≥ 2 days at level 3
//!   4 → 5  ≥ 7 days at level 4
//!
//! A level whose channels are all unconfigured uses whichever channel is
//! configured instead (level 5 goes to Telegram on a Telegram-only install;
//! levels 1-2 go to email on an email-only one), so the ladder never stalls
//! on a channel that does not exist.
//!
//! Each reminder is claimed before it is sent: one conditional UPDATE
//! re-checks the task against its current row (still eligible, same level,
//! same `last_reminded`) and stamps the cooldown, and the Telegram budget
//! slot is reserved. A failed delivery releases both. A crash or a failed
//! write after delivery therefore cannot leave a sent nudge unrecorded and
//! due again, and a task completed or snoozed mid-run is not nudged.
//!
//! A new level is persisted only once its notice is delivered on some
//! channel, so a dead or unconfigured channel cannot walk a task up the
//! ladder unseen, and a failed level-5 email is retried rather than lost.
//!
//! Budgets:
//!
//!   - Daily budget: at most 3 Telegram sends per UTC day (counted in
//!     `daily_budget`). Email is unbudgeted.
//!   - Per-task cooldown: ≥ 4 hours between reminders.
//!   - Quiet hours: 22:00 — 08:00 Europe/London (no sends).
//!
//! Message generation:
//!
//!   - If `PTASK_HAL_NUDGE_URL` is set, POST `{task, level, age_days,
//!     dismissal_count}` and use the returned `message` field.
//!   - Otherwise use a static template per-level.

use crate::Db;
use crate::dates::parse_iso_to_utc;
use crate::error::{Error, Result};
use jiff::Zoned;
use rusqlite::OptionalExtension;
use rusqlite::params;
use tracing::{error, info};

pub const DAILY_BUDGET_MAX: i64 = 3;

/// Consecutive Telegram send failures after which the channel is treated as
/// dead for the remainder of the run. Prevents hammering a 401ing bot token
/// once per eligible task (45 WARN lines per cycle during the 2026-06/07
/// dead-token incident) while still proving the failure three times.
pub const TELEGRAM_CIRCUIT_BREAK: i64 = 3;
pub const MIN_HOURS_BETWEEN_TASK_REMINDERS: i64 = 4;
pub const QUIET_START_UTC_HOUR: i8 = 22;
pub const QUIET_END_UTC_HOUR: i8 = 8;

/// Channels touched by a single task's notification cycle.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DispatchedFor {
    pub task_uuid: String,
    pub level: i64,
    pub telegram_sent: bool,
    pub email_sent: bool,
    pub message: String,
    pub error: Option<String>,
}

/// Aggregate of one `run_check` invocation.
#[derive(Debug, Default, Clone)]
pub struct RunReport {
    pub quiet_hours: bool,
    pub budget_used_before: i64,
    pub budget_used_after: i64,
    pub eligible: i64,
    pub dispatched: Vec<DispatchedFor>,
    /// Send attempts that failed while the channel WAS configured. Eligible
    /// tasks with zero dispatches and non-zero failures means every channel
    /// is dead — callers must fail loud, not report ok.
    pub send_failures: i64,
}

#[derive(Debug, Clone)]
struct EligibleTask {
    id: String,
    title: String,
    /// When the current occurrence began: the latest recurrence advance or
    /// reopen, else `created_at`. Age is measured from here, so a recurring
    /// task completed on schedule is never "33 days old".
    occurrence_start: String,
    last_reminded: Option<String>,
    next_reminder: Option<String>,
    dismissal_count: i64,
    escalation_level: i64,
    level_changed_at: Option<String>,
}

/// True in the quiet window 22:00 — 08:00 **Europe/London** (the operator's
/// wall clock — the old UTC window drifted an hour every BST switch).
pub fn in_quiet_hours_at(z: &Zoned) -> bool {
    let tz = jiff::tz::TimeZone::get("Europe/London").unwrap_or(jiff::tz::TimeZone::UTC);
    let h = z.with_time_zone(tz).hour();
    !(QUIET_END_UTC_HOUR..QUIET_START_UTC_HOUR).contains(&h)
}

/// Read the daily-Telegram-budget counter for `date_utc` (YYYY-MM-DD).
pub fn get_daily_budget(db: &Db, date_utc: &str) -> Result<i64> {
    let conn = db.get()?;
    let row: Option<i64> = conn
        .query_row(
            "SELECT notifications_sent FROM daily_budget WHERE date = ?1",
            [date_utc],
            |r| r.get(0),
        )
        .optional()?;
    Ok(row.unwrap_or(0))
}

/// Atomically take one Telegram budget slot for `date_utc` if fewer than
/// `max` are used. `Ok(false)` = the budget is spent (possibly by a
/// concurrent run). A single conditional upsert, so two runs can never both
/// take the last slot.
pub fn reserve_daily_budget(db: &Db, date_utc: &str, max: i64) -> Result<bool> {
    if max <= 0 {
        return Ok(false);
    }
    let conn = db.get()?;
    let changed = conn.execute(
        "INSERT INTO daily_budget (date, notifications_sent) VALUES (?1, 1)
         ON CONFLICT(date) DO UPDATE SET notifications_sent = notifications_sent + 1
          WHERE notifications_sent < ?2",
        params![date_utc, max],
    )?;
    Ok(changed == 1)
}

/// Increment the Telegram budget counter for `date_utc` by one. Returns the
/// new value.
pub fn increment_daily_budget(db: &Db, date_utc: &str) -> Result<i64> {
    let conn = db.get()?;
    conn.execute(
        "INSERT INTO daily_budget (date, notifications_sent) VALUES (?1, 1)
         ON CONFLICT(date) DO UPDATE SET notifications_sent = notifications_sent + 1",
        [date_utc],
    )?;
    let n: i64 = conn.query_row(
        "SELECT notifications_sent FROM daily_budget WHERE date = ?1",
        [date_utc],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// The reminder ladder's eligibility test, with `?1` = now (operator ISO).
/// Shared by [`fetch_eligible`] and [`claim_reminder`], which re-applies it
/// atomically immediately before a send.
const ELIGIBLE_PREDICATE: &str = "status IN ('pending', 'delayed')
           AND NOT (COALESCE(status_v2,'') = 'snoozed'
                    AND snoozed_until IS NOT NULL
                    AND julianday(snoozed_until) IS NOT NULL
                    AND ((length(snoozed_until) = 10
                          AND snoozed_until > substr(?1, 1, 10))
                         OR (length(snoozed_until) > 10
                             AND julianday(snoozed_until) > julianday(?1))))
           AND COALESCE(task_type,'operational') != 'idea'
           AND (next_reminder IS NULL
                OR julianday(next_reminder) IS NULL
                OR julianday(next_reminder) <= julianday(?1))
           AND COALESCE(escalation_level, 0) < 5";

/// Tasks the reminder ladder may act on right now.
///
/// Both timestamp guards below fail *visible*: `julianday()` returns NULL on
/// an unparseable value, so a malformed `snoozed_until` must not be allowed to
/// suppress the row and a malformed `next_reminder` counts as due. Otherwise a
/// single bad timestamp silences a task permanently.
fn fetch_eligible(db: &Db, now_iso: &str) -> Result<Vec<EligibleTask>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(&format!(
        "SELECT id, title,
                COALESCE((SELECT i.ts FROM interactions i
                           WHERE i.task_id = tasks.id
                             AND (i.action = 'recurrence_advance'
                                  OR (i.action = 'status_change'
                                      AND i.details LIKE 'Reopened%'))
                             AND julianday(i.ts) IS NOT NULL
                           ORDER BY julianday(i.ts) DESC LIMIT 1),
                         created_at),
                last_reminded,
                COALESCE(dismissal_count, 0), COALESCE(escalation_level, 0),
                level_changed_at, next_reminder
         FROM tasks
         WHERE {ELIGIBLE_PREDICATE}
         ORDER BY (last_reminded IS NOT NULL), last_reminded ASC,
                  priority DESC, priority_score DESC"
    ))?;
    let rows = stmt.query_map([now_iso], |r| {
        Ok(EligibleTask {
            id: r.get(0)?,
            title: r.get(1)?,
            occurrence_start: r.get(2)?,
            last_reminded: r.get(3)?,
            dismissal_count: r.get(4)?,
            escalation_level: r.get(5)?,
            level_changed_at: r.get(6)?,
            next_reminder: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn task_age_days(task: &EligibleTask, now: &Zoned) -> i64 {
    let Some(created) = parse_iso_to_utc(&task.occurrence_start) else {
        return 0;
    };
    let delta = now.timestamp().as_second() - created.timestamp().as_second();
    delta / 86_400
}

fn should_advance(task: &EligibleTask, age_days: i64, now: &Zoned) -> bool {
    // Time-at-level replaces the v1 dismissal_count gates, which had NO
    // writer anywhere in the codebase — levels 2-3 were unreachable for a
    // year while the doc claimed a 6-level ladder. Levels 3-4 used to gate
    // on `last_reminded` age instead, but those levels email every 4h and
    // each send restamps it, so 3 → 4 could never fire while email worked.
    let level_age_days = task
        .level_changed_at
        .as_deref()
        .and_then(parse_iso_to_utc)
        .map(|z| (now.timestamp().as_second() - z.timestamp().as_second()) / 86_400)
        .unwrap_or(age_days);
    match task.escalation_level {
        0 => age_days >= 2,
        1 => level_age_days >= 3,
        2 => level_age_days >= 4,
        3 => level_age_days >= 2,
        4 => level_age_days >= 7,
        _ => false,
    }
}

fn can_remind(task: &EligibleTask, now: &Zoned) -> bool {
    let Some(last) = task.last_reminded.as_deref().and_then(parse_iso_to_utc) else {
        return true;
    };
    let delta = now.timestamp().as_second() - last.timestamp().as_second();
    delta >= MIN_HOURS_BETWEEN_TASK_REMINDERS * 3600
}

/// Channels a level is delivered on. A level whose channels are all
/// unconfigured falls back to the configured one: level 5 is email-only,
/// so with email unset its notice could never be delivered, the level never
/// persisted, and the task went silent; likewise Telegram-only levels on an
/// email-only install. With nothing configured the ladder's own list is
/// returned and nothing is sent.
fn channels_for(level: i64, cfg: &DispatchCfg) -> Vec<&'static str> {
    let ladder: &[&'static str] = match level {
        1 | 2 => &["telegram"],
        3 | 4 => &["telegram", "email"],
        5 => &["email"],
        _ => &[],
    };
    let configured = |c: &&str| match *c {
        "telegram" => cfg.telegram_configured(),
        "email" => cfg.email_configured(),
        _ => false,
    };
    if ladder.is_empty() || ladder.iter().any(configured) {
        return ladder.to_vec();
    }
    let fallback: Vec<&'static str> = ["telegram", "email"]
        .into_iter()
        .filter(configured)
        .collect();
    if fallback.is_empty() {
        ladder.to_vec()
    } else {
        fallback
    }
}

/// Telegram's message-text limit, counted in UTF-16 code units of the text
/// after entity parsing (an emoji outside the BMP counts as two).
pub const TELEGRAM_TEXT_LIMIT: usize = 4096;

/// Cut `s` to at most `max_units` UTF-16 code units (Telegram's unit),
/// ending in `…` when anything was removed. Never splits a character; cut
/// plain text *before* HTML-escaping it so no entity is split either.
pub fn truncate_utf16(s: &str, max_units: usize) -> String {
    if s.encode_utf16().count() <= max_units {
        return s.to_string();
    }
    let budget = max_units.saturating_sub(1);
    let mut used = 0;
    let mut out = String::new();
    for ch in s.chars() {
        used += ch.len_utf16();
        if used > budget {
            break;
        }
        out.push(ch);
    }
    if max_units > 0 {
        out.push('…');
    }
    out
}

/// Escape text for a Telegram `parse_mode: HTML` message body.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Loss-frame static message templates. Match `_LEVEL_PROMPTS` in
/// `accountability/engine.py` semantically — short, factual, day-count first.
fn fallback_message(task: &EligibleTask, level: i64, age_days: i64) -> String {
    match level {
        1 => format!("Still open: {}. Day {}.", task.title, age_days),
        2 => format!(
            "Still open after {} days: {}. Each defer cements the avoidance.",
            age_days, task.title
        ),
        3 => format!(
            "{}: open {} days. State the concrete consequence to yourself.",
            task.title, age_days
        ),
        4 => format!(
            "{}: {} days open. Action today or this becomes a blocker.",
            task.title, age_days
        ),
        5 => format!(
            "{}: {} days dormant. Final notice — fix it or kill it.",
            task.title, age_days
        ),
        _ => format!("Task pending {} days: {}", age_days, task.title),
    }
}

/// Update tasks(escalation_level=N), log to interactions, and record an
/// attributed `task.escalated` event — escalations used to be invisible to
/// the journal (and thus to delta sync and the audit trail).
///
/// `updated_at` is deliberately untouched: it is the operator's "last
/// touched" signal for neglect scoring and the reaper, and the engine
/// escalating an ignored task is not a touch.
fn set_escalation_level(db: &Db, task_uuid: &str, level: i64) -> Result<()> {
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let now = crate::dates::format_iso(&crate::dates::now_in_operator_tz()?);
    tx.execute(
        "UPDATE tasks SET escalation_level=?1, level_changed_at=?2 WHERE id=?3",
        params![level, now, task_uuid],
    )?;
    tx.execute(
        "INSERT INTO interactions (task_id, action, ts, details)
         VALUES (?1, 'escalation', ?2, ?3)",
        params![task_uuid, now, format!("escalation_level → {}", level)],
    )?;
    crate::event_log::record_in_conn(
        &tx,
        &format!("local:{}", uuid::Uuid::new_v4()),
        Some(task_uuid),
        "task.escalated",
        &serde_json::json!({ "task_uuid": task_uuid, "level": level }),
        &crate::event_log::EventCtx::system("accountability"),
    )?;
    tx.commit()?;
    Ok(())
}

fn log_notification(
    db: &Db,
    task_uuid: &str,
    channel: &str,
    level: i64,
    message: &str,
) -> Result<()> {
    let conn = db.get()?;
    let now = crate::dates::format_iso(&crate::dates::now_in_operator_tz()?);
    conn.execute(
        "INSERT INTO notifications (task_id, channel, sent_at, escalation_level, message_text)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![task_uuid, channel, now, level, message],
    )?;
    Ok(())
}

/// Claim this task's reminder slot BEFORE sending: stamp `last_reminded =
/// now` and `next_reminder = now + 4h` in one conditional UPDATE that also
/// re-checks, against the row as it is now, that the task is still eligible
/// (not completed, dismissed or snoozed meanwhile — e.g. by a Done tap) and
/// unchanged since [`fetch_eligible`] (same level, same `last_reminded`, so
/// a concurrent run cannot claim it twice). `Ok(false)` = skip the task.
///
/// Recording before the send is what makes the nudge idempotent: a crash,
/// kill or write failure after delivery can no longer leave it unstamped
/// and due again. If every channel then fails, [`release_claim`] undoes it.
fn claim_reminder(
    db: &Db,
    task: &EligibleTask,
    now_iso_operator: &str,
    now: &Zoned,
) -> Result<bool> {
    let now_iso = crate::dates::format_iso(now);
    let next = now
        .checked_add(jiff::Span::new().hours(MIN_HOURS_BETWEEN_TASK_REMINDERS))
        .map_err(|e| Error::Other(format!("next_reminder math: {}", e)))?;
    let next_iso = crate::dates::format_iso(&next);
    let conn = db.get()?;
    let claimed = conn.execute(
        &format!(
            "UPDATE tasks SET last_reminded=?2, next_reminder=?3
              WHERE id=?4 AND {ELIGIBLE_PREDICATE}
                AND COALESCE(escalation_level, 0) = ?5
                AND last_reminded IS ?6"
        ),
        params![
            now_iso_operator,
            now_iso,
            next_iso,
            task.id,
            task.escalation_level,
            task.last_reminded
        ],
    )?;
    Ok(claimed == 1)
}

/// Undo [`claim_reminder`] when nothing was delivered, so a dead channel
/// does not silently consume the task's cooldown. Best effort: if this
/// write fails the task simply waits out one cooldown (fails quiet, never
/// loud-twice).
fn release_claim(db: &Db, task: &EligibleTask, now: &Zoned) {
    let claimed_iso = crate::dates::format_iso(now);
    let result = db.get().and_then(|conn| {
        conn.execute(
            "UPDATE tasks SET last_reminded=?1, next_reminder=?2
              WHERE id=?3 AND last_reminded=?4",
            params![task.last_reminded, task.next_reminder, task.id, claimed_iso],
        )
        .map_err(Error::from)
    });
    if let Err(e) = result {
        error!(target: "ptask::accountability", task_uuid = %task.id, error = %e, "could not release an undelivered reminder claim");
    }
}

/// Give back a Telegram budget slot reserved for a send that failed.
fn refund_daily_budget(db: &Db, date_utc: &str) {
    let result = db.get().and_then(|conn| {
        conn.execute(
            "UPDATE daily_budget SET notifications_sent = MAX(notifications_sent - 1, 0)
              WHERE date = ?1",
            [date_utc],
        )
        .map_err(Error::from)
    });
    if let Err(e) = result {
        error!(target: "ptask::accountability", error = %e, "could not refund a telegram budget slot");
    }
}

/// A database write that follows a delivered send. Failing it must not abort
/// the run (the send happened and the claim is already recorded), so it is
/// logged and attached to the task's report instead.
fn after_send(what: &str, result: Result<()>, dispatched: &mut DispatchedFor) {
    if let Err(e) = result {
        error!(target: "ptask::accountability", task_uuid = %dispatched.task_uuid, error = %e, "{what} failed after delivery");
        dispatched.error = Some(format!("{what}: {e}"));
    }
}

/// Dispatch configuration lives in the central config module; re-exported
/// here so existing `accountability::DispatchCfg` callers keep working.
pub use crate::config::DispatchCfg;

/// Everything HAL needs to compose a nudge message for one task.
#[derive(Debug, Clone)]
pub struct NudgeRequest {
    pub task_uuid: String,
    pub title: String,
    pub level: i64,
    pub age_days: i64,
    pub dismissal_count: i64,
}

/// Side-effecting notification channels. Core decides *what* to send and
/// records outcomes; the network implementations (reqwest/lettre) live in
/// `ptask-notify` so this crate carries no HTTP/TLS/executor dependencies
/// and tests can inject deterministic fakes instead of scrubbing process
/// env or dialing unroutable ports.
///
/// Contract for the send methods: `Ok(true)` = delivered, `Ok(false)` =
/// attempted but failed (network / non-2xx / timeout), `Err` =
/// misconfiguration. [`run_check_at`] treats an `Err` as that channel's
/// failure for that task (recorded in `DispatchedFor::error`) and carries on.
/// Config-missing and dry-run short-circuits are handled by the CALLER
/// ([`run_check_at`]) — implementations may assume real config and a live
/// send is wanted.
pub trait Dispatch: Send + Sync {
    /// `buttons` are inline-keyboard actions rendered as one row under the
    /// message; empty slice = no keyboard. Each pair is (label,
    /// callback_data). Callback taps are received by nexus — the bot's
    /// single `getUpdates` owner — and forwarded to `POST /tg/callback`.
    fn send_telegram(
        &self,
        cfg: &DispatchCfg,
        text: &str,
        buttons: &[(String, String)],
    ) -> impl Future<Output = Result<bool>> + Send;

    fn send_email(
        &self,
        cfg: &DispatchCfg,
        subject: &str,
        body: &str,
    ) -> impl Future<Output = Result<bool>> + Send;

    /// Ask HAL to compose the message body. `None` = unavailable/failed;
    /// the caller falls back to the static template.
    fn compose_via_hal(
        &self,
        cfg: &DispatchCfg,
        req: &NudgeRequest,
    ) -> impl Future<Output = Option<String>> + Send;
}

/// Inline actions attached to every Telegram nudge: the triage loop's
/// missing actuator. callback_data is `<verb>:<task_uuid>` (fits Telegram's
/// 64-byte limit: 9 + 36); the `/tg/callback` server route executes taps.
pub fn nudge_buttons(task_uuid: &str) -> Vec<(String, String)> {
    vec![
        ("\u{2705} Done".into(), format!("ptdone:{task_uuid}")),
        (
            "\u{1f4a4} Snooze 3d".into(),
            format!("ptsnooze:{task_uuid}"),
        ),
        ("\u{1f5d1} Dismiss".into(), format!("ptdismiss:{task_uuid}")),
    ]
}

/// Fold one channel's send result into delivered / not delivered. A channel
/// `Err` (misconfiguration, e.g. an unparseable address) is that channel's
/// failure only: propagating it used to abort the run after another channel
/// had already delivered but before the reminder was stamped, so every later
/// run repeated the nudge.
fn delivered(result: Result<bool>, channel: &str, dispatched: &mut DispatchedFor) -> bool {
    match result {
        Ok(ok) => ok,
        Err(e) => {
            error!(
                target: "ptask::accountability",
                channel,
                task_uuid = %dispatched.task_uuid,
                error = %e,
                "channel misconfigured — counted as a failed send"
            );
            dispatched.error = Some(format!("{channel}: {e}"));
            false
        }
    }
}

/// Run one accountability cycle. Mirrors `engine.run_check()`.
pub async fn run_check<D: Dispatch>(db: &Db, cfg: &DispatchCfg, dispatch: &D) -> Result<RunReport> {
    let now = crate::dates::now_in_operator_tz()?;
    run_check_at(db, cfg, dispatch, &now).await
}

/// Same as [`run_check`] but with an injected `now`. Tests use this to
/// pin the wall-clock anchor outside of the quiet-hours window.
pub async fn run_check_at<D: Dispatch>(
    db: &Db,
    cfg: &DispatchCfg,
    dispatch: &D,
    now: &Zoned,
) -> Result<RunReport> {
    let now_utc = now.with_time_zone(jiff::tz::TimeZone::UTC);
    let operator_tz = jiff::tz::TimeZone::get(crate::dates::OPERATOR_TZ)
        .map_err(|e| Error::Other(format!("operator tz: {e}")))?;
    let now_operator = now.with_time_zone(operator_tz);
    let now_iso_operator = crate::dates::format_iso(&now_operator);
    let date_utc = now_utc.date().to_string();
    let mut report = RunReport {
        quiet_hours: in_quiet_hours_at(now),
        ..Default::default()
    };
    if report.quiet_hours {
        info!(target: "ptask::accountability", "quiet hours — skipping");
        return Ok(report);
    }
    let budget_used = get_daily_budget(db, &date_utc)?;
    report.budget_used_before = budget_used;
    report.budget_used_after = budget_used;
    let mut telegram_remaining = (DAILY_BUDGET_MAX - budget_used).max(0);
    if telegram_remaining == 0 {
        info!(
            target: "ptask::accountability",
            used = budget_used, max = DAILY_BUDGET_MAX, "telegram budget exhausted"
        );
    }

    let eligible = fetch_eligible(db, &now_iso_operator)?;
    report.eligible = eligible.len() as i64;
    let mut sent_telegrams = 0i64;
    let mut telegram_consecutive_failures = 0i64;
    let mut email_broken = false;

    for task in eligible {
        let age_days = task_age_days(&task, &now_utc);

        let level_after_transition = if should_advance(&task, age_days, &now_utc) {
            (task.escalation_level + 1).min(5)
        } else {
            task.escalation_level
        };
        if level_after_transition == 0 {
            continue;
        }
        if !can_remind(&task, &now_utc) {
            continue;
        }

        let channels = channels_for(level_after_transition, cfg);
        let telegram_only = channels == ["telegram"];
        if telegram_only && sent_telegrams >= telegram_remaining {
            continue;
        }

        // The new level is only persisted below, once its notice has been
        // delivered on some channel.
        let escalating = level_after_transition != task.escalation_level;
        let level = level_after_transition;

        let composed = if cfg.hal_nudge_url.is_none() || cfg.dry_run {
            None
        } else {
            let req = NudgeRequest {
                task_uuid: task.id.clone(),
                title: task.title.clone(),
                level,
                age_days,
                dismissal_count: task.dismissal_count,
            };
            dispatch.compose_via_hal(cfg, &req).await
        };
        let message = composed.unwrap_or_else(|| fallback_message(&task, level, age_days));
        // Re-check and record atomically, right before anything is sent.
        if !cfg.dry_run && !claim_reminder(db, &task, &now_iso_operator, &now_utc)? {
            info!(
                target: "ptask::accountability",
                task_uuid = %task.id,
                "task changed since it was selected — not reminded"
            );
            continue;
        }
        let mut dispatched = DispatchedFor {
            task_uuid: task.id.clone(),
            level,
            message: message.clone(),
            ..Default::default()
        };

        for channel in &channels {
            let ok = match *channel {
                "telegram" => {
                    if sent_telegrams >= telegram_remaining
                        || telegram_consecutive_failures >= TELEGRAM_CIRCUIT_BREAK
                        || !cfg.telegram_configured()
                    {
                        false
                    } else {
                        // parse_mode is HTML: a title like "Fix <br> in footer"
                        // is a 400 from Telegram, and three in a row trip the
                        // circuit breaker for every other nudge in the run.
                        // Cap the visible text at Telegram's limit too: an
                        // oversize title or HAL reply is a 400 just the same.
                        let prefix_units = format!("Task #{level}: ").encode_utf16().count();
                        let body = truncate_utf16(&message, TELEGRAM_TEXT_LIMIT - prefix_units);
                        let prefixed = format!("<b>Task #{}:</b> {}", level, html_escape(&body));
                        let buttons = nudge_buttons(&task.id);
                        // Reserve the budget slot before sending (refunded on
                        // failure), so a write failure after delivery cannot
                        // let the run exceed the daily budget. The reservation
                        // is conditional on the shared counter, not on this
                        // run's snapshot, so concurrent runs cannot overrun it.
                        let reserved = cfg.dry_run
                            || match reserve_daily_budget(db, &date_utc, DAILY_BUDGET_MAX) {
                                Ok(reserved) => reserved,
                                Err(e) => {
                                    release_claim(db, &task, &now_utc);
                                    return Err(e);
                                }
                            };
                        if !reserved {
                            // Not a delivery failure: the budget is spent.
                            // Stop Telegram for the rest of this run; the
                            // claim is released below if nothing else went.
                            info!(
                                target: "ptask::accountability",
                                task_uuid = %task.id,
                                "telegram budget spent by a concurrent run — skipping"
                            );
                            telegram_remaining = sent_telegrams;
                            continue;
                        }
                        let r = if cfg.dry_run {
                            true
                        } else {
                            delivered(
                                dispatch.send_telegram(cfg, &prefixed, &buttons).await,
                                "telegram",
                                &mut dispatched,
                            )
                        };
                        if r {
                            telegram_consecutive_failures = 0;
                            dispatched.telegram_sent = true;
                            sent_telegrams += 1;
                        } else {
                            if !cfg.dry_run {
                                refund_daily_budget(db, &date_utc);
                            }
                            report.send_failures += 1;
                            telegram_consecutive_failures += 1;
                            if telegram_consecutive_failures == TELEGRAM_CIRCUIT_BREAK {
                                error!(
                                    target: "ptask::accountability",
                                    failures = telegram_consecutive_failures,
                                    "telegram channel circuit-broken for this run — \
                                     suppressing further attempts"
                                );
                            }
                        }
                        r
                    }
                }
                "email" => {
                    if !cfg.email_configured() || email_broken {
                        false
                    } else {
                        let subject = format!(
                            "[PureTensor] Task escalated (level {}): {}",
                            level,
                            task.title.chars().take(60).collect::<String>()
                        );
                        let r = if cfg.dry_run {
                            true
                        } else {
                            delivered(
                                dispatch.send_email(cfg, &subject, &message).await,
                                "email",
                                &mut dispatched,
                            )
                        };
                        if r {
                            dispatched.email_sent = true;
                        } else {
                            report.send_failures += 1;
                            // SMTP failures are systemic (server stalled or
                            // down, credentials rejected) and each can cost the
                            // full 30 s send timeout: stop trying for this run.
                            email_broken = true;
                            error!(
                                target: "ptask::accountability",
                                "email send failed — email circuit-broken for the rest of this run"
                            );
                        }
                        r
                    }
                }
                _ => false,
            };
            if ok && !cfg.dry_run {
                let logged = log_notification(db, &task.id, channel, level, &message);
                after_send("notification log", logged, &mut dispatched);
            }
        }
        if !(dispatched.telegram_sent || dispatched.email_sent) {
            if !cfg.dry_run {
                release_claim(db, &task, &now_utc);
            }
        } else {
            if !cfg.dry_run && escalating {
                let persisted = set_escalation_level(db, &task.id, level);
                after_send("escalation level", persisted, &mut dispatched);
            }
            if escalating {
                info!(
                    target: "ptask::accountability",
                    task_uuid = %task.id,
                    level,
                    dry_run = cfg.dry_run,
                    title = %task.title.chars().take(60).collect::<String>(),
                    "escalated"
                );
            }
            report.dispatched.push(dispatched);
        }
    }
    report.budget_used_after = get_daily_budget(db, &date_utc)?;
    info!(
        target: "ptask::accountability",
        sent_telegrams,
        emails = report.dispatched.iter().filter(|d| d.email_sent).count(),
        eligible = report.eligible,
        send_failures = report.send_failures,
        "run_check complete"
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::EventCtx;
    use crate::tasks::{Extensions, NewTask, create_with_extensions};
    use rusqlite::params;

    fn fresh_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (
                    id               TEXT PRIMARY KEY,
                    title            TEXT NOT NULL,
                    description      TEXT DEFAULT '',
                    priority         INTEGER DEFAULT 2,
                    status           TEXT DEFAULT 'pending',
                    created_at       TEXT NOT NULL,
                    updated_at       TEXT NOT NULL,
                    deadline         TEXT,
                    source_type      TEXT DEFAULT 'manual',
                    source_files     TEXT DEFAULT '[]',
                    ai_confidence    REAL DEFAULT 1.0,
                    ai_reasoning     TEXT DEFAULT '',
                    depends_on       TEXT DEFAULT '[]',
                    blocks_tasks     TEXT DEFAULT '[]',
                    escalation_level INTEGER DEFAULT 0,
                    dismissal_count  INTEGER DEFAULT 0,
                    last_reminded    TEXT,
                    next_reminder    TEXT,
                    priority_score   REAL DEFAULT 0.0,
                    score_urgency    REAL DEFAULT 0.0,
                    score_dependency REAL DEFAULT 0.0,
                    score_neglect    REAL DEFAULT 0.0,
                    subtasks         TEXT DEFAULT '[]',
                    task_type        TEXT DEFAULT 'operational',
                    cluster_keywords TEXT DEFAULT '[]'
                 );
                 CREATE TABLE interactions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    action TEXT NOT NULL,
                    ts TEXT NOT NULL,
                    details TEXT DEFAULT ''
                 );
                 CREATE TABLE notifications (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    channel TEXT NOT NULL,
                    sent_at TEXT NOT NULL,
                    escalation_level INTEGER,
                    message_text TEXT,
                    dismissed INTEGER DEFAULT 0,
                    dismissed_at TEXT
                 );
                 CREATE TABLE daily_budget (
                    date TEXT PRIMARY KEY,
                    notifications_sent INTEGER DEFAULT 0
                 );",
            )
            .unwrap();
        }
        (dir, Db::open(&path).unwrap())
    }

    /// Build a task and back-date created_at relative to a `before` anchor
    /// so age_days computes deterministically regardless of wall clock.
    fn aged_task_before(db: &Db, title: &str, age_days: i64, before: &Zoned) -> String {
        let t = create_with_extensions(
            db,
            NewTask::minimal(title),
            Extensions::default(),
            &EventCtx::test(),
        )
        .unwrap();
        // Pad by an extra hour so integer-truncated age_days lands at or
        // above the requested value even when the anchor moves slightly.
        let created = before
            .checked_sub(jiff::Span::new().hours(age_days * 24 + 1))
            .unwrap();
        let iso = crate::dates::format_iso(&created);
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET created_at=?1, updated_at=?1 WHERE id=?2",
                params![iso, &t.id],
            )?;
            Ok(())
        })
        .unwrap();
        t.id
    }

    #[test]
    fn quiet_hours_match_22_to_08_london() {
        // The window is the operator's wall clock (Europe/London), so build
        // the probes IN that zone — during BST these are UTC+1.
        let tz = jiff::tz::TimeZone::get("Europe/London").unwrap();
        let mk = |h: i8| {
            jiff::civil::date(2026, 5, 13)
                .at(h, 0, 0, 0)
                .to_zoned(tz.clone())
                .unwrap()
        };
        assert!(in_quiet_hours_at(&mk(22)));
        assert!(in_quiet_hours_at(&mk(0)));
        assert!(in_quiet_hours_at(&mk(7)));
        assert!(!in_quiet_hours_at(&mk(8)));
        assert!(!in_quiet_hours_at(&mk(12)));
        assert!(!in_quiet_hours_at(&mk(21)));
    }

    #[test]
    fn daily_budget_increments_per_date() {
        let (_dir, db) = fresh_db();
        assert_eq!(get_daily_budget(&db, "2026-05-13").unwrap(), 0);
        assert_eq!(increment_daily_budget(&db, "2026-05-13").unwrap(), 1);
        assert_eq!(increment_daily_budget(&db, "2026-05-13").unwrap(), 2);
        assert_eq!(get_daily_budget(&db, "2026-05-13").unwrap(), 2);
        assert_eq!(get_daily_budget(&db, "2026-05-14").unwrap(), 0);
    }

    #[test]
    fn eligibility_respects_mixed_offset_snooze_and_reminder_instants() {
        let (_dir, db) = fresh_db();
        let ctx = EventCtx::test();
        let snoozed = create_with_extensions(
            &db,
            NewTask::minimal("future snooze"),
            Extensions::default(),
            &ctx,
        )
        .unwrap();
        let reminded = create_with_extensions(
            &db,
            NewTask::minimal("future reminder"),
            Extensions::default(),
            &ctx,
        )
        .unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET status='delayed', status_v2='snoozed',
                                  snoozed_until='2026-05-13T12:30:00Z'
                  WHERE id=?1",
                [&snoozed.id],
            )?;
            c.execute(
                "UPDATE tasks SET next_reminder='2026-05-13T12:30:00Z'
                  WHERE id=?1",
                [&reminded.id],
            )?;
            Ok(())
        })
        .unwrap();

        // 13:00 BST is 12:00 UTC. Both timestamps are still 30 minutes in
        // the future even though their hour fields sort before 13 lexically.
        let eligible = fetch_eligible(&db, "2026-05-13T13:00:00+01:00").unwrap();
        assert!(
            eligible.is_empty(),
            "unexpected eligible tasks: {eligible:?}"
        );
    }

    #[test]
    fn eligibility_not_suppressed_by_unparseable_timestamps() {
        // Regression: `NOT (… julianday(junk) > julianday(now))` evaluates to
        // NULL, not TRUE, so a malformed `snoozed_until` silently dropped the
        // task out of the reminder ladder — and a malformed `next_reminder`
        // did the same. Both must fail visible.
        let (_dir, db) = fresh_db();
        let ctx = EventCtx::test();
        let junk_snooze = create_with_extensions(
            &db,
            NewTask::minimal("unreadable snooze"),
            Extensions::default(),
            &ctx,
        )
        .unwrap();
        let junk_reminder = create_with_extensions(
            &db,
            NewTask::minimal("unreadable reminder"),
            Extensions::default(),
            &ctx,
        )
        .unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET status='delayed', status_v2='snoozed',
                                  snoozed_until='when the rack is quiet'
                  WHERE id=?1",
                [&junk_snooze.id],
            )?;
            c.execute(
                "UPDATE tasks SET next_reminder='later' WHERE id=?1",
                [&junk_reminder.id],
            )?;
            Ok(())
        })
        .unwrap();

        let eligible = fetch_eligible(&db, "2026-05-13T13:00:00+01:00").unwrap();
        let ids: Vec<&str> = eligible.iter().map(|t| t.id.as_str()).collect();
        assert!(
            ids.contains(&junk_snooze.id.as_str()),
            "unreadable snooze suppressed the task: {ids:?}"
        );
        assert!(
            ids.contains(&junk_reminder.id.as_str()),
            "unreadable next_reminder suppressed the task: {ids:?}"
        );
    }

    #[test]
    fn eligibility_treats_date_only_snooze_as_operator_midnight() {
        let (_dir, db) = fresh_db();
        let task = create_with_extensions(
            &db,
            NewTask::minimal("date-only snooze"),
            Extensions::default(),
            &EventCtx::test(),
        )
        .unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET status='delayed', status_v2='snoozed',
                                  snoozed_until='2026-05-13'
                  WHERE id=?1",
                [&task.id],
            )?;
            Ok(())
        })
        .unwrap();

        let eligible = fetch_eligible(&db, "2026-05-13T00:30:00+01:00").unwrap();
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, task.id);
    }

    #[test]
    fn should_advance_transitions_match_spec() {
        let now = Zoned::now().with_time_zone(jiff::tz::TimeZone::UTC);
        // (level, level_age_days, task_age_days, last_reminded_offset_secs)
        let mk = |level: i64, level_age_days: i64, age_days: i64, last_offset_secs: Option<i64>| {
            EligibleTask {
                id: "x".into(),
                title: "t".into(),
                occurrence_start: crate::dates::format_iso(
                    &now.checked_sub(jiff::Span::new().days(age_days)).unwrap(),
                ),
                next_reminder: None,
                last_reminded: last_offset_secs.map(|s| {
                    crate::dates::format_iso(
                        &now.checked_sub(jiff::Span::new().seconds(s)).unwrap(),
                    )
                }),
                dismissal_count: 0,
                escalation_level: level,
                level_changed_at: Some(crate::dates::format_iso(
                    &now.checked_sub(jiff::Span::new().days(level_age_days))
                        .unwrap(),
                )),
            }
        };
        // 0 → 1 at task age 2d.
        assert!(should_advance(&mk(0, 0, 2, None), 2, &now));
        assert!(!should_advance(&mk(0, 0, 1, None), 1, &now));
        // 1 → 2 after 3 days AT level 1 (time-at-level, not dismissals).
        assert!(should_advance(&mk(1, 3, 10, None), 10, &now));
        assert!(!should_advance(&mk(1, 2, 10, None), 10, &now));
        // 2 → 3 after 4 days at level.
        assert!(should_advance(&mk(2, 4, 20, None), 20, &now));
        assert!(!should_advance(&mk(2, 3, 20, None), 20, &now));
        // 3 → 4 after 2 days at level, even though level 3 emails every
        // 4h and so `last_reminded` is always recent.
        assert!(should_advance(&mk(3, 2, 20, Some(4 * 3600)), 20, &now));
        assert!(!should_advance(&mk(3, 1, 20, Some(48 * 3600)), 20, &now));
        // 4 → 5 after 7 days at level.
        assert!(should_advance(&mk(4, 7, 30, Some(4 * 3600)), 30, &now));
        assert!(!should_advance(&mk(4, 6, 30, Some(7 * 86_400)), 30, &now));
        // Level 5 never advances.
        assert!(!should_advance(&mk(5, 99, 99, Some(99 * 86_400)), 99, &now));
    }

    #[test]
    fn fallback_message_per_level_is_short_and_concrete() {
        let t = EligibleTask {
            id: "x".into(),
            title: "Renew SSL".into(),
            occurrence_start: "2026-05-01T00:00:00+00:00".into(),
            last_reminded: None,
            next_reminder: None,
            dismissal_count: 2,
            escalation_level: 0,
            level_changed_at: None,
        };
        for lv in 1..=5 {
            let m = fallback_message(&t, lv, 5);
            assert!(!m.is_empty());
            assert!(m.len() < 200, "level {} message too long: {:?}", lv, m);
            assert!(m.contains("Renew SSL"));
        }
    }

    /// Anchor at 12:00 UTC so quiet hours don't fire regardless of when CI
    /// runs the test.
    fn noon_utc() -> Zoned {
        jiff::civil::date(2026, 5, 13)
            .at(12, 0, 0, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
    }

    /// Dispatch fake: every send succeeds. Dry-run tests never reach it,
    /// but the type is still required by the signature.
    struct SendOk;
    impl Dispatch for SendOk {
        async fn send_telegram(
            &self,
            _cfg: &DispatchCfg,
            _text: &str,
            _buttons: &[(String, String)],
        ) -> Result<bool> {
            Ok(true)
        }
        async fn send_email(&self, _cfg: &DispatchCfg, _s: &str, _b: &str) -> Result<bool> {
            Ok(true)
        }
        async fn compose_via_hal(&self, _cfg: &DispatchCfg, _req: &NudgeRequest) -> Option<String> {
            None
        }
    }

    /// Dispatch fake: every attempted send fails (delivery failure, not
    /// misconfiguration) — the dead-bot-token mode.
    struct SendFail;
    impl Dispatch for SendFail {
        async fn send_telegram(
            &self,
            _cfg: &DispatchCfg,
            _text: &str,
            _buttons: &[(String, String)],
        ) -> Result<bool> {
            Ok(false)
        }
        async fn send_email(&self, _cfg: &DispatchCfg, _s: &str, _b: &str) -> Result<bool> {
            Ok(false)
        }
        async fn compose_via_hal(&self, _cfg: &DispatchCfg, _req: &NudgeRequest) -> Option<String> {
            None
        }
    }

    #[tokio::test]
    async fn dry_run_reports_dispatch_without_mutating_database() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        // Two-day-old task would advance to level 1 and dispatch via Telegram.
        let task_uuid = aged_task_before(&db, "Renew SSL certs", 2, &anchor);
        let cfg = DispatchCfg {
            telegram_token: Some("test".into()),
            telegram_chat_id: Some(1),
            dry_run: true,
            ..Default::default()
        };
        let report = run_check_at(&db, &cfg, &SendOk, &anchor).await.unwrap();
        assert_eq!(report.eligible, 1);
        assert_eq!(report.dispatched.len(), 1);
        assert!(report.dispatched[0].telegram_sent);
        assert_eq!(report.dispatched[0].level, 1);
        assert_eq!(report.budget_used_before, 0);
        assert_eq!(report.budget_used_after, 0);

        db.with_conn(|c| {
            let (level, last): (i64, Option<String>) = c
                .query_row(
                    "SELECT escalation_level, last_reminded FROM tasks WHERE id=?1",
                    [&task_uuid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(level, 0);
            assert!(last.is_none());
            let notifications: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM notifications WHERE task_id=?1",
                    [&task_uuid],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(notifications, 0);
            let escalations: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM interactions WHERE task_id=?1 AND action='escalation'",
                    [&task_uuid],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(escalations, 0);
            let budget = get_daily_budget(&db, &anchor.date().to_string()).unwrap();
            assert_eq!(budget, 0);
            Ok(())
        })
        .unwrap();
    }

    #[tokio::test]
    async fn dead_telegram_endpoint_circuit_breaks_and_counts_failures() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        for i in 0..5 {
            aged_task_before(&db, &format!("stale task {}", i), 3, &anchor);
        }
        let cfg = DispatchCfg {
            telegram_token: Some("test".into()),
            telegram_chat_id: Some(1),
            ..Default::default()
        };
        let report = run_check_at(&db, &cfg, &SendFail, &anchor).await.unwrap();
        assert_eq!(report.eligible, 5);
        assert_eq!(
            report.dispatched.len(),
            0,
            "nothing dispatched on a dead channel"
        );
        // Exactly CIRCUIT_BREAK attempts, then the channel is suppressed for
        // the rest of the run — not one failure per eligible task.
        assert_eq!(report.send_failures, TELEGRAM_CIRCUIT_BREAK);
    }

    #[tokio::test]
    async fn run_check_respects_daily_telegram_budget() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        let today = anchor.date().to_string();
        for _ in 0..DAILY_BUDGET_MAX {
            increment_daily_budget(&db, &today).unwrap();
        }
        aged_task_before(&db, "stale task A", 3, &anchor);
        aged_task_before(&db, "stale task B", 3, &anchor);
        let cfg = DispatchCfg {
            telegram_token: Some("test".into()),
            telegram_chat_id: Some(1),
            dry_run: true,
            ..Default::default()
        };
        let report = run_check_at(&db, &cfg, &SendOk, &anchor).await.unwrap();
        assert_eq!(report.dispatched.len(), 0);
        assert_eq!(report.budget_used_before, DAILY_BUDGET_MAX);
        assert_eq!(report.budget_used_after, DAILY_BUDGET_MAX);
    }

    #[tokio::test]
    async fn exhausted_telegram_budget_still_allows_unbudgeted_email() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        let today = anchor.date().to_string();
        for _ in 0..DAILY_BUDGET_MAX {
            increment_daily_budget(&db, &today).unwrap();
        }
        let task_uuid = aged_task_before(&db, "email escalation", 5, &anchor);
        // Reached level 3 just now, so the run reminds at 3 without advancing.
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET escalation_level=3, level_changed_at=?1 WHERE id=?2",
                params![crate::dates::format_iso(&anchor), &task_uuid],
            )?;
            Ok(())
        })
        .unwrap();

        let cfg = DispatchCfg {
            telegram_token: Some("test".into()),
            telegram_chat_id: Some(1),
            smtp_host: Some("smtp.example.test".into()),
            smtp_user: Some("hal@puretensor.ai".into()),
            smtp_pass: Some("secret".into()),
            notify_email: Some("heimir@example.test".into()),
            dry_run: true,
            ..Default::default()
        };
        let report = run_check_at(&db, &cfg, &SendOk, &anchor).await.unwrap();
        assert_eq!(report.dispatched.len(), 1);
        assert!(!report.dispatched[0].telegram_sent);
        assert!(report.dispatched[0].email_sent);
        assert_eq!(report.dispatched[0].level, 3);
        assert_eq!(report.budget_used_before, DAILY_BUDGET_MAX);
        assert_eq!(report.budget_used_after, DAILY_BUDGET_MAX);
    }

    #[tokio::test]
    async fn level_five_escalation_does_not_block_the_task() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        let task_uuid = aged_task_before(&db, "final escalation", 30, &anchor);
        let last =
            crate::dates::format_iso(&anchor.checked_sub(jiff::Span::new().hours(8 * 24)).unwrap());
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET escalation_level=4, last_reminded=?1, level_changed_at=?1 WHERE id=?2",
                params![last, &task_uuid],
            )?;
            Ok(())
        })
        .unwrap();

        let cfg = DispatchCfg {
            smtp_host: Some("smtp.example.test".into()),
            smtp_user: Some("hal@puretensor.ai".into()),
            smtp_pass: Some("secret".into()),
            notify_email: Some("heimir@example.test".into()),
            dry_run: false,
            ..Default::default()
        };
        let report = run_check_at(&db, &cfg, &SendOk, &anchor).await.unwrap();
        assert_eq!(report.dispatched.len(), 1);
        assert_eq!(report.dispatched[0].level, 5);
        let (level, status): (i64, String) = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT escalation_level, status FROM tasks WHERE id=?1",
                    [&task_uuid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();
        assert_eq!(level, 5);
        assert_eq!(
            status, "pending",
            "level 5 must not hide the task as blocked"
        );
    }

    fn level_and_updated_at(db: &Db, task_uuid: &str) -> (i64, String) {
        db.with_conn(|c| {
            Ok(c.query_row(
                "SELECT escalation_level, updated_at FROM tasks WHERE id=?1",
                [task_uuid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap()
    }

    fn email_cfg() -> DispatchCfg {
        DispatchCfg {
            smtp_host: Some("smtp.example.test".into()),
            smtp_user: Some("hal@puretensor.ai".into()),
            smtp_pass: Some("secret".into()),
            notify_email: Some("heimir@example.test".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn level_three_reminded_every_four_hours_still_reaches_level_four() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        let task_uuid = aged_task_before(&db, "emailed every 4h", 20, &anchor);
        let at = |h: i64| {
            crate::dates::format_iso(&anchor.checked_sub(jiff::Span::new().hours(h)).unwrap())
        };
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET escalation_level=3, level_changed_at=?1, last_reminded=?2
                 WHERE id=?3",
                params![at(49), at(4), &task_uuid],
            )?;
            Ok(())
        })
        .unwrap();
        let (_, updated_before) = level_and_updated_at(&db, &task_uuid);

        let report = run_check_at(&db, &email_cfg(), &SendOk, &anchor)
            .await
            .unwrap();
        assert_eq!(report.dispatched.len(), 1);
        assert_eq!(report.dispatched[0].level, 4);
        let (level, updated_after) = level_and_updated_at(&db, &task_uuid);
        assert_eq!(level, 4);
        assert_eq!(
            updated_after, updated_before,
            "an escalation is not an operator touch (neglect score, reaper)"
        );
    }

    #[tokio::test]
    async fn undelivered_escalation_is_not_persisted() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        let task_uuid = aged_task_before(&db, "email down", 30, &anchor);
        let long_ago =
            crate::dates::format_iso(&anchor.checked_sub(jiff::Span::new().hours(8 * 24)).unwrap());
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET escalation_level=4, last_reminded=?1, level_changed_at=?1
                 WHERE id=?2",
                params![long_ago, &task_uuid],
            )?;
            Ok(())
        })
        .unwrap();

        // The level-5 email fails: the task must stay at 4 (still eligible,
        // retried next run) instead of sitting at 5 with no notice sent.
        let report = run_check_at(&db, &email_cfg(), &SendFail, &anchor)
            .await
            .unwrap();
        assert!(report.dispatched.is_empty());
        assert_eq!(level_and_updated_at(&db, &task_uuid).0, 4);
        let escalations: i64 = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM interactions WHERE task_id=?1 AND action='escalation'",
                    [&task_uuid],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(escalations, 0);

        let report = run_check_at(&db, &email_cfg(), &SendOk, &anchor)
            .await
            .unwrap();
        assert_eq!(report.dispatched.len(), 1);
        assert_eq!(report.dispatched[0].level, 5);
        assert_eq!(level_and_updated_at(&db, &task_uuid).0, 5);
    }

    /// Telegram delivers; email reports misconfiguration (`Err`), the way a
    /// bad NOTIFY_EMAIL/CC address used to.
    struct TelegramOkEmailErr;
    impl Dispatch for TelegramOkEmailErr {
        async fn send_telegram(
            &self,
            _cfg: &DispatchCfg,
            _text: &str,
            _buttons: &[(String, String)],
        ) -> Result<bool> {
            Ok(true)
        }
        async fn send_email(&self, _cfg: &DispatchCfg, _s: &str, _b: &str) -> Result<bool> {
            Err(Error::Other(
                "invalid NOTIFY_EMAIL \"not an address\"".into(),
            ))
        }
        async fn compose_via_hal(&self, _cfg: &DispatchCfg, _req: &NudgeRequest) -> Option<String> {
            None
        }
    }

    /// Regression (DIST-8): an email `Err` was propagated with `?` after the
    /// Telegram nudge had already gone out but before the reminder stamp was
    /// written, aborting the run — so every following run re-sent it.
    #[tokio::test]
    async fn an_email_error_does_not_skip_stamping_the_delivered_telegram() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        let task_uuid = aged_task_before(&db, "escalated with broken email", 20, &anchor);
        let other = aged_task_before(&db, "second escalated task", 20, &anchor);
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET escalation_level=3, level_changed_at=?1",
                params![crate::dates::format_iso(&anchor)],
            )?;
            Ok(())
        })
        .unwrap();
        let cfg = DispatchCfg {
            telegram_token: Some("test".into()),
            telegram_chat_id: Some(1),
            ..email_cfg()
        };
        let report = run_check_at(&db, &cfg, &TelegramOkEmailErr, &anchor)
            .await
            .expect("one channel's misconfiguration must not abort the run");
        assert_eq!(report.dispatched.len(), 2, "both tasks still nudged");
        assert!(
            report
                .dispatched
                .iter()
                .all(|d| d.telegram_sent && !d.email_sent)
        );
        assert!(report.dispatched[0].error.is_some());
        // One email failure: the email circuit breaker skips the second.
        assert_eq!(report.send_failures, 1);
        for id in [&task_uuid, &other] {
            let last: Option<String> = db
                .with_conn(|c| {
                    Ok(
                        c.query_row("SELECT last_reminded FROM tasks WHERE id=?1", [id], |r| {
                            r.get(0)
                        })?,
                    )
                })
                .unwrap();
            assert!(last.is_some(), "delivered nudge was not stamped");
        }
        // The stamp holds: an immediate re-run sends nothing new.
        let again = run_check_at(&db, &cfg, &TelegramOkEmailErr, &anchor)
            .await
            .unwrap();
        assert!(again.dispatched.is_empty());
    }

    /// Regression (DIST-13): level 5 is email-only, so with email
    /// unconfigured the 4 → 5 notice could never be delivered, the level
    /// never persisted, and the task went silent for good. Likewise a
    /// Telegram-only level with only email configured.
    #[tokio::test]
    async fn a_level_falls_back_to_the_configured_channel() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        let task_uuid = aged_task_before(&db, "final notice, telegram only", 30, &anchor);
        let long_ago =
            crate::dates::format_iso(&anchor.checked_sub(jiff::Span::new().hours(8 * 24)).unwrap());
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET escalation_level=4, last_reminded=?1, level_changed_at=?1
                 WHERE id=?2",
                params![long_ago, &task_uuid],
            )?;
            Ok(())
        })
        .unwrap();
        let telegram_only = DispatchCfg {
            telegram_token: Some("test".into()),
            telegram_chat_id: Some(1),
            ..Default::default()
        };
        let report = run_check_at(&db, &telegram_only, &SendOk, &anchor)
            .await
            .unwrap();
        assert_eq!(report.dispatched.len(), 1, "level 5 must reach Telegram");
        assert_eq!(report.dispatched[0].level, 5);
        assert!(report.dispatched[0].telegram_sent);
        assert_eq!(level_and_updated_at(&db, &task_uuid).0, 5);
        assert_eq!(report.budget_used_after, 1, "the fallback is budgeted");

        let (_dir, db) = fresh_db();
        aged_task_before(&db, "first reminder, email only", 2, &anchor);
        let report = run_check_at(&db, &email_cfg(), &SendOk, &anchor)
            .await
            .unwrap();
        assert_eq!(report.dispatched.len(), 1, "level 1 must reach email");
        assert!(report.dispatched[0].email_sent);
        assert!(!report.dispatched[0].telegram_sent);
    }

    /// 12:00 UTC `days` after today: after any wall-clock write the task API
    /// makes (mark_done/reopen stamp the real now), and outside quiet hours.
    fn noon_utc_days_from_today(days: i64) -> Zoned {
        jiff::Zoned::now()
            .with_time_zone(jiff::tz::TimeZone::UTC)
            .date()
            .checked_add(jiff::Span::new().days(days))
            .unwrap()
            .at(12, 0, 0, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
    }

    fn telegram_cfg() -> DispatchCfg {
        DispatchCfg {
            telegram_token: Some("test".into()),
            telegram_chat_id: Some(1),
            ..Default::default()
        }
    }

    /// Regression (PARSE-10): advancing a recurring task never reset its
    /// escalation, and age came from `created_at`, so a daily task completed
    /// on schedule every day still climbed to level 5 ("Day 33").
    #[tokio::test]
    async fn an_on_schedule_recurring_task_restarts_the_ladder_each_occurrence() {
        let (_dir, db) = fresh_db();
        let tomorrow = noon_utc_days_from_today(1);
        let mut new = NewTask::minimal("water the plants");
        new.deadline = Some(crate::dates::format_iso(&tomorrow));
        let task = create_with_extensions(
            &db,
            new,
            Extensions {
                recurrence: Some(crate::recurrence::parse("every day").unwrap()),
                ..Default::default()
            },
            &EventCtx::test(),
        )
        .unwrap();
        let ago = |days: i64| {
            crate::dates::format_iso(&tomorrow.checked_sub(jiff::Span::new().days(days)).unwrap())
        };
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET created_at=?1, escalation_level=4,
                                  level_changed_at=?2, last_reminded=?2
                  WHERE id=?3",
                params![ago(30), ago(8), &task.id],
            )?;
            Ok(())
        })
        .unwrap();
        let outcome = crate::tasks::mark_done(&db, &task, &EventCtx::test()).unwrap();
        assert!(matches!(
            outcome,
            crate::tasks::DoneOutcome::Advanced { .. }
        ));
        assert_eq!(
            level_and_updated_at(&db, &task.id).0,
            0,
            "advance resets the ladder"
        );

        let report = run_check_at(&db, &telegram_cfg(), &SendOk, &tomorrow)
            .await
            .unwrap();
        assert!(
            report.dispatched.is_empty(),
            "a just-completed occurrence is not overdue: {:?}",
            report.dispatched
        );

        // Left alone for three days, the new occurrence starts at level 1
        // and counts its age from the advance, not from creation.
        let later = noon_utc_days_from_today(3);
        let dispatch = RecordTelegram::default();
        let report = run_check_at(&db, &telegram_cfg(), &dispatch, &later)
            .await
            .unwrap();
        assert_eq!(report.dispatched.len(), 1);
        assert_eq!(report.dispatched[0].level, 1);
        assert!(
            report.dispatched[0].message.contains("Day 2")
                || report.dispatched[0].message.contains("Day 3"),
            "{}",
            report.dispatched[0].message
        );
    }

    /// Regression (PARSE-10): a task reopened after reaching level 5 kept
    /// `escalation_level = 5` and was excluded from the ladder forever.
    #[tokio::test]
    async fn a_task_reopened_after_level_five_rejoins_the_ladder() {
        let (_dir, db) = fresh_db();
        let task = create_with_extensions(
            &db,
            NewTask::minimal("renew the domain"),
            Extensions::default(),
            &EventCtx::test(),
        )
        .unwrap();
        let anchor = noon_utc_days_from_today(3);
        let long_ago =
            crate::dates::format_iso(&anchor.checked_sub(jiff::Span::new().days(40)).unwrap());
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET created_at=?1, escalation_level=5, level_changed_at=?1,
                                  last_reminded=?1, status='done', status_v2='done'
                  WHERE id=?2",
                params![long_ago, &task.id],
            )?;
            Ok(())
        })
        .unwrap();
        crate::tasks::reopen(&db, &task.id, &EventCtx::test()).unwrap();
        assert_eq!(level_and_updated_at(&db, &task.id).0, 0);
        let report = run_check_at(&db, &telegram_cfg(), &SendOk, &anchor)
            .await
            .unwrap();
        assert_eq!(report.eligible, 1, "reopened task is eligible again");
        assert_eq!(report.dispatched.len(), 1);
        assert_eq!(report.dispatched[0].level, 1, "the ladder restarts");
    }

    /// Completes the task while the message is being composed — the operator
    /// tapping Done (or another run) between `fetch_eligible` and the send.
    struct CompletesDuringCompose {
        db: Db,
        sent: RecordTelegram,
    }
    impl Dispatch for CompletesDuringCompose {
        async fn send_telegram(
            &self,
            cfg: &DispatchCfg,
            text: &str,
            buttons: &[(String, String)],
        ) -> Result<bool> {
            self.sent.send_telegram(cfg, text, buttons).await
        }
        async fn send_email(&self, _cfg: &DispatchCfg, _s: &str, _b: &str) -> Result<bool> {
            Ok(true)
        }
        async fn compose_via_hal(&self, _cfg: &DispatchCfg, req: &NudgeRequest) -> Option<String> {
            self.db
                .with_conn(|c| {
                    c.execute(
                        "UPDATE tasks SET status='done', status_v2='done' WHERE id=?1",
                        [&req.task_uuid],
                    )?;
                    Ok(())
                })
                .unwrap();
            None
        }
    }

    /// Regression (PARSE-12): the task row was read once, before composing
    /// and sending, so a task completed in the meantime was still nudged.
    #[tokio::test]
    async fn task_state_is_rechecked_immediately_before_sending() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        let task_uuid = aged_task_before(&db, "done while composing", 3, &anchor);
        let cfg = DispatchCfg {
            hal_nudge_url: Some("http://hal.invalid/nudge".into()),
            ..telegram_cfg()
        };
        let dispatch = CompletesDuringCompose {
            db: db.clone(),
            sent: RecordTelegram::default(),
        };
        let report = run_check_at(&db, &cfg, &dispatch, &anchor).await.unwrap();
        assert!(
            dispatch.sent.0.lock().unwrap().is_empty(),
            "a completed task was nudged"
        );
        assert!(report.dispatched.is_empty());
        assert_eq!(
            get_daily_budget(&db, &anchor.date().to_string()).unwrap(),
            0
        );
        let last: Option<String> = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT last_reminded FROM tasks WHERE id=?1",
                    [&task_uuid],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert!(last.is_none());
    }

    /// Regression (PARSE-12): the reminder was recorded only after the send,
    /// so a database write failing after a delivered nudge aborted the run
    /// unstamped and every following run sent it again, outside the budget.
    #[tokio::test]
    async fn a_write_failure_after_sending_does_not_repeat_the_nudge() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        aged_task_before(&db, "nudged once only", 3, &anchor);
        db.with_conn(|c| {
            c.execute_batch(
                "CREATE TRIGGER reject_notification BEFORE INSERT ON notifications
                 BEGIN SELECT RAISE(ABORT, 'simulated disk full'); END;",
            )?;
            Ok(())
        })
        .unwrap();
        let dispatch = RecordTelegram::default();
        for _ in 0..3 {
            let _ = run_check_at(&db, &telegram_cfg(), &dispatch, &anchor).await;
        }
        assert_eq!(
            dispatch.0.lock().unwrap().len(),
            1,
            "the nudge was repeated after a post-send write failure"
        );
        assert_eq!(
            get_daily_budget(&db, &anchor.date().to_string()).unwrap(),
            1
        );
    }

    /// Telegram delivers; every email attempt fails (a stalled SMTP server
    /// that times out after 30 s in production). Counts both.
    #[derive(Default)]
    struct EmailStalls {
        telegrams: std::sync::atomic::AtomicUsize,
        emails: std::sync::atomic::AtomicUsize,
    }
    impl Dispatch for EmailStalls {
        async fn send_telegram(
            &self,
            _cfg: &DispatchCfg,
            _text: &str,
            _buttons: &[(String, String)],
        ) -> Result<bool> {
            self.telegrams
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(true)
        }
        async fn send_email(&self, _cfg: &DispatchCfg, _s: &str, _b: &str) -> Result<bool> {
            self.emails
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(false)
        }
        async fn compose_via_hal(&self, _cfg: &DispatchCfg, _req: &NudgeRequest) -> Option<String> {
            None
        }
    }

    /// Regression (round 2, optional): a stalled SMTP server cost the full
    /// 30 s send timeout for every level 3/4 task in a run. After the first
    /// failed email the channel is skipped for the rest of the run; Telegram
    /// still goes out.
    #[tokio::test]
    async fn email_is_circuit_broken_after_the_first_failure_in_a_run() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        for i in 0..3 {
            aged_task_before(&db, &format!("escalated task {i}"), 20, &anchor);
        }
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET escalation_level=3, level_changed_at=?1",
                params![crate::dates::format_iso(&anchor)],
            )?;
            Ok(())
        })
        .unwrap();
        let cfg = DispatchCfg {
            telegram_token: Some("test".into()),
            telegram_chat_id: Some(1),
            ..email_cfg()
        };
        let dispatch = EmailStalls::default();
        let report = run_check_at(&db, &cfg, &dispatch, &anchor).await.unwrap();
        assert_eq!(
            dispatch.emails.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "email retried against a dead server"
        );
        assert_eq!(
            dispatch.telegrams.load(std::sync::atomic::Ordering::SeqCst),
            3
        );
        assert_eq!(report.dispatched.len(), 3);
        assert_eq!(report.send_failures, 1);
    }

    /// Counts Telegram sends and yields inside each one, so two runs driven
    /// by one `join!` interleave at every send.
    #[derive(Default)]
    struct YieldingTelegram(std::sync::atomic::AtomicUsize);
    impl Dispatch for YieldingTelegram {
        async fn send_telegram(
            &self,
            _cfg: &DispatchCfg,
            _text: &str,
            _buttons: &[(String, String)],
        ) -> Result<bool> {
            tokio::task::yield_now().await;
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(true)
        }
        async fn send_email(&self, _cfg: &DispatchCfg, _s: &str, _b: &str) -> Result<bool> {
            Ok(true)
        }
        async fn compose_via_hal(&self, _cfg: &DispatchCfg, _req: &NudgeRequest) -> Option<String> {
            None
        }
    }

    /// Regression (round 2, PARSE-12): `telegram_remaining` was computed once
    /// per run and the budget increment was unconditional, so two concurrent
    /// runs each spent the full daily budget (6 sends, counter 6, max 3).
    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_runs_cannot_overrun_the_daily_budget() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        for i in 0..8 {
            aged_task_before(&db, &format!("stale concurrent task {i}"), 3, &anchor);
        }
        let dispatch = YieldingTelegram::default();
        let cfg = telegram_cfg();
        let (a, b) = tokio::join!(
            run_check_at(&db, &cfg, &dispatch, &anchor),
            run_check_at(&db, &cfg, &dispatch, &anchor)
        );
        a.unwrap();
        b.unwrap();
        let sent = dispatch.0.load(std::sync::atomic::Ordering::SeqCst) as i64;
        let budget = get_daily_budget(&db, &anchor.date().to_string()).unwrap();
        assert!(sent <= DAILY_BUDGET_MAX, "{sent} telegrams sent");
        assert!(budget <= DAILY_BUDGET_MAX, "budget counter {budget}");
        assert_eq!(sent, budget, "every send is reserved exactly once");
    }

    /// Records every Telegram body it is asked to send.
    #[derive(Default)]
    struct RecordTelegram(std::sync::Mutex<Vec<String>>);
    impl Dispatch for RecordTelegram {
        async fn send_telegram(
            &self,
            _cfg: &DispatchCfg,
            text: &str,
            _buttons: &[(String, String)],
        ) -> Result<bool> {
            self.0.lock().unwrap().push(text.to_string());
            Ok(true)
        }
        async fn send_email(&self, _cfg: &DispatchCfg, _s: &str, _b: &str) -> Result<bool> {
            Ok(true)
        }
        async fn compose_via_hal(&self, _cfg: &DispatchCfg, _req: &NudgeRequest) -> Option<String> {
            None
        }
    }

    /// Telegram counts the 4096 limit in UTF-16 code units of the text
    /// after HTML entity parsing.
    fn rendered_utf16_len(html: &str) -> usize {
        html.replace("<b>", "")
            .replace("</b>", "")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
            .encode_utf16()
            .count()
    }

    /// HAL composes whatever it likes; this one ignores any length budget.
    struct LongHal(RecordTelegram);
    impl Dispatch for LongHal {
        async fn send_telegram(
            &self,
            cfg: &DispatchCfg,
            text: &str,
            buttons: &[(String, String)],
        ) -> Result<bool> {
            self.0.send_telegram(cfg, text, buttons).await
        }
        async fn send_email(&self, _cfg: &DispatchCfg, _s: &str, _b: &str) -> Result<bool> {
            Ok(true)
        }
        async fn compose_via_hal(&self, _cfg: &DispatchCfg, _req: &NudgeRequest) -> Option<String> {
            Some("&<🔥>".repeat(3_000))
        }
    }

    /// Regression (DIST-9/PARSE-11): the fallback message embedded the full
    /// title and the HAL message was uncapped, so a long title went over
    /// Telegram's 4096 limit, got a 400, and three of those tripped the
    /// circuit breaker for every other nudge in the run.
    #[tokio::test]
    async fn telegram_nudges_fit_the_limit_in_utf16_units() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        aged_task_before(&db, &"Fix 😀 <emoji> & co ".repeat(400), 2, &anchor);
        let cfg = DispatchCfg {
            telegram_token: Some("test".into()),
            telegram_chat_id: Some(1),
            ..Default::default()
        };
        let dispatch = RecordTelegram::default();
        run_check_at(&db, &cfg, &dispatch, &anchor).await.unwrap();
        let sent = dispatch.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        let len = rendered_utf16_len(&sent[0]);
        assert!(len <= 4096, "fallback nudge is {len} UTF-16 units");
        assert!(sent[0].starts_with("<b>Task #1:</b> Still open: Fix 😀"));

        let (_dir, db) = fresh_db();
        aged_task_before(&db, "short title", 2, &anchor);
        let cfg = DispatchCfg {
            hal_nudge_url: Some("http://hal.invalid/nudge".into()),
            ..cfg
        };
        let dispatch = LongHal(RecordTelegram::default());
        run_check_at(&db, &cfg, &dispatch, &anchor).await.unwrap();
        let sent = dispatch.0.0.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let len = rendered_utf16_len(&sent[0]);
        assert!(len <= 4096, "HAL nudge is {len} UTF-16 units");
        // Truncation happens before escaping, so no entity is ever split.
        let body = sent[0].strip_prefix("<b>Task #1:</b> ").unwrap();
        assert!(!body.contains('<') && !body.contains('>'));
        for (i, _) in body.match_indices('&') {
            let rest = &body[i..];
            assert!(
                ["&amp;", "&lt;", "&gt;"]
                    .iter()
                    .any(|e| rest.starts_with(e)),
                "split entity at {i}: {:?}",
                &rest[..rest.len().min(8)]
            );
        }
    }

    #[tokio::test]
    async fn telegram_nudge_escapes_the_title_for_html_parse_mode() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        aged_task_before(&db, "Fix <br> & <p> in the email footer", 2, &anchor);
        let cfg = DispatchCfg {
            telegram_token: Some("test".into()),
            telegram_chat_id: Some(1),
            ..Default::default()
        };
        let dispatch = RecordTelegram::default();
        run_check_at(&db, &cfg, &dispatch, &anchor).await.unwrap();
        let sent = dispatch.0.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].starts_with("<b>Task #1:</b> "));
        assert!(
            sent[0].contains("Fix &lt;br&gt; &amp; &lt;p&gt; in the email footer"),
            "{}",
            sent[0]
        );
    }

    #[tokio::test]
    async fn run_check_skips_during_quiet_hours() {
        let (_dir, db) = fresh_db();
        let quiet = jiff::civil::date(2026, 5, 13)
            .at(3, 0, 0, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap();
        aged_task_before(&db, "old task", 5, &quiet);
        let cfg = DispatchCfg {
            telegram_token: Some("test".into()),
            telegram_chat_id: Some(1),
            dry_run: true,
            ..Default::default()
        };
        let report = run_check_at(&db, &cfg, &SendOk, &quiet).await.unwrap();
        assert!(report.quiet_hours);
        assert_eq!(report.dispatched.len(), 0);
    }

    #[tokio::test]
    async fn run_check_is_a_noop_during_quiet_hours() {
        // 03:00 UTC = quiet. We can't easily inject Zoned::now, so this test
        // confirms the predicate via in_quiet_hours_at; the integration test
        // above already proves the non-quiet path.
        let z = jiff::civil::date(2026, 5, 13)
            .at(3, 0, 0, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap();
        assert!(in_quiet_hours_at(&z));
    }
}
