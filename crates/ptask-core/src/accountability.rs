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
//!   5  blocked    — email only; task.status flips to 'blocked'
//!
//! Transitions:
//!
//!   0 → 1  age_days ≥ 2
//!   1 → 2  dismissal_count ≥ 1
//!   2 → 3  dismissal_count ≥ 3
//!   3 → 4  last_reminded ≥ 48h ago
//!   4 → 5  last_reminded ≥ 7 days ago
//!
//! Budgets:
//!
//!   - Daily budget: at most 3 Telegram sends per UTC day (counted in
//!     `daily_budget`). Email is unbudgeted.
//!   - Per-task cooldown: ≥ 4 hours between reminders.
//!   - Quiet hours: 22:00 — 08:00 UTC (no sends).
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
    created_at: String,
    last_reminded: Option<String>,
    dismissal_count: i64,
    escalation_level: i64,
}

/// True when the UTC hour of `z` is in the quiet window 22:00 — 08:00 UTC.
pub fn in_quiet_hours_at(z: &Zoned) -> bool {
    let h = z.with_time_zone(jiff::tz::TimeZone::UTC).hour();
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

fn fetch_eligible(db: &Db, now_iso: &str) -> Result<Vec<EligibleTask>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, title, created_at, last_reminded,
                COALESCE(dismissal_count, 0), COALESCE(escalation_level, 0)
         FROM tasks
         WHERE status IN ('pending', 'delayed')
           AND NOT (COALESCE(status_v2,'') = 'snoozed'
                    AND snoozed_until IS NOT NULL AND snoozed_until > ?1)
           AND (next_reminder IS NULL OR next_reminder <= ?1)
           AND COALESCE(escalation_level, 0) < 5
         ORDER BY priority_score DESC, priority DESC, created_at DESC",
    )?;
    let rows = stmt.query_map([now_iso], |r| {
        Ok(EligibleTask {
            id: r.get(0)?,
            title: r.get(1)?,
            created_at: r.get(2)?,
            last_reminded: r.get(3)?,
            dismissal_count: r.get(4)?,
            escalation_level: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn task_age_days(task: &EligibleTask, now: &Zoned) -> i64 {
    let Some(created) = parse_iso_to_utc(&task.created_at) else {
        return 0;
    };
    let delta = now.timestamp().as_second() - created.timestamp().as_second();
    delta / 86_400
}

fn should_advance(task: &EligibleTask, age_days: i64, now: &Zoned) -> bool {
    let level = task.escalation_level;
    let last = task.last_reminded.as_deref().and_then(parse_iso_to_utc);
    let secs_since = |z: &Zoned| now.timestamp().as_second() - z.timestamp().as_second();
    match level {
        0 => age_days >= 2,
        1 => task.dismissal_count >= 1,
        2 => task.dismissal_count >= 3,
        3 => last.as_ref().is_some_and(|z| secs_since(z) >= 48 * 3600),
        4 => last.as_ref().is_some_and(|z| secs_since(z) >= 7 * 86_400),
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

fn channels_for(level: i64) -> &'static [&'static str] {
    match level {
        1 | 2 => &["telegram"],
        3 | 4 => &["telegram", "email"],
        5 => &["email"],
        _ => &[],
    }
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
            "{}: deferred {}× over {} days. State the concrete consequence to yourself.",
            task.title, task.dismissal_count, age_days
        ),
        4 => format!(
            "{}: {} days open, deferred {}×. Action today or this becomes a blocker.",
            task.title, age_days, task.dismissal_count
        ),
        5 => format!(
            "{}: {} days dormant. Marked BLOCKED — fix it or kill it.",
            task.title, age_days
        ),
        _ => format!("Task pending {} days: {}", age_days, task.title),
    }
}

/// Update tasks(escalation_level=N), log to interactions, and record an
/// attributed `task.escalated` event — escalations used to be invisible to
/// the journal (and thus to delta sync and the audit trail).
fn set_escalation_level(db: &Db, task_uuid: &str, level: i64) -> Result<()> {
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let now = crate::dates::format_iso(&crate::dates::now_in_operator_tz()?);
    tx.execute(
        "UPDATE tasks SET escalation_level=?1, updated_at=?2 WHERE id=?3",
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

fn set_status(db: &Db, task_uuid: &str, status: &str) -> Result<()> {
    let mut conn = db.get()?;
    let tx = conn.transaction()?;
    let now = crate::dates::format_iso(&crate::dates::now_in_operator_tz()?);
    let v2 = match status {
        "blocked" => "blocked",
        "pending" => "todo",
        "delayed" => "snoozed",
        other => other,
    };
    tx.execute(
        "UPDATE tasks SET status=?1, status_v2=?4, updated_at=?2 WHERE id=?3",
        params![status, now, task_uuid, v2],
    )?;
    tx.execute(
        "INSERT INTO interactions (task_id, action, ts, details)
         VALUES (?1, 'status_change', ?2, ?3)",
        params![task_uuid, now, format!("status → {}", status)],
    )?;
    crate::event_log::record_in_conn(
        &tx,
        &format!("local:{}", uuid::Uuid::new_v4()),
        Some(task_uuid),
        "task.updated",
        &serde_json::json!({ "task_uuid": task_uuid, "status": status }),
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

/// Stamp `last_reminded = now` and `next_reminder = now + 4h` so the next
/// run-check skips this task during its cool-down window.
fn stamp_reminder(db: &Db, task_uuid: &str, now: &Zoned) -> Result<()> {
    let now_iso = crate::dates::format_iso(now);
    let next = now
        .checked_add(jiff::Span::new().hours(MIN_HOURS_BETWEEN_TASK_REMINDERS))
        .map_err(|e| Error::Other(format!("next_reminder math: {}", e)))?;
    let next_iso = crate::dates::format_iso(&next);
    let conn = db.get()?;
    conn.execute(
        "UPDATE tasks SET last_reminded=?1, next_reminder=?2 WHERE id=?3",
        params![now_iso, next_iso, task_uuid],
    )?;
    Ok(())
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
/// attempted but failed (network / non-2xx), `Err` = misconfiguration.
/// Config-missing and dry-run short-circuits are handled by the CALLER
/// ([`run_check_at`]) — implementations may assume real config and a live
/// send is wanted.
pub trait Dispatch: Send + Sync {
    fn send_telegram(
        &self,
        cfg: &DispatchCfg,
        text: &str,
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
    let now_iso_utc = crate::dates::format_iso(&now_utc);
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
    let telegram_remaining = (DAILY_BUDGET_MAX - budget_used).max(0);
    if telegram_remaining == 0 {
        info!(
            target: "ptask::accountability",
            used = budget_used, max = DAILY_BUDGET_MAX, "telegram budget exhausted"
        );
    }

    let eligible = fetch_eligible(db, &now_iso_utc)?;
    report.eligible = eligible.len() as i64;
    let mut sent_telegrams = 0i64;
    let mut telegram_consecutive_failures = 0i64;

    for mut task in eligible {
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

        let channels = channels_for(level_after_transition);
        let telegram_only = channels == ["telegram"];
        if telegram_only && sent_telegrams >= telegram_remaining {
            continue;
        }

        if level_after_transition != task.escalation_level {
            if !cfg.dry_run {
                set_escalation_level(db, &task.id, level_after_transition)?;
            }
            task.escalation_level = level_after_transition;
            info!(
                target: "ptask::accountability",
                task_uuid = %task.id,
                level = level_after_transition,
                dry_run = cfg.dry_run,
                title = %task.title.chars().take(60).collect::<String>(),
                "escalated"
            );
        }
        let level = task.escalation_level;

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
        let mut dispatched = DispatchedFor {
            task_uuid: task.id.clone(),
            level,
            message: message.clone(),
            ..Default::default()
        };

        for channel in channels {
            let ok = match *channel {
                "telegram" => {
                    if sent_telegrams >= telegram_remaining
                        || telegram_consecutive_failures >= TELEGRAM_CIRCUIT_BREAK
                        || !cfg.telegram_configured()
                    {
                        false
                    } else {
                        let prefixed = format!("<b>Task #{}:</b> {}", level, message);
                        let r = if cfg.dry_run {
                            true
                        } else {
                            dispatch.send_telegram(cfg, &prefixed).await?
                        };
                        if r {
                            telegram_consecutive_failures = 0;
                            dispatched.telegram_sent = true;
                            sent_telegrams += 1;
                            if !cfg.dry_run {
                                increment_daily_budget(db, &date_utc)?;
                            }
                        } else {
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
                    if !cfg.email_configured() {
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
                            dispatch.send_email(cfg, &subject, &message).await?
                        };
                        if r {
                            dispatched.email_sent = true;
                        } else {
                            report.send_failures += 1;
                        }
                        r
                    }
                }
                _ => false,
            };
            if ok && !cfg.dry_run {
                log_notification(db, &task.id, channel, level, &message)?;
            }
        }
        if level == 5 && !cfg.dry_run {
            set_status(db, &task.id, "blocked")?;
        }
        if dispatched.telegram_sent || dispatched.email_sent {
            if !cfg.dry_run {
                stamp_reminder(db, &task.id, &now_utc)?;
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
    fn quiet_hours_match_22_to_08_utc() {
        let mk = |h: i8| {
            jiff::civil::date(2026, 5, 13)
                .at(h, 0, 0, 0)
                .to_zoned(jiff::tz::TimeZone::UTC)
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
    fn should_advance_transitions_match_spec() {
        let now = Zoned::now().with_time_zone(jiff::tz::TimeZone::UTC);
        let mk = |level: i64, dismissal: i64, age_days: i64, last_offset_secs: Option<i64>| {
            EligibleTask {
                id: "x".into(),
                title: "t".into(),
                created_at: crate::dates::format_iso(
                    &now.checked_sub(jiff::Span::new().days(age_days)).unwrap(),
                ),
                last_reminded: last_offset_secs.map(|s| {
                    crate::dates::format_iso(
                        &now.checked_sub(jiff::Span::new().seconds(s)).unwrap(),
                    )
                }),
                dismissal_count: dismissal,
                escalation_level: level,
            }
        };
        // 0 → 1 at age 2d.
        assert!(should_advance(&mk(0, 0, 2, None), 2, &now));
        assert!(!should_advance(&mk(0, 0, 1, None), 1, &now));
        // 1 → 2 after first dismissal.
        assert!(should_advance(&mk(1, 1, 5, None), 5, &now));
        assert!(!should_advance(&mk(1, 0, 5, None), 5, &now));
        // 2 → 3 after third dismissal.
        assert!(should_advance(&mk(2, 3, 5, None), 5, &now));
        assert!(!should_advance(&mk(2, 2, 5, None), 5, &now));
        // 3 → 4 after 48h since last reminder.
        assert!(should_advance(&mk(3, 3, 5, Some(48 * 3600)), 5, &now));
        assert!(!should_advance(&mk(3, 3, 5, Some(40 * 3600)), 5, &now));
        // 4 → 5 after 7 days since last reminder.
        assert!(should_advance(&mk(4, 3, 9, Some(7 * 86_400)), 9, &now));
        assert!(!should_advance(&mk(4, 3, 9, Some(5 * 86_400)), 9, &now));
        // Level 5 never advances.
        assert!(!should_advance(&mk(5, 99, 99, Some(99 * 86_400)), 99, &now));
    }

    #[test]
    fn fallback_message_per_level_is_short_and_concrete() {
        let t = EligibleTask {
            id: "x".into(),
            title: "Renew SSL".into(),
            created_at: "2026-05-01T00:00:00+00:00".into(),
            last_reminded: None,
            dismissal_count: 2,
            escalation_level: 0,
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
        async fn send_telegram(&self, _cfg: &DispatchCfg, _text: &str) -> Result<bool> {
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
        async fn send_telegram(&self, _cfg: &DispatchCfg, _text: &str) -> Result<bool> {
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
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET escalation_level=3 WHERE id=?1",
                [&task_uuid],
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
