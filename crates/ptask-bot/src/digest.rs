//! Morning digest + evening recap content.
//!
//! Morning (07:00 London): today's deadline + overdue tasks, grouped by
//! priority. The operator's "what should I tackle first" snapshot.
//!
//! Evening (18:00 London): what got done today (interactions log), what
//! slipped (overdue still open), blocked tasks. The post-mortem snapshot.

use crate::config::BotConfig;
use crate::telegram::{Bot, ChatId};
use anyhow::Result;
use ptask_core::dates;
use ptask_core::tasks::Task;
use ptask_core::{Db, priority};
use tracing::warn;

/// 07:00 London morning digest.
pub async fn send_morning(bot: Bot, db: Db, cfg: BotConfig) -> Result<()> {
    let text = render_morning(&db)?;
    fanout(&bot, &cfg, &text).await;
    Ok(())
}

/// 18:00 London evening recap.
pub async fn send_evening(bot: Bot, db: Db, cfg: BotConfig) -> Result<()> {
    let text = render_evening(&db)?;
    fanout(&bot, &cfg, &text).await;
    Ok(())
}

fn render_morning(db: &Db) -> Result<String> {
    let now = dates::now_in_operator_tz()?;
    let today = now.date().to_string();
    // (today | overdue) & pending — let the filter DSL do the date math,
    // but push status into SQL so done/cancelled rows cannot crowd out the
    // morning snapshot or suppress the empty "Clear runway" case.
    let expr = ptask_core::filter::parse("today | overdue")?;
    let rows = ptask_core::tasks::list_with_filter(db, Some(&expr), Some("pending"), None, 200)?;
    let mut out = format!("☕ pTask digest {}\n\n", today);
    if rows.is_empty() {
        out.push_str("Nothing due today and nothing overdue. Clear runway.\n");
        return Ok(out);
    }

    let (overdue, due_today): (Vec<&Task>, Vec<&Task>) = rows.iter().partition(|t| {
        t.deadline
            .as_deref()
            .map(|d| d < today.as_str())
            .unwrap_or(false)
    });

    if !overdue.is_empty() {
        out.push_str(&format!("🚨 OVERDUE ({})\n", overdue.len()));
        push_rows(&mut out, &overdue);
        out.push('\n');
    }
    if !due_today.is_empty() {
        out.push_str(&format!("📅 DUE TODAY ({})\n", due_today.len()));
        push_rows(&mut out, &due_today);
    }
    Ok(out)
}

fn render_evening(db: &Db) -> Result<String> {
    let now = dates::now_in_operator_tz()?;
    render_evening_at(db, &now)
}

fn render_evening_at(db: &Db, now: &jiff::Zoned) -> Result<String> {
    let today = now.date().to_string();
    let (day_start_utc, next_day_start_utc) = local_day_utc_bounds(now)?;

    let mut completed = 0i64;
    let mut advanced = 0i64;
    db.with_conn(|c| {
        let n_done: i64 = c.query_row(
            "SELECT COUNT(*) FROM interactions
             WHERE action='status_change' AND details='Completed via Claude Code'
               AND ts >= ?1 AND ts < ?2",
            [&day_start_utc, &next_day_start_utc],
            |r| r.get(0),
        )?;
        let n_adv: i64 = c.query_row(
            "SELECT COUNT(*) FROM interactions
             WHERE action='recurrence_advance'
               AND ts >= ?1 AND ts < ?2",
            [&day_start_utc, &next_day_start_utc],
            |r| r.get(0),
        )?;
        completed = n_done;
        advanced = n_adv;
        Ok(())
    })?;

    // Still-open overdue (operator missed them).
    let overdue_expr = ptask_core::filter::parse("overdue")?;
    let still_overdue =
        ptask_core::tasks::list_with_filter(db, Some(&overdue_expr), None, None, 50)?
            .into_iter()
            .filter(|t| t.status == "pending")
            .collect::<Vec<_>>();

    // Blocked = status='blocked' OR with unmet deps. Cheap version: just
    // status='blocked' for now; DAG-blocked is a v0.5+ refinement.
    let blocked = ptask_core::tasks::list_with_filter(db, None, Some("blocked"), None, 50)?;

    let mut out = format!("🌙 pTask recap {}\n\n", today);
    out.push_str(&format!(
        "✓ Completed today: {}  ({} recurring advanced)\n",
        completed, advanced
    ));
    out.push_str(&format!("⏰ Still overdue: {}\n", still_overdue.len()));
    out.push_str(&format!("⛔ Blocked: {}\n", blocked.len()));

    if !still_overdue.is_empty() {
        out.push_str("\nOverdue tail:\n");
        let refs: Vec<&Task> = still_overdue.iter().collect();
        push_rows(&mut out, &refs);
    }
    if !blocked.is_empty() {
        out.push_str("\nBlocked:\n");
        let refs: Vec<&Task> = blocked.iter().collect();
        push_rows(&mut out, &refs);
    }
    Ok(out)
}

fn local_day_utc_bounds(now: &jiff::Zoned) -> Result<(String, String)> {
    let tz = now.time_zone().clone();
    let start_local = now.date().to_zoned(tz.clone())?;
    let next_start_local = now
        .date()
        .checked_add(jiff::Span::new().days(1))?
        .to_zoned(tz)?;
    Ok((
        dates::format_iso(&start_local.with_time_zone(jiff::tz::TimeZone::UTC)),
        dates::format_iso(&next_start_local.with_time_zone(jiff::tz::TimeZone::UTC)),
    ))
}

fn push_rows(out: &mut String, rows: &[&Task]) {
    for t in rows {
        let pt = t.pt_id.as_deref().unwrap_or("?");
        let label = priority::label(t.priority);
        let due = t.deadline.as_deref().unwrap_or("");
        out.push_str(&format!("  {} [{}] {} {}\n", pt, label, t.title, due));
    }
}

async fn fanout(bot: &Bot, cfg: &BotConfig, text: &str) {
    for &chat_id in &cfg.digest_chats {
        if let Err(e) = bot.send_message(ChatId(chat_id), text).await {
            warn!(
                target: "ptask::bot",
                chat_id, error = %e, "digest send failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptask_core::NewTask;

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
                 );",
            )
            .unwrap();
        }
        (dir, Db::open(&path).unwrap())
    }

    #[test]
    fn morning_digest_with_no_tasks_says_clear_runway() {
        let (_dir, db) = fresh_db();
        let txt = render_morning(&db).unwrap();
        assert!(txt.contains("Clear runway"));
    }

    #[test]
    fn morning_digest_ignores_completed_due_today_for_empty_case() {
        let (_dir, db) = fresh_db();
        let today = dates::now_in_operator_tz().unwrap().date().to_string();
        let mut new = NewTask::minimal("already done today");
        new.deadline = Some(today);
        let task = ptask_core::tasks::create(&db, new).unwrap();
        ptask_core::tasks::mark_done(&db, &task).unwrap();

        let txt = render_morning(&db).unwrap();
        assert!(txt.contains("Clear runway"), "got:\n{txt}");
        assert!(!txt.contains("already done today"), "got:\n{txt}");
    }

    #[test]
    fn evening_recap_reports_zero_when_nothing_today() {
        let (_dir, db) = fresh_db();
        let txt = render_evening(&db).unwrap();
        assert!(txt.contains("Completed today: 0"));
        assert!(txt.contains("Still overdue: 0"));
        assert!(txt.contains("Blocked: 0"));
    }

    #[test]
    fn evening_recap_counts_done_today() {
        let (_dir, db) = fresh_db();
        let t = ptask_core::tasks::create(&db, NewTask::minimal("x")).unwrap();
        ptask_core::tasks::mark_done(&db, &t).unwrap();
        let txt = render_evening(&db).unwrap();
        assert!(txt.contains("Completed today: 1"));
    }

    #[test]
    fn local_day_bounds_are_london_local_in_bst() {
        let tz = jiff::tz::TimeZone::get(dates::OPERATOR_TZ).unwrap();
        let now = jiff::civil::date(2026, 5, 13)
            .at(18, 0, 0, 0)
            .to_zoned(tz)
            .unwrap();
        let (start, end) = local_day_utc_bounds(&now).unwrap();
        assert_eq!(start, "2026-05-12T23:00:00+00:00");
        assert_eq!(end, "2026-05-13T23:00:00+00:00");
    }

    #[test]
    fn evening_recap_counts_london_day_across_bst_offset() {
        let (_dir, db) = fresh_db();
        let task = ptask_core::tasks::create(&db, NewTask::minimal("late done")).unwrap();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO interactions (task_id, action, ts, details)
                 VALUES (?1, 'status_change', '2026-05-12T23:30:00+00:00',
                         'Completed via Claude Code')",
                [&task.id],
            )?;
            Ok(())
        })
        .unwrap();

        let tz = jiff::tz::TimeZone::get(dates::OPERATOR_TZ).unwrap();
        let now = jiff::civil::date(2026, 5, 13)
            .at(18, 0, 0, 0)
            .to_zoned(tz)
            .unwrap();
        let txt = render_evening_at(&db, &now).unwrap();
        assert!(txt.contains("Completed today: 1"), "got:\n{txt}");
    }
}
