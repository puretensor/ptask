//! GET /metrics — Prometheus text-format gauges.
//!
//! Hand-rolled rather than a crate dep — the set is small and each value
//! comes from a quick SQL query, so a 30-line writer is the right size.
//! Counters (request counts, webhook sends) want in-process state and
//! will land alongside a `tracing-prometheus` adapter in a later phase.

use crate::AppState;
use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use ptask_core::Db;
use std::fmt::Write;

pub fn router() -> Router<AppState> {
    Router::new().route("/metrics", get(metrics))
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
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
