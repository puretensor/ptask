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
use crate::error::{Error, Result};
use jiff::{Timestamp, Zoned};
use rusqlite::OptionalExtension;
use rusqlite::params;
use tracing::{info, warn};

pub const DAILY_BUDGET_MAX: i64 = 3;
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

fn parse_iso_to_utc(s: &str) -> Option<Zoned> {
    s.parse::<Timestamp>()
        .ok()
        .map(|t| t.to_zoned(jiff::tz::TimeZone::UTC))
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

/// Update tasks(escalation_level=N) and log to interactions.
fn set_escalation_level(db: &Db, task_uuid: &str, level: i64) -> Result<()> {
    let conn = db.get()?;
    let now = crate::dates::format_iso(&crate::dates::now_in_operator_tz()?);
    conn.execute(
        "UPDATE tasks SET escalation_level=?1, updated_at=?2 WHERE id=?3",
        params![level, now, task_uuid],
    )?;
    conn.execute(
        "INSERT INTO interactions (task_id, action, ts, details)
         VALUES (?1, 'escalation', ?2, ?3)",
        params![task_uuid, now, format!("escalation_level → {}", level)],
    )?;
    Ok(())
}

fn set_status(db: &Db, task_uuid: &str, status: &str) -> Result<()> {
    let conn = db.get()?;
    let now = crate::dates::format_iso(&crate::dates::now_in_operator_tz()?);
    conn.execute(
        "UPDATE tasks SET status=?1, updated_at=?2 WHERE id=?3",
        params![status, now, task_uuid],
    )?;
    conn.execute(
        "INSERT INTO interactions (task_id, action, ts, details)
         VALUES (?1, 'status_change', ?2, ?3)",
        params![task_uuid, now, format!("status → {}", status)],
    )?;
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

/// Configuration the dispatcher needs. Reads env in `from_env`; tests build
/// it directly to avoid touching shared process state.
#[derive(Debug, Clone, Default)]
pub struct DispatchCfg {
    pub telegram_token: Option<String>,
    pub telegram_chat_id: Option<i64>,
    pub smtp_host: Option<String>,
    pub smtp_port: u16,
    pub smtp_user: Option<String>,
    pub smtp_pass: Option<String>,
    pub notify_email: Option<String>,
    /// Always CC'd on every outbound email per CLAUDE.md.
    pub cc_email: Option<String>,
    pub hal_nudge_url: Option<String>,
    /// Suppress side-effecting sends. Tests set this; production never does.
    pub dry_run: bool,
}

impl DispatchCfg {
    pub fn from_env() -> Self {
        let smtp_port = std::env::var("PTASK_SMTP_PORT")
            .or_else(|_| std::env::var("SMTP_PORT"))
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(587);
        DispatchCfg {
            telegram_token: env_first(&["PTASK_TELEGRAM_BOT_TOKEN", "TELEGRAM_BOT_TOKEN"]),
            telegram_chat_id: env_first(&[
                "PTASK_ACCOUNTABILITY_CHAT_ID",
                "PTASK_TELEGRAM_DIGEST_CHATS",
                "TELEGRAM_CHAT_ID",
            ])
            .and_then(|s| s.split(',').next()?.trim().parse().ok()),
            smtp_host: env_first(&["PTASK_SMTP_HOST", "SMTP_HOST"]),
            smtp_port,
            smtp_user: env_first(&["PTASK_SMTP_USER", "SMTP_USER"]),
            smtp_pass: env_first(&["PTASK_SMTP_PASS", "SMTP_PASS"]),
            notify_email: env_first(&["PTASK_NOTIFY_EMAIL", "NOTIFY_EMAIL"]),
            cc_email: env_first(&["PTASK_NOTIFY_CC", "PTASK_OPS_EMAIL"])
                .or_else(|| Some("ops@puretensor.ai".to_string())),
            hal_nudge_url: std::env::var("PTASK_HAL_NUDGE_URL").ok(),
            dry_run: std::env::var("PTASK_ACCOUNTABILITY_DRY_RUN")
                .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
        }
    }
}

fn env_first(names: &[&str]) -> Option<String> {
    for n in names {
        if let Ok(v) = std::env::var(n)
            && !v.trim().is_empty()
        {
            return Some(v);
        }
    }
    None
}

/// Send `text` via the Telegram Bot API. `Ok(true)` on HTTP 2xx, `Ok(false)`
/// on network failure (logged), `Err` only for misconfiguration. `dry_run`
/// short-circuits to `Ok(true)` without touching the network.
pub async fn send_telegram(cfg: &DispatchCfg, text: &str) -> Result<bool> {
    let (Some(token), Some(chat)) = (cfg.telegram_token.as_deref(), cfg.telegram_chat_id) else {
        return Ok(false);
    };
    if cfg.dry_run {
        return Ok(true);
    }
    let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
    let body = serde_json::json!({"chat_id": chat, "text": text, "parse_mode": "HTML"});
    let client = reqwest::Client::new();
    match client.post(url).json(&body).send().await {
        Ok(r) if r.status().is_success() => Ok(true),
        Ok(r) => {
            warn!(target: "ptask::accountability", status = %r.status(), "telegram send failed");
            Ok(false)
        }
        Err(e) => {
            warn!(target: "ptask::accountability", error = %e, "telegram send error");
            Ok(false)
        }
    }
}

/// Send a single email via SMTP. CC is mandatory (CLAUDE.md). Returns
/// `Ok(true)` on send, `Ok(false)` on missing config / network failure.
pub async fn send_email(cfg: &DispatchCfg, subject: &str, body: &str) -> Result<bool> {
    let (Some(host), Some(user), Some(pass), Some(to)) = (
        cfg.smtp_host.as_deref(),
        cfg.smtp_user.as_deref(),
        cfg.smtp_pass.as_deref(),
        cfg.notify_email.as_deref(),
    ) else {
        return Ok(false);
    };
    if cfg.dry_run {
        return Ok(true);
    }
    use lettre::message::Mailbox;
    use lettre::transport::smtp::AsyncSmtpTransport;
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{AsyncTransport, Message, Tokio1Executor};

    let from: Mailbox = format!("HAL <{}>", user)
        .parse()
        .map_err(|e| Error::Other(format!("invalid SMTP_USER address {:?}: {}", user, e)))?;
    let to: Mailbox = to
        .parse()
        .map_err(|e| Error::Other(format!("invalid NOTIFY_EMAIL {:?}: {}", to, e)))?;
    let mut builder = Message::builder().from(from).to(to).subject(subject);
    if let Some(cc) = cfg.cc_email.as_deref() {
        let cc: Mailbox = cc
            .parse()
            .map_err(|e| Error::Other(format!("invalid CC {:?}: {}", cc, e)))?;
        builder = builder.cc(cc);
    }
    let email = builder
        .body(body.to_string())
        .map_err(|e| Error::Other(format!("build email: {}", e)))?;
    let creds = Credentials::new(user.to_string(), pass.to_string());
    let mailer = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
        .map_err(|e| Error::Other(format!("smtp transport: {}", e)))?
        .port(cfg.smtp_port)
        .credentials(creds)
        .build();
    match mailer.send(email).await {
        Ok(_) => Ok(true),
        Err(e) => {
            warn!(target: "ptask::accountability", error = %e, "email send failed");
            Ok(false)
        }
    }
}

/// Ask HAL to compose the message body. Falls back silently if `hal_nudge_url`
/// is unset or the call fails.
async fn maybe_compose_via_hal(
    cfg: &DispatchCfg,
    task: &EligibleTask,
    level: i64,
    age_days: i64,
) -> Option<String> {
    let url = cfg.hal_nudge_url.as_deref()?;
    if cfg.dry_run {
        return None;
    }
    let body = serde_json::json!({
        "task_uuid": task.id,
        "title": task.title,
        "level": level,
        "age_days": age_days,
        "dismissal_count": task.dismissal_count,
    });
    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    v.get("message")
        .and_then(|m| m.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Run one accountability cycle. Mirrors `engine.run_check()`.
pub async fn run_check(db: &Db, cfg: &DispatchCfg) -> Result<RunReport> {
    let now = crate::dates::now_in_operator_tz()?;
    run_check_at(db, cfg, &now).await
}

/// Same as [`run_check`] but with an injected `now`. Tests use this to
/// pin the wall-clock anchor outside of the quiet-hours window.
pub async fn run_check_at(db: &Db, cfg: &DispatchCfg, now: &Zoned) -> Result<RunReport> {
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
    let remaining = (DAILY_BUDGET_MAX - budget_used).max(0);
    if remaining == 0 {
        info!(
            target: "ptask::accountability",
            used = budget_used, max = DAILY_BUDGET_MAX, "daily budget exhausted"
        );
        return Ok(report);
    }

    let eligible = fetch_eligible(db, &now_iso_utc)?;
    report.eligible = eligible.len() as i64;
    let mut sent_telegrams = 0i64;

    for mut task in eligible {
        if sent_telegrams >= remaining {
            break;
        }
        let age_days = task_age_days(&task, &now_utc);

        if should_advance(&task, age_days, &now_utc) {
            let new_level = (task.escalation_level + 1).min(5);
            set_escalation_level(db, &task.id, new_level)?;
            task.escalation_level = new_level;
            info!(
                target: "ptask::accountability",
                task_uuid = %task.id,
                level = new_level,
                title = %task.title.chars().take(60).collect::<String>(),
                "escalated"
            );
        }
        let level = task.escalation_level;
        if level == 0 {
            continue;
        }
        if !can_remind(&task, &now_utc) {
            continue;
        }

        let message = match maybe_compose_via_hal(cfg, &task, level, age_days).await {
            Some(m) => m,
            None => fallback_message(&task, level, age_days),
        };
        let mut dispatched = DispatchedFor {
            task_uuid: task.id.clone(),
            level,
            message: message.clone(),
            ..Default::default()
        };

        for channel in channels_for(level) {
            let ok = match *channel {
                "telegram" => {
                    let prefixed = format!("<b>Task #{}:</b> {}", level, message);
                    let r = send_telegram(cfg, &prefixed).await?;
                    if r {
                        dispatched.telegram_sent = true;
                        sent_telegrams += 1;
                        increment_daily_budget(db, &date_utc)?;
                    }
                    r
                }
                "email" => {
                    let subject = format!(
                        "[PureTensor] Task escalated (level {}): {}",
                        level,
                        task.title.chars().take(60).collect::<String>()
                    );
                    let r = send_email(cfg, &subject, &message).await?;
                    if r {
                        dispatched.email_sent = true;
                    }
                    r
                }
                _ => false,
            };
            if ok {
                log_notification(db, &task.id, channel, level, &message)?;
            }
        }
        if level == 5 {
            set_status(db, &task.id, "blocked")?;
        }
        stamp_reminder(db, &task.id, &now_utc)?;
        report.dispatched.push(dispatched);
    }
    report.budget_used_after = get_daily_budget(db, &date_utc)?;
    info!(
        target: "ptask::accountability",
        sent_telegrams,
        emails = report.dispatched.iter().filter(|d| d.email_sent).count(),
        eligible = report.eligible,
        "run_check complete"
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let t = create_with_extensions(db, NewTask::minimal(title), Extensions::default()).unwrap();
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

    #[tokio::test]
    async fn run_check_advances_age_two_task_then_sends_dry_run() {
        let (_dir, db) = fresh_db();
        let anchor = noon_utc();
        // Two-day-old task — should advance to level 1 and dispatch via telegram.
        let task_uuid = aged_task_before(&db, "Renew SSL certs", 2, &anchor);
        let cfg = DispatchCfg {
            telegram_token: Some("test".into()),
            telegram_chat_id: Some(1),
            dry_run: true,
            ..Default::default()
        };
        let report = run_check_at(&db, &cfg, &anchor).await.unwrap();
        assert_eq!(report.eligible, 1);
        assert_eq!(report.dispatched.len(), 1);
        assert!(report.dispatched[0].telegram_sent);
        assert_eq!(report.dispatched[0].level, 1);
        // Budget consumed once.
        assert_eq!(report.budget_used_after, 1);
        // Cooldown stamped + escalation row in interactions + notification row.
        db.with_conn(|c| {
            let last: Option<String> = c
                .query_row(
                    "SELECT last_reminded FROM tasks WHERE id=?1",
                    [&task_uuid],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(last.is_some());
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM notifications WHERE task_id=?1 AND channel='telegram'",
                    [&task_uuid],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1);
            let escalations: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM interactions WHERE task_id=?1 AND action='escalation'",
                    [&task_uuid],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(escalations, 1);
            Ok(())
        })
        .unwrap();
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
        let report = run_check_at(&db, &cfg, &anchor).await.unwrap();
        assert_eq!(report.dispatched.len(), 0);
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
        let report = run_check_at(&db, &cfg, &quiet).await.unwrap();
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
