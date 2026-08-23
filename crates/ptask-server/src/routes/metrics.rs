//! GET /metrics — Prometheus text-format gauges.
//!
//! Hand-rolled rather than a crate dep — the set is small and each value
//! comes from a quick SQL query, so a 30-line writer is the right size.
//! Counters (request counts, webhook sends) want in-process state and
//! will land alongside a `tracing-prometheus` adapter in a later phase.

use crate::AppState;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::IntoResponse;
use axum::routing::get;
use ptask_core::Db;
use std::fmt::Write;

pub fn router() -> Router<AppState> {
    Router::new().route("/metrics", get(metrics))
}

async fn metrics(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    // Same enforce-if-configured gate as the write routes: /metrics leaks
    // task/store counts. When PTASK_API_TOKEN is unset this returns None and
    // the scrape is served (back-compat); when set, a missing/wrong token 401s.
    if let Some(resp) = crate::auth::require_read_token(&state.db, &state.auth, &headers) {
        return resp;
    }
    let body = render(&state.db).unwrap_or_else(|e| {
        format!(
            "# pt_metrics_render_error: {}\n",
            e.to_string().replace('\n', " ")
        )
    });
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

fn render(db: &Db) -> ptask_core::Result<String> {
    let mut out = String::new();
    // --- tasks by status ---
    writeln!(out, "# HELP pt_tasks_total Number of tasks by status.").ok();
    writeln!(out, "# TYPE pt_tasks_total gauge").ok();
    db.with_conn(|c| {
        let mut stmt =
            c.prepare("SELECT status, COUNT(*) FROM tasks GROUP BY status ORDER BY status")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (status, count) = row?;
            let _ = writeln!(
                out,
                "pt_tasks_total{{status=\"{}\"}} {}",
                escape(&status),
                count
            );
        }
        Ok(())
    })?;

    // --- tasks by priority ---
    writeln!(
        out,
        "# HELP pt_tasks_priority_total Number of tasks by priority."
    )
    .ok();
    writeln!(out, "# TYPE pt_tasks_priority_total gauge").ok();
    db.with_conn(|c| {
        let mut stmt =
            c.prepare("SELECT priority, COUNT(*) FROM tasks GROUP BY priority ORDER BY priority")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (priority, count) = row?;
            let _ = writeln!(
                out,
                "pt_tasks_priority_total{{priority=\"{}\"}} {}",
                priority, count
            );
        }
        Ok(())
    })?;

    // --- raw_items unprocessed ---
    let unprocessed = ptask_core::raw_items::unprocessed_count(db).unwrap_or(0);
    writeln!(
        out,
        "# HELP pt_raw_items_unprocessed Unprocessed inbox captures."
    )
    .ok();
    writeln!(out, "# TYPE pt_raw_items_unprocessed gauge").ok();
    writeln!(out, "pt_raw_items_unprocessed {}", unprocessed).ok();

    // --- views ---
    let views = ptask_core::views::list(db).unwrap_or_default();
    writeln!(out, "# HELP pt_views_total Saved views count.").ok();
    writeln!(out, "# TYPE pt_views_total gauge").ok();
    writeln!(out, "pt_views_total {}", views.len()).ok();

    // --- event log cursor (sync token) ---
    let cursor = ptask_core::event_log::current_cursor(db).unwrap_or(0);
    writeln!(
        out,
        "# HELP pt_event_log_cursor Highest pt_event_log.id (sync cursor)."
    )
    .ok();
    writeln!(out, "# TYPE pt_event_log_cursor gauge").ok();
    writeln!(out, "pt_event_log_cursor {}", cursor).ok();

    // --- webhook log totals by direction ---
    writeln!(
        out,
        "# HELP pt_webhook_log_total Cumulative rows in pt_webhook_log by direction."
    )
    .ok();
    writeln!(out, "# TYPE pt_webhook_log_total gauge").ok();
    db.with_conn(|c| {
        let mut stmt =
            c.prepare("SELECT direction, COUNT(*) FROM pt_webhook_log GROUP BY direction")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (direction, count) = row?;
            let _ = writeln!(
                out,
                "pt_webhook_log_total{{direction=\"{}\"}} {}",
                escape(&direction),
                count
            );
        }
        Ok(())
    })?;

    // --- recurrence count ---
    let rec_count: i64 = db
        .with_conn(|c| {
            let n: i64 = c.query_row("SELECT COUNT(*) FROM pt_recurrence", [], |r| r.get(0))?;
            Ok(n)
        })
        .unwrap_or(0);
    writeln!(
        out,
        "# HELP pt_recurrence_total Recurring task definitions."
    )
    .ok();
    writeln!(out, "# TYPE pt_recurrence_total gauge").ok();
    writeln!(out, "pt_recurrence_total {}", rec_count).ok();

    // --- distill freshness ---
    // Age of the last SUCCESSFUL distill run (`distill.run` event). The
    // 2026-05 incident produced zero tasks for 7 weeks with no signal; the
    // alert rule fires when this exceeds ~26h (daily timer + slack).
    // -1 = never ran. SQLite's strftime handles the mixed +01:00/UTC
    // offsets these rows carry.
    let distill_age: i64 = db
        .with_conn(|c| {
            let n: Option<i64> = c.query_row(
                "SELECT CAST(strftime('%s','now') AS INTEGER)
                        - CAST(strftime('%s', MAX(ts)) AS INTEGER)
                 FROM pt_event_log WHERE event_type = 'distill.run'",
                [],
                |r| r.get(0),
            )?;
            Ok(n.unwrap_or(-1))
        })
        .unwrap_or(-1);
    writeln!(
        out,
        "# HELP pt_distill_last_success_age_seconds Seconds since the last successful distill run (-1 = never)."
    )
    .ok();
    writeln!(out, "# TYPE pt_distill_last_success_age_seconds gauge").ok();
    writeln!(out, "pt_distill_last_success_age_seconds {}", distill_age).ok();

    let distill_failed: i64 = db
        .with_conn(|c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM pt_event_log WHERE event_type = 'distill.failed'",
                [],
                |r| r.get(0),
            )?;
            Ok(n)
        })
        .unwrap_or(0);
    writeln!(
        out,
        "# HELP pt_distill_failed_total Distill runs recorded as failed."
    )
    .ok();
    writeln!(out, "# TYPE pt_distill_failed_total gauge").ok();
    writeln!(out, "pt_distill_failed_total {}", distill_failed).ok();

    // Current-state health: did the MOST RECENT distill run succeed? Alerts
    // must describe now, not history (operator doctrine 2026-08-23) — the old
    // increase(pt_distill_failed_total[2h]) rule kept a WARNING latched for
    // two hours after full recovery. 1 = latest outcome ok (or never failed),
    // 0 = latest outcome failed. Epoch comparison sidesteps the mixed
    // +01:00/UTC offsets these rows carry.
    let distill_last_ok: i64 = db
        .with_conn(|c| {
            let n: i64 = c.query_row(
                "SELECT CASE
                    WHEN f.t IS NULL THEN 1
                    WHEN s.t IS NULL THEN 0
                    WHEN s.t >= f.t THEN 1
                    ELSE 0 END
                 FROM (SELECT MAX(CAST(strftime('%s', ts) AS INTEGER)) AS t
                       FROM pt_event_log WHERE event_type = 'distill.run') s,
                      (SELECT MAX(CAST(strftime('%s', ts) AS INTEGER)) AS t
                       FROM pt_event_log WHERE event_type = 'distill.failed') f",
                [],
                |r| r.get(0),
            )?;
            Ok(n)
        })
        .unwrap_or(1);
    writeln!(
        out,
        "# HELP pt_distill_last_run_ok Whether the most recent distill run succeeded (1) or failed (0)."
    )
    .ok();
    writeln!(out, "# TYPE pt_distill_last_run_ok gauge").ok();
    writeln!(out, "pt_distill_last_run_ok {}", distill_last_ok).ok();

    // Captures parked out of the distill queue after repeated isolated
    // failures (V013). Before quarantine existed one such row re-served
    // forever and wedged everything behind it with no signal at all; the
    // pipeline now walks past it, so this gauge is what makes it visible.
    // Any non-zero value wants a human eye — nothing clears it automatically.
    let quarantined = ptask_core::raw_items::quarantined_count(db).unwrap_or(0);
    writeln!(
        out,
        "# HELP pt_distill_quarantined_captures Unprocessed raw_items parked after repeated distill failures."
    )
    .ok();
    writeln!(out, "# TYPE pt_distill_quarantined_captures gauge").ok();
    writeln!(out, "pt_distill_quarantined_captures {}", quarantined).ok();

    // --- accountability dispatch freshness (per channel) ---
    // Rows land in `notifications` only on successful sends, so channel age
    // is a liveness signal for the dispatch path (the 401ing bot token sat
    // undetected for 8 weeks because nothing measured this).
    writeln!(
        out,
        "# HELP pt_notifications_last_sent_age_seconds Seconds since the last successful send per channel."
    )
    .ok();
    writeln!(out, "# TYPE pt_notifications_last_sent_age_seconds gauge").ok();
    db.with_conn(|c| {
        let mut stmt = c.prepare(
            "SELECT channel,
                    CAST(strftime('%s','now') AS INTEGER)
                    - CAST(strftime('%s', MAX(sent_at)) AS INTEGER)
             FROM notifications GROUP BY channel",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (channel, age) = row?;
            let _ = writeln!(
                out,
                "pt_notifications_last_sent_age_seconds{{channel=\"{}\"}} {}",
                escape(&channel),
                age
            );
        }
        Ok(())
    })?;

    Ok(out)
}

/// Escape `"` and `\` per Prometheus text format.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}
