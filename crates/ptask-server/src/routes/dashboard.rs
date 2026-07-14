//! Triage Cockpit surface (v2.3.0) — the Python sidecar's API, absorbed.
//!
//! Same paths + wire shapes as `dashboard/server.py` v0.6.0 (the sidecar's
//! unit tests define the contract), plus the triage verbs the cockpit never
//! had: dismiss / snooze / reopen / edit, per-task journal history, and an
//! SSE event stream replacing the 15-second poll. Writes call ptask-core
//! directly (no `pt` subprocess) attributed as `actor=dashboard`.
//!
//! Auth is HTTP Basic (`PTASK_DASH_USER`/`PTASK_DASH_PASS`) because the
//! consumer is a browser — bearer tokens stay on the machine API. If no
//! password is configured the dashboard routes are open (local/dev), the
//! sidecar's exact rule. `/api/voice` proxies to the Python shim, which
//! keeps Whisper STT + field extraction (`PTASK_VOICE_SHIM_URL`).

use crate::AppState;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use base64::Engine;
use ptask_core::event_log::EventCtx;
use serde::Deserialize;
use std::collections::HashMap;
use std::convert::Infallible;

const MAX_AUDIO_BYTES: usize = 12 * 1024 * 1024;

/// Columns exposed to the cockpit. Explicit so a schema change can't leak
/// surprises — byte-for-byte the sidecar's TASK_COLS.
const TASK_COLS: [&str; 19] = [
    "id",
    "title",
    "description",
    "priority",
    "status",
    "created_at",
    "updated_at",
    "deadline",
    "source_type",
    "task_type",
    "priority_score",
    "score_urgency",
    "score_dependency",
    "score_neglect",
    "escalation_level",
    "dismissal_count",
    "last_reminded",
    "cluster_keywords",
    // v2.3 addition (backwards-compatible new key): the detail drawer shows
    // WHY the distiller/HAL created a task.
    "ai_reasoning",
];

/// Flux windows the cockpit can switch between: (label, SQLite datetime
/// modifier). The frontend renders one at a time; the backend returns all so
/// switching is a local re-render, not a refetch.
const FLUX_WINDOWS: [(&str, &str); 5] = [
    ("30m", "-30 minutes"),
    ("1h", "-1 hour"),
    ("6h", "-6 hours"),
    ("24h", "-1 day"),
    ("7d", "-7 days"),
];

const AGE_BUCKETS: [(f64, f64, &str); 5] = [
    (0.0, 7.0, "0-7d"),
    (7.0, 30.0, "1-4w"),
    (30.0, 60.0, "1-2m"),
    (60.0, 90.0, "2-3m"),
    (90.0, 1.0e9, "90d+"),
];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/stats", get(api_stats))
        .route("/api/tasks", get(api_tasks).post(api_create))
        .route("/api/critical", get(api_critical))
        .route("/api/timeline", get(api_timeline))
        .route("/api/heatmap", get(api_heatmap))
        .route("/api/tasks/{id}/done", post(act_done))
        .route("/api/tasks/{id}/priority", post(act_priority))
        .route("/api/tasks/{id}/dismiss", post(act_dismiss))
        .route("/api/tasks/{id}/snooze", post(act_snooze))
        .route("/api/tasks/{id}/reopen", post(act_reopen))
        .route("/api/tasks/{id}/edit", post(act_edit))
        .route("/api/tasks/{id}/events", get(api_events))
        .route("/api/stream", get(api_stream))
        .route(
            "/api/voice",
            post(api_voice).layer(DefaultBodyLimit::max(MAX_AUDIO_BYTES)),
        )
        .route("/index.html", get(serve_index))
        .route("/manifest.webmanifest", get(serve_manifest))
}

// ---------------------------------------------------------------- auth

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Sidecar rule: no configured password = open (local/dev); otherwise
/// require exact Basic credentials.
pub fn authed(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(pass) = state.dash.pass.as_deref() else {
        return true;
    };
    let Some(hdr) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(b64) = hdr.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(b64) else {
        return false;
    };
    let Ok(s) = String::from_utf8(raw) else {
        return false;
    };
    let Some((user, pw)) = s.split_once(':') else {
        return false;
    };
    ct_eq(user.as_bytes(), state.dash.user.as_bytes()) & ct_eq(pw.as_bytes(), pass.as_bytes())
}

/// CSRF guard for the state-changing dashboard routes: browsers attach
/// cached Basic credentials cross-origin, so a present Origin header must
/// match the Host we were addressed as. Header-free callers (curl, same-
/// origin fetches in some browsers) pass.
fn origin_ok(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .is_some_and(|o| o.trim_end_matches('/') == host)
}

fn need_auth() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"PTASK\"")],
        "unauthorized",
    )
        .into_response()
}

fn dash_ctx() -> EventCtx {
    EventCtx::system("dashboard")
}

fn jerr(code: StatusCode, msg: &str) -> Response {
    (code, Json(serde_json::json!({"error": msg}))).into_response()
}

// ------------------------------------------------------------- helpers

/// Days since an ISO timestamp (mixed formats survive), 1-decimal.
fn age_days(created_at: &str) -> Option<f64> {
    let z = ptask_core::dates::parse_iso_to_utc(created_at)?;
    let now = ptask_core::dates::now_in_operator_tz()
        .ok()?
        .with_time_zone(ptask_core::jiff::tz::TimeZone::UTC);
    let secs = now.timestamp().as_second() - z.timestamp().as_second();
    Some(((secs as f64 / 86_400.0) * 10.0).round() / 10.0)
}

/// (deadline_date, days_until) for mixed date / ISO formats — the sidecar's
/// `_parse_deadline`.
fn parse_deadline(s: &str) -> Option<(String, f64)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let z = ptask_core::dates::parse_iso_to_utc(s).or_else(|| {
        // Date-only "YYYY-MM-DD" → midnight UTC. `s.get(..10)` is char-safe:
        // a raw `&s[..10]` panics when byte 10 lands inside a multi-byte scalar
        // (a poisoned deadline row would then crash every dashboard read that
        // touches it). Non-boundary / short input falls back to `s`, which then
        // fails the parse and yields None.
        let prefix = s.get(..10).unwrap_or(s);
        ptask_core::dates::parse_iso_to_utc(&format!("{prefix}T00:00:00+00:00"))
    })?;
    let now = ptask_core::dates::now_in_operator_tz()
        .ok()?
        .with_time_zone(ptask_core::jiff::tz::TimeZone::UTC);
    let days = (z.timestamp().as_second() - now.timestamp().as_second()) as f64 / 86_400.0;
    Some((z.date().to_string(), (days * 10.0).round() / 10.0))
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// One task row → the cockpit's task dict (TASK_COLS + pt_id + derived).
fn row_to_task(r: &rusqlite::Row) -> rusqlite::Result<serde_json::Value> {
    let mut m = serde_json::Map::new();
    for (i, col) in TASK_COLS.iter().enumerate() {
        let v: rusqlite::types::Value = r.get(i)?;
        m.insert(
            (*col).to_string(),
            match v {
                rusqlite::types::Value::Null => serde_json::Value::Null,
                rusqlite::types::Value::Integer(n) => serde_json::json!(n),
                rusqlite::types::Value::Real(f) => serde_json::json!(f),
                rusqlite::types::Value::Text(s) => serde_json::json!(s),
                rusqlite::types::Value::Blob(_) => serde_json::Value::Null,
            },
        );
    }
    let pt_id: Option<String> = r.get(TASK_COLS.len())?;
    m.insert("pt_id".into(), serde_json::json!(pt_id));
    Ok(serde_json::Value::Object(m))
}

fn finish_task(mut t: serde_json::Value) -> serde_json::Value {
    let created = t["created_at"].as_str().unwrap_or("").to_string();
    t["age_days"] = match age_days(&created) {
        Some(a) => serde_json::json!(a),
        None => serde_json::Value::Null,
    };
    let dl = t["deadline"].as_str().unwrap_or("").to_string();
    match parse_deadline(&dl) {
        Some((d, days)) => {
            t["deadline_date"] = serde_json::json!(d);
            t["days_until"] = serde_json::json!(days);
        }
        None => {
            t["deadline_date"] = serde_json::Value::Null;
            t["days_until"] = serde_json::Value::Null;
        }
    }
    let ps = t["priority_score"].as_f64().unwrap_or(0.0);
    t["priority_score"] = serde_json::json!(round4(ps));
    t
}

fn q_tasks(
    state: &AppState,
    status: &str,
    limit: i64,
) -> ptask_core::Result<Vec<serde_json::Value>> {
    let cols = TASK_COLS
        .iter()
        .map(|c| format!("t.{}", c))
        .collect::<Vec<_>>()
        .join(",");
    let base = format!(
        "SELECT {cols}, t.pt_id FROM tasks t {where_clause} \
         ORDER BY t.priority_score DESC, t.priority DESC LIMIT ?1",
        where_clause = if status == "all" {
            ""
        } else {
            "WHERE t.status = ?2"
        },
    );
    let conn = state.db.get()?;
    let mut stmt = conn.prepare(&base)?;
    let rows: Vec<serde_json::Value> = if status == "all" {
        stmt.query_map(rusqlite::params![limit], row_to_task)?
            .collect::<std::result::Result<_, _>>()?
    } else {
        stmt.query_map(rusqlite::params![limit, status], row_to_task)?
            .collect::<std::result::Result<_, _>>()?
    };
    Ok(rows.into_iter().map(finish_task).collect())
}

// ----------------------------------------------------------- GET reads

#[derive(Deserialize)]
struct TasksQ {
    status: Option<String>,
    limit: Option<i64>,
}

async fn api_tasks(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TasksQ>,
) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    let status = q.status.unwrap_or_else(|| "pending".into());
    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    match q_tasks(&state, &status, limit) {
        Ok(tasks) => Json(serde_json::json!({"tasks": tasks})).into_response(),
        Err(e) => jerr(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn api_critical(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TasksQ>,
) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    let limit = q.limit.unwrap_or(12).clamp(1, 100);
    match q_tasks(&state, "pending", limit) {
        Ok(tasks) => Json(serde_json::json!({"tasks": tasks})).into_response(),
        Err(e) => jerr(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn api_stats(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    let out = state.db.with_conn(|c| {
        let mut by_pri = serde_json::Map::new();
        let mut stmt = c.prepare(
            "SELECT priority, count(*) FROM tasks WHERE status='pending' GROUP BY priority",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (p, n) = row?;
            by_pri.insert(p.to_string(), serde_json::json!(n));
        }
        let mut by_status = serde_json::Map::new();
        let mut stmt = c.prepare("SELECT status, count(*) FROM tasks GROUP BY status")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (s, n) = row?;
            by_status.insert(s, serde_json::json!(n));
        }
        let mut by_type = serde_json::Map::new();
        let mut stmt = c.prepare(
            "SELECT COALESCE(task_type,'unknown'), count(*) FROM tasks \
             WHERE status='pending' GROUP BY task_type",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (t, n) = row?;
            by_type.insert(t, serde_json::json!(n));
        }
        let mut thru = Vec::new();
        let mut stmt = c.prepare(
            "SELECT substr(updated_at,1,10) d, count(*) n FROM tasks \
             WHERE status='done' AND updated_at >= date('now','-21 days') \
             GROUP BY d ORDER BY d",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (day, n) = row?;
            thru.push(serde_json::json!({"day": day, "n": n}));
        }
        // Flux: tasks added vs completed, computed across several selectable
        // windows so the cockpit can switch range client-side without a
        // refetch. The added side is split by INTENT, not by which process ran
        // the INSERT: `robot` = autonomously generated with no human ask (the
        // distiller, puresentinel incident capture, subtask auto-promotion,
        // the specola worker — see ROBOT_SOURCES); `human` = everything else,
        // i.e. the operator typed/dictated it (manual/voice_memo/telegram) OR
        // Claude Code/HAL created it on the operator's request (claude_code/
        // mcp/remote-cli). Unknown sources default to human (operator-driven
        // is the common case). `datetime()` normalises mixed T/space ISO forms.
        let mut flux_by_window = serde_json::Map::new();
        for (label, modifier) in FLUX_WINDOWS {
            let mut added_human = 0i64;
            let mut added_robot = 0i64;
            let mut stmt = c.prepare(
                "SELECT CASE WHEN source_type IN \
                          ('distilled','incident','subtask_promotion','specola') \
                        THEN 'robot' ELSE 'human' END o, count(*) \
                 FROM tasks WHERE datetime(created_at) >= datetime('now', ?1) \
                 GROUP BY o",
            )?;
            for row in
                stmt.query_map([modifier], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            {
                let (o, n) = row?;
                if o == "robot" {
                    added_robot = n;
                } else {
                    added_human = n;
                }
            }
            let done: i64 = c.query_row(
                "SELECT count(*) FROM tasks WHERE status='done' \
                 AND datetime(updated_at) >= datetime('now', ?1)",
                [modifier],
                |r| r.get(0),
            )?;
            flux_by_window.insert(
                label.to_string(),
                serde_json::json!({
                    "added": added_human + added_robot,
                    "added_human": added_human,
                    "added_robot": added_robot,
                    "done": done,
                }),
            );
        }
        let pending: i64 = by_pri.values().filter_map(|v| v.as_i64()).sum();
        let mut due_soon = 0i64;
        let mut overdue = 0i64;
        let mut stmt = c.prepare(
            "SELECT deadline FROM tasks WHERE status='pending' \
             AND deadline IS NOT NULL AND deadline != ''",
        )?;
        for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
            if let Some((_, days)) = parse_deadline(&row?) {
                if days < 0.0 {
                    overdue += 1;
                } else if days <= 7.0 {
                    due_soon += 1;
                }
            }
        }
        Ok(serde_json::json!({
            "generated_at": ptask_core::dates::format_iso(
                &ptask_core::dates::now_in_operator_tz()
                    .map_err(|e| ptask_core::Error::Other(e.to_string()))?
                    .with_time_zone(ptask_core::jiff::tz::TimeZone::UTC)),
            "pending_total": pending,
            "by_priority": by_pri,
            "by_status": by_status,
            "by_type": by_type,
            "throughput": thru,
            "due_within_7d": due_soon,
            "overdue": overdue,
            "flux": {
                "windows": FLUX_WINDOWS.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
                "by_window": flux_by_window,
            },
            "version": ptask_core::VERSION,
        }))
    });
    match out {
        Ok(v) => Json(v).into_response(),
        Err(e) => jerr(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn api_timeline(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    match q_tasks(&state, "pending", 2000) {
        Ok(tasks) => {
            let mut items: Vec<serde_json::Value> = tasks
                .into_iter()
                .filter(|t| t["deadline_date"].is_string())
                .map(|t| {
                    serde_json::json!({
                        "id": t["id"], "title": t["title"], "priority": t["priority"],
                        "priority_score": t["priority_score"],
                        "deadline_date": t["deadline_date"], "days_until": t["days_until"],
                    })
                })
                .collect();
            items.sort_by(|a, b| {
                a["deadline_date"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["deadline_date"].as_str().unwrap_or(""))
            });
            Json(serde_json::json!({"items": items})).into_response()
        }
        Err(e) => jerr(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn api_heatmap(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    match q_tasks(&state, "pending", 5000) {
        Ok(tasks) => {
            let mut grid: HashMap<i64, HashMap<&str, i64>> = HashMap::new();
            for p in 1..=5 {
                grid.insert(p, AGE_BUCKETS.iter().map(|b| (b.2, 0)).collect());
            }
            for t in &tasks {
                let (Some(p), Some(age)) = (t["priority"].as_i64(), t["age_days"].as_f64()) else {
                    continue;
                };
                if let Some(cells) = grid.get_mut(&p) {
                    for (lo, hi, label) in AGE_BUCKETS {
                        if age >= lo && age < hi {
                            *cells.get_mut(label).unwrap() += 1;
                            break;
                        }
                    }
                }
            }
            let rows: Vec<serde_json::Value> = (1..=5)
                .rev()
                .map(|p| {
                    let cells = &grid[&p];
                    let mut m = serde_json::Map::new();
                    for b in AGE_BUCKETS {
                        m.insert(b.2.to_string(), serde_json::json!(cells[b.2]));
                    }
                    serde_json::json!({"priority": p, "cells": m})
                })
                .collect();
            Json(serde_json::json!({
                "buckets": AGE_BUCKETS.iter().map(|b| b.2).collect::<Vec<_>>(),
                "rows": rows,
            }))
            .into_response()
        }
        Err(e) => jerr(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn api_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    let task = match ptask_core::tasks::resolve_for_lookup(&state.db, &id, true) {
        Ok(t) => t,
        Err(_) => return jerr(StatusCode::NOT_FOUND, "task not found"),
    };
    match ptask_core::event_log::history_for_task(&state.db, &task.id, 200) {
        Ok(events) => {
            let rows: Vec<serde_json::Value> = events
                .into_iter()
                .map(|e| {
                    serde_json::json!({
                        "ts": e.ts, "event_type": e.event_type,
                        "actor": e.actor, "payload": e.payload,
                    })
                })
                .collect();
            Json(serde_json::json!({"pt_id": task.pt_id, "events": rows})).into_response()
        }
        Err(e) => jerr(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// -------------------------------------------------------------- writes

#[allow(clippy::result_large_err)]
fn resolve_active(state: &AppState, id: &str) -> Result<ptask_core::tasks::Task, Response> {
    ptask_core::tasks::resolve_for_lookup(&state.db, id, true)
        .map_err(|_| jerr(StatusCode::NOT_FOUND, "task not found"))
}

fn rescore(state: &AppState) {
    if let Err(e) = ptask_core::scoring::run_once(&state.db, false) {
        tracing::warn!(target: "ptask::dashboard", error = %e, "post-mutation rescore failed");
    }
}

fn ok_json(pt_id: Option<&str>, message: &str) -> Response {
    (
        StatusCode::OK,
        Json(serde_json::json!({"ok": true, "pt_id": pt_id, "message": message})),
    )
        .into_response()
}

async fn act_done(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    if !origin_ok(&headers) {
        return jerr(StatusCode::FORBIDDEN, "cross-origin write rejected");
    }
    let task = match resolve_active(&state, &id) {
        Ok(t) => t,
        Err(r) => return r,
    };
    match ptask_core::tasks::mark_done(&state.db, &task, &dash_ctx()) {
        Ok(_) => {
            rescore(&state);
            ok_json(task.pt_id.as_deref(), "done")
        }
        Err(e) => jerr(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()),
    }
}

async fn act_dismiss(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    if !origin_ok(&headers) {
        return jerr(StatusCode::FORBIDDEN, "cross-origin write rejected");
    }
    let task = match resolve_active(&state, &id) {
        Ok(t) => t,
        Err(r) => return r,
    };
    match ptask_core::tasks::dismiss(&state.db, &task.id, &dash_ctx()) {
        Ok(()) => {
            rescore(&state);
            ok_json(task.pt_id.as_deref(), "dismissed")
        }
        Err(e) => jerr(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()),
    }
}

async fn act_reopen(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    if !origin_ok(&headers) {
        return jerr(StatusCode::FORBIDDEN, "cross-origin write rejected");
    }
    let task = match resolve_active(&state, &id) {
        Ok(t) => t,
        Err(r) => return r,
    };
    match ptask_core::tasks::reopen(&state.db, &task.id, &dash_ctx()) {
        Ok(()) => {
            rescore(&state);
            ok_json(task.pt_id.as_deref(), "reopened")
        }
        Err(e) => jerr(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()),
    }
}

#[derive(Deserialize)]
struct PriorityBody {
    level: i64,
}

async fn act_priority(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<PriorityBody>,
) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    if !origin_ok(&headers) {
        return jerr(StatusCode::FORBIDDEN, "cross-origin write rejected");
    }
    if !(1..=5).contains(&body.level) {
        return jerr(StatusCode::BAD_REQUEST, "level must be an integer 1..5");
    }
    let task = match resolve_active(&state, &id) {
        Ok(t) => t,
        Err(r) => return r,
    };
    match ptask_core::tasks::update_priority(&state.db, &task.id, body.level, &dash_ctx()) {
        Ok(()) => {
            rescore(&state);
            ok_json(task.pt_id.as_deref(), "priority updated")
        }
        Err(e) => jerr(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()),
    }
}

#[derive(Deserialize)]
struct SnoozeBody {
    #[serde(default)]
    days: Option<i64>,
}

async fn act_snooze(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SnoozeBody>,
) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    if !origin_ok(&headers) {
        return jerr(StatusCode::FORBIDDEN, "cross-origin write rejected");
    }
    let days = body.days.unwrap_or(3).clamp(1, 90);
    let task = match resolve_active(&state, &id) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let until = match ptask_core::dates::now_in_operator_tz()
        .map_err(|e| e.to_string())
        .and_then(|now| {
            now.checked_add(ptask_core::jiff::Span::new().days(days))
                .map_err(|e| e.to_string())
        }) {
        Ok(z) => {
            ptask_core::dates::format_iso(&z.with_time_zone(ptask_core::jiff::tz::TimeZone::UTC))
        }
        Err(e) => return jerr(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    match ptask_core::tasks::snooze(&state.db, &task.id, &until, &dash_ctx()) {
        Ok(()) => {
            rescore(&state);
            ok_json(task.pt_id.as_deref(), &format!("snoozed {}d", days))
        }
        Err(e) => jerr(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()),
    }
}

#[derive(Deserialize)]
struct EditBody {
    title: Option<String>,
    description: Option<String>,
    priority: Option<i64>,
    /// Present-as-string sets, present-as-null clears, absent = untouched.
    #[serde(default, deserialize_with = "deserialize_maybe_null")]
    deadline: Option<Option<String>>,
}

fn deserialize_maybe_null<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(d)?))
}

async fn act_edit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<EditBody>,
) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    if !origin_ok(&headers) {
        return jerr(StatusCode::FORBIDDEN, "cross-origin write rejected");
    }
    let task = match resolve_active(&state, &id) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let ctx = dash_ctx();
    if body.title.is_none()
        && body.description.is_none()
        && body.priority.is_none()
        && body.deadline.is_none()
    {
        return jerr(StatusCode::BAD_REQUEST, "no fields to edit");
    }
    if (body.title.is_some() || body.description.is_some())
        && let Err(e) = ptask_core::tasks::update_text(
            &state.db,
            &task.id,
            body.title.as_deref(),
            body.description.as_deref(),
            &ctx,
        )
    {
        return jerr(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string());
    }
    if let Some(p) = body.priority {
        if !(1..=5).contains(&p) {
            return jerr(StatusCode::BAD_REQUEST, "priority must be 1..5");
        }
        if let Err(e) = ptask_core::tasks::update_priority(&state.db, &task.id, p, &ctx) {
            return jerr(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string());
        }
    }
    if let Some(dl) = &body.deadline
        && let Err(e) = ptask_core::tasks::update_deadline(&state.db, &task.id, dl.as_deref(), &ctx)
    {
        return jerr(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string());
    }
    rescore(&state);
    ok_json(task.pt_id.as_deref(), "edited")
}

#[derive(Deserialize)]
struct CreateBody {
    title: String,
    description: Option<String>,
    priority: Option<i64>,
    deadline: Option<String>,
}

async fn api_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateBody>,
) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    if !origin_ok(&headers) {
        return jerr(StatusCode::FORBIDDEN, "cross-origin write rejected");
    }
    let title = body.title.trim().to_string();
    if !(3..=400).contains(&title.len()) {
        return jerr(StatusCode::BAD_REQUEST, "title 3-400 chars");
    }
    if let Some(p) = body.priority
        && !(1..=5).contains(&p)
    {
        return jerr(StatusCode::BAD_REQUEST, "priority must be an integer 1..5");
    }
    // Quick-add parse keeps inline @label/#project/~duration tokens working;
    // explicit fields win — the sidecar's exact precedence.
    let q = match ptask_core::quickadd::parse(&title) {
        Ok(q) => q,
        Err(e) => return jerr(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    let new = ptask_core::NewTask {
        title: q.title.clone(),
        description: body.description.unwrap_or_else(|| q.description.clone()),
        priority: body.priority.or(q.priority).unwrap_or(2),
        deadline: body.deadline.or_else(|| q.deadline.clone()),
        source_type: "manual".into(),
        ai_confidence: 1.0,
        ai_reasoning: String::new(),
    };
    let ext = ptask_core::Extensions {
        labels: q.labels.clone(),
        project: q.project.clone(),
        duration_min: q.duration_min,
        planned_at: None,
        energy: None,
        recurrence: q.recurrence.clone(),
        due_at: q.due.clone(),
    };
    match ptask_core::tasks::create_with_extensions(&state.db, new, ext, &dash_ctx()) {
        Ok(t) => {
            rescore(&state);
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "ok": true, "message": format!("Task created: {}", t.title),
                    "pt_id": t.pt_id, "id": t.id,
                })),
            )
                .into_response()
        }
        Err(e) => jerr(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()),
    }
}

// ----------------------------------------------------------------- SSE

#[derive(Deserialize)]
struct StreamQ {
    cursor: Option<i64>,
}

/// Live journal tail. Emits `event: change` frames with journal rows past
/// the client's cursor; the cockpit refetches on receipt (data is small —
/// clients treat it as an invalidation signal with context).
async fn api_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<StreamQ>,
) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    let start = match q.cursor {
        Some(c) => c,
        None => state
            .db
            .with_conn(|c| {
                Ok(
                    c.query_row("SELECT COALESCE(MAX(id),0) FROM pt_event_log", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                )
            })
            .unwrap_or(0),
    };
    let db = state.db.clone();
    let stream = futures_util::stream::unfold(start, move |cursor| {
        let db = db.clone();
        async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let batch: Vec<(i64, String, Option<String>, String)> = db
                    .with_conn(|c| {
                        let mut stmt = c.prepare(
                            "SELECT id, event_type, actor, ts FROM pt_event_log \
                             WHERE id > ?1 ORDER BY id LIMIT 100",
                        )?;
                        let rows = stmt
                            .query_map([cursor], |r| {
                                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                            })?
                            .collect::<std::result::Result<Vec<_>, _>>()?;
                        Ok(rows)
                    })
                    .unwrap_or_default();
                if batch.is_empty() {
                    continue;
                }
                let next = batch.last().map(|r| r.0).unwrap_or(cursor);
                let events: Vec<serde_json::Value> = batch
                    .into_iter()
                    .map(|(id, et, actor, ts)| {
                        serde_json::json!({"id": id, "event_type": et, "actor": actor, "ts": ts})
                    })
                    .collect();
                let ev = Event::default()
                    .event("change")
                    .json_data(serde_json::json!({"cursor": next, "events": events}))
                    .unwrap_or_else(|_| Event::default().event("change").data("{}"));
                return Some((Ok::<Event, Infallible>(ev), next));
            }
        }
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

// ------------------------------------------------------ static + voice

async fn serve_index(State(state): State<AppState>, headers: HeaderMap) -> Response {
    serve_www_file(&state, &headers, "index.html", "text/html; charset=utf-8")
}

/// Auth-exempt: browsers fetch manifests without credentials, and the file
/// holds nothing sensitive.
async fn serve_manifest(State(state): State<AppState>) -> Response {
    let path = state.dash.www_dir.join("manifest.webmanifest");
    match std::fs::read(&path) {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/manifest+json")],
            bytes,
        )
            .into_response(),
        Err(_) => jerr(StatusCode::NOT_FOUND, "not found"),
    }
}

/// Allow-listed static files from the configured www dir. No arbitrary
/// path resolution — traversal is impossible by construction.
pub fn serve_www_file(state: &AppState, headers: &HeaderMap, name: &str, ctype: &str) -> Response {
    if !authed(state, headers) {
        return need_auth();
    }
    let path = state.dash.www_dir.join(name);
    match std::fs::read(&path) {
        Ok(bytes) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, ctype.to_string()),
                (header::CACHE_CONTROL, "no-store".to_string()),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                (header::X_FRAME_OPTIONS, "DENY".to_string()),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => jerr(StatusCode::NOT_FOUND, "not found"),
    }
}

/// Voice passthrough: STT + field extraction stay in the Python shim.
async fn api_voice(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !authed(&state, &headers) {
        return need_auth();
    }
    if !origin_ok(&headers) {
        return jerr(StatusCode::FORBIDDEN, "cross-origin write rejected");
    }
    let url = format!(
        "{}/api/voice",
        state.dash.voice_shim_url.trim_end_matches('/')
    );
    let ctype = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let client = reqwest::Client::new();
    match client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, ctype)
        .body(body.to_vec())
        .timeout(std::time::Duration::from_secs(90))
        .send()
        .await
    {
        Ok(r) => {
            let code = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let text = r.text().await.unwrap_or_default();
            (code, [(header::CONTENT_TYPE, "application/json")], text).into_response()
        }
        Err(e) => jerr(
            StatusCode::BAD_GATEWAY,
            &format!("voice shim unreachable: {}", e),
        ),
    }
}

/// Root: the cockpit when a www dir is configured and present, else the
/// plain-text banner (test/dev instances without a dashboard).
pub async fn root(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if state.dash.www_dir.join("index.html").is_file() {
        return serve_www_file(&state, &headers, "index.html", "text/html; charset=utf-8");
    }
    format!(
        "pt {} — sovereign task manager (pt serve)\nEndpoints: /healthz /version /sync /capture /list /metrics /api/*\n",
        ptask_core::VERSION
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::parse_deadline;

    #[test]
    fn parse_deadline_is_char_boundary_safe() {
        // Regression: a raw `&s[..10]` panics when byte 10 lands inside a
        // multi-byte scalar. A poisoned `deadline` row would then crash every
        // dashboard read that touches it (/api/stats, /api/tasks, timeline …).
        // "123456789é" is 11 bytes with 'é' at bytes 9..11, so byte 10 is not
        // a char boundary. Must return None, not panic.
        assert_eq!(parse_deadline("123456789é"), None);
        // A valid date-only string still parses to (date, days_until).
        assert!(parse_deadline("2026-01-01").is_some());
        // Empty / junk resolve cleanly to None.
        assert_eq!(parse_deadline(""), None);
        assert_eq!(parse_deadline("not-a-date"), None);
    }
}
