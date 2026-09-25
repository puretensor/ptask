//! POST /capture — inbox for external items.
//!
//! Default path: write to `raw_items` for the distillation pipeline.
//!
//! CRITICAL FAST-LANE (v2.1.0): a capture carrying `severity >= 3` — either
//! the explicit `severity` field or a puresentinel incident (source
//! `puresentinel:incident:…`, `[puresentinel sevN]` in the text) — creates
//! the task SYNCHRONOUSLY, attributed to the capturing identity. Before
//! this, puresentinel's critical incidents (Ceph HEALTH_ERR, site outages)
//! sat unread in raw_items with up-to-24h materialization latency; the
//! fleet's incident-to-action chain ended at an INSERT. The raw_items row
//! is still written (as the record) but marked processed so distill won't
//! double-create.

use crate::AppState;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use ptask_core::event_log::EventCtx;
use ptask_core::tokens::Scope;
use serde::{Deserialize, Serialize};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/capture", post(capture))
        .route("/capture/resolve", post(resolve))
}

#[derive(Debug, Deserialize)]
pub struct ResolveReq {
    /// The deterministic capture key the incident was captured under.
    pub client_key: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ResolveResp {
    /// Tasks transitioned to done. 0 for an unknown key (idempotent no-op —
    /// incidents captured before v2.5.0 carry keys ptask never saw).
    pub closed: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pt_ids: Vec<String>,
}

/// POST /capture/resolve — close-on-recovery (v2.6.0). The capturing source
/// reports the condition cleared; every OPEN task carrying that capture_key
/// is marked done with provenance. Capture scope: this is the recovery half
/// of the capture contract, and it can only touch tasks the capture path
/// itself created (keyed, machine-sourced).
async fn resolve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ResolveReq>,
) -> impl IntoResponse {
    crate::blocking::db_response(move || resolve_blocking(state, headers, req)).await
}

fn resolve_blocking(
    state: AppState,
    headers: HeaderMap,
    req: ResolveReq,
) -> axum::response::Response {
    let identity = match crate::auth::authenticate(&state.db, &state.auth, &headers, Scope::Capture)
    {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let key = req.client_key.trim().to_string();
    if key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "client_key must be non-empty"})),
        )
            .into_response();
    }
    let open: Vec<String> = match state.db.with_conn(|c| {
        let mut stmt = c.prepare(
            "SELECT id FROM tasks
                 WHERE capture_key = ?1 AND status_v2 NOT IN ('done','dismissed')",
        )?;
        let rows = stmt.query_map([&key], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }) {
        Ok(open) => open,
        Err(e) => {
            tracing::error!(
                target: "ptask::capture",
                error = %e,
                client_key = %key,
                "capture resolve query failed"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "database query failed"})),
            )
                .into_response();
        }
    };

    let mut closed = 0usize;
    let mut pt_ids = Vec::new();
    for uuid in &open {
        let task = match ptask_core::tasks::resolve_for_lookup(&state.db, uuid, false) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(target: "ptask::capture", error = %e, uuid = %uuid, "resolve lookup failed");
                continue;
            }
        };
        let ctx = EventCtx {
            actor: identity.client_id.clone(),
            source: "capture-resolve".into(),
            // Keyed on the task's updated_at too: a reopened task must be
            // closable by the next recovery, and a fixed (key, task) uuid
            // hit the event log's UNIQUE index so mark_done failed forever.
            event_uuid: Some(format!(
                "capture-resolve:{}:{}:{}",
                key, uuid, task.updated_at
            )),
        };
        match ptask_core::tasks::mark_done(&state.db, &task, &ctx) {
            Ok(_) => {
                closed += 1;
                if let Some(pt) = task.pt_id.clone() {
                    pt_ids.push(pt);
                }
                let payload = serde_json::json!({
                    "client_key": key,
                    "note": req.note,
                });
                let ev_uuid = format!("capture-resolve-note:{}:{}:{}", key, uuid, task.updated_at);
                if let Err(e) = ptask_core::event_log::record(
                    &state.db,
                    &ev_uuid,
                    Some(uuid),
                    "task.capture_resolved",
                    &payload,
                    &ctx,
                ) {
                    tracing::warn!(target: "ptask::capture", error = %e, "resolve event failed");
                }
            }
            Err(e) => {
                tracing::warn!(target: "ptask::capture", error = %e, uuid = %uuid, "mark_done failed");
            }
        }
    }
    tracing::info!(
        target: "ptask::capture",
        client_key = %key,
        closed,
        actor = %identity.client_id,
        "capture resolve processed"
    );
    (StatusCode::OK, Json(ResolveResp { closed, pt_ids })).into_response()
}

#[derive(Debug, Deserialize)]
pub struct CaptureReq {
    pub text: String,
    /// Logical source (`telegram`, `email`, `hal`, ...). Defaults to `http`.
    #[serde(default)]
    pub source: Option<String>,
    /// Breadcrumb identifying the origin (e.g. `telegram:msg/123`). Defaults
    /// to `http://capture`.
    #[serde(default)]
    pub source_file: Option<String>,
    /// Incident severity. `>= 3` takes the fast lane (immediate task).
    #[serde(default)]
    pub severity: Option<i64>,
    /// Stable client key for idempotent federation: a re-send with the same
    /// key + text returns the original row instead of a new one (kills the
    /// heartbeat/fleet-sentry re-nag loop).
    #[serde(default)]
    pub client_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CaptureResp {
    pub id: i64,
    pub source_type: String,
    pub source_date: String,
    /// True when client_key matched an existing capture (idempotent replay).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[serde(default)]
    pub duplicate: bool,
    /// Set when the fast lane created a task synchronously.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pt_id: Option<String>,
}

/// Delivery idempotency vs incident identity.
enum CaptureIdentity {
    Proceed,
    Duplicate,
    NewEpisode,
}

fn capture_identity_action(
    duplicate: bool,
    is_incident: bool,
    has_capture_key: bool,
    has_open_keyed_task: bool,
) -> CaptureIdentity {
    if !duplicate {
        return CaptureIdentity::Proceed;
    }
    if is_incident && has_capture_key && !has_open_keyed_task {
        return CaptureIdentity::NewEpisode;
    }
    CaptureIdentity::Duplicate
}

/// Is incident `key` still live on an open task? Either a task carries the
/// key itself, or the fast lane merged a capture under this key onto
/// another open incident (semantic match keeps that task's own
/// capture_key, recording the absorbed key only on the occurrence event).
/// Without the second arm every re-send of a merged incident read as
/// "recovered" and minted a new raw_items episode plus an occurrence event,
/// without bound.
fn open_incident_for_key(db: &ptask_core::Db, key: &str) -> Result<bool, ptask_core::Error> {
    db.with_conn(|c| {
        Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks
                           WHERE capture_key = ?1
                             AND status_v2 NOT IN ('done','dismissed'))
                 OR EXISTS(SELECT 1 FROM tasks t
                           JOIN pt_event_log e ON e.task_uuid = t.id
                           WHERE t.source_type = 'incident'
                             AND t.status_v2 NOT IN ('done','dismissed')
                             AND e.event_type = 'task.capture_occurrence'
                             AND json_extract(e.payload, '$.client_key') = ?1)",
            [key],
            |r| r.get::<_, bool>(0),
        )?)
    })
}

fn next_episode_source_file(
    db: &ptask_core::Db,
    source_file: &str,
) -> Result<String, ptask_core::Error> {
    let n: i64 = db.with_conn(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM raw_items
             WHERE source_file = ?1 OR source_file LIKE ?2",
            rusqlite::params![source_file, format!("{source_file}#episode=%")],
            |r| r.get(0),
        )?)
    })?;
    Ok(format!("{source_file}#episode={}", n + 1))
}

fn duplicate_capture_response(r: ptask_core::raw_items::RawItem) -> axum::response::Response {
    (
        StatusCode::OK,
        Json(CaptureResp {
            id: r.id,
            source_type: r.source_type,
            source_date: r.source_date,
            duplicate: true,
            task_uuid: None,
            pt_id: None,
        }),
    )
        .into_response()
}

/// Severity from the explicit field, else parsed from a puresentinel
/// incident marker (`[puresentinel sevN]`) when the source says incident.
fn effective_severity(req: &CaptureReq, source: &str) -> Option<i64> {
    if let Some(s) = req.severity {
        return Some(s);
    }
    if source.starts_with("puresentinel:incident:") {
        // First `sev` followed by digits: an earlier word ("several OSDs
        // down [puresentinel sev4]") must not hide the marker.
        let marked = req.text.match_indices("sev").find_map(|(idx, _)| {
            let digits: String = req.text[idx + 3..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse::<i64>().ok()
        });
        // Incident source without a parsable marker still counts as critical.
        return Some(marked.unwrap_or(3));
    }
    None
}

async fn capture(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CaptureReq>,
) -> impl IntoResponse {
    crate::blocking::db_response(move || capture_blocking(state, headers, req)).await
}

fn capture_blocking(
    state: AppState,
    headers: HeaderMap,
    req: CaptureReq,
) -> axum::response::Response {
    let identity = match crate::auth::authenticate(&state.db, &state.auth, &headers, Scope::Capture)
    {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let text = req.text.trim().to_string();
    if text.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "text must be non-empty"})),
        )
            .into_response();
    }
    let source = req.source.clone().unwrap_or_else(|| "http".into());
    // One normalised key for every use below. The raw value used to be
    // stored and matched in some places and trimmed in others: an empty
    // `client_key` was stored as capture_key '' and then exact-matched every
    // later unrelated keyed-"" incident (dropping it as a duplicate), and a
    // padded key was stored raw so /capture/resolve (trimmed) never closed it.
    let capture_key: Option<String> = req
        .client_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let source_file = req
        .source_file
        .clone()
        .or_else(|| capture_key.clone())
        .unwrap_or_else(|| "http://capture".into());

    // PT-1687 shipped the unique index on (source_file, text); the HTTP lane was
    // left on the non-tolerant insert, so an unkeyed re-send (and the loser of
    // two concurrent keyed sends) surfaced as a 500 carrying raw SQLite text.
    // The insert itself is the idempotency check now: one statement, no race.
    // Recovered incidents are a new episode: the unique key is scoped to the
    // episode so a later identical capture can create work again.
    let severity = effective_severity(&req, &source);
    let is_incident = severity.is_some_and(|s| s >= 3);
    let has_open_keyed_task = match capture_key.as_deref() {
        Some(key) => match open_incident_for_key(&state.db, key) {
            Ok(open) => open,
            Err(e) => {
                tracing::error!(
                    target: "ptask::capture",
                    error = %e,
                    "open-incident lookup failed — refusing new episode"
                );
                true
            }
        },
        None => false,
    };
    let row =
        match ptask_core::raw_items::insert_idempotent(&state.db, &text, &source, &source_file) {
            Ok((r, duplicate)) => {
                match capture_identity_action(
                    duplicate,
                    is_incident,
                    capture_key.is_some(),
                    has_open_keyed_task,
                ) {
                    CaptureIdentity::Duplicate => return duplicate_capture_response(r),
                    CaptureIdentity::Proceed => r,
                    CaptureIdentity::NewEpisode => {
                        let episode_file = match next_episode_source_file(&state.db, &source_file) {
                            Ok(f) => f,
                            Err(e) => {
                                tracing::error!(
                                    target: "ptask::capture",
                                    error = %e,
                                    "episode source_file failed"
                                );
                                return (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    Json(serde_json::json!({"error": format!("{}", e)})),
                                )
                                    .into_response();
                            }
                        };
                        match ptask_core::raw_items::insert_idempotent(
                            &state.db,
                            &text,
                            &source,
                            &episode_file,
                        ) {
                            Ok((episode_row, _)) => episode_row,
                            Err(e) => {
                                tracing::error!(
                                    target: "ptask::capture",
                                    error = %e,
                                    "episode insert failed"
                                );
                                return (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    Json(serde_json::json!({"error": format!("{}", e)})),
                                )
                                    .into_response();
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::error!(target: "ptask::capture", error = %e, "insert failed");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("{}", e)})),
                )
                    .into_response();
            }
        };

    // ---- critical fast lane -------------------------------------------------
    let mut task_uuid = None;
    let mut pt_id = None;
    if let Some(sev) = severity.filter(|s| *s >= 3) {
        let priority = if sev >= 4 { 5 } else { 4 };
        let title: String = text
            .lines()
            .next()
            .unwrap_or(&text)
            .chars()
            .take(200)
            .collect();

        // ---- v2.5.0 signal intelligence: refresh, don't duplicate ----------
        // The same live incident re-captured (deterministic capture_key) or
        // re-worded (semantic >= 0.82) bumps the existing open task instead of
        // minting a new PT-N. Fail-open: any error falls through to create.
        let open_incidents: Vec<(String, String, Option<String>)> = state
            .db
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT id, title, capture_key FROM tasks
                     WHERE source_type = 'incident'
                       AND status_v2 NOT IN ('done','dismissed')",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<String>>(2)?))
                })?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .unwrap_or_default();

        let exact = capture_key.as_ref().and_then(|k| {
            open_incidents
                .iter()
                .find(|(_, _, ck)| ck.as_deref() == Some(k.as_str()))
                .map(|(uuid, _, _)| (uuid.clone(), 1.0f32, "capture_key"))
        });
        let matched = if exact.is_some() {
            exact
        } else {
            let title_owned = title.clone();
            let cands: Vec<(String, String)> = open_incidents
                .iter()
                .map(|(u, t, _)| (u.clone(), t.clone()))
                .collect();
            crate::dedup::best_match(&title_owned, &cands, crate::dedup::CAPTURE_THRESHOLD)
                .map(|(u, s)| (u, s, "semantic"))
        };

        if let Some((existing_uuid, score, how)) = matched {
            let now = ptask_core::dates::format_iso(
                &ptask_core::dates::now_in_operator_tz().unwrap_or_else(|_| jiff::Zoned::now()),
            );
            let refresh = state.db.with_conn(|c| {
                c.execute(
                    "UPDATE tasks SET
                         capture_count = capture_count + 1,
                         last_captured_at = ?1,
                         updated_at = ?1,
                         capture_key = COALESCE(capture_key, ?2)
                     WHERE id = ?3",
                    rusqlite::params![now, capture_key, existing_uuid],
                )?;
                Ok(())
            });
            match refresh {
                Ok(()) => {
                    let ctx = EventCtx {
                        actor: identity.client_id.clone(),
                        source: "capture".into(),
                        event_uuid: None,
                    };
                    let ev_uuid = format!("capture-occurrence:{}", row.id);
                    let payload = serde_json::json!({
                        "matched_by": how,
                        "score": score,
                        "severity": sev,
                        "raw_item_id": row.id,
                        "client_key": capture_key,
                        "title": title,
                    });
                    if let Err(e) = ptask_core::event_log::record(
                        &state.db,
                        &ev_uuid,
                        Some(&existing_uuid),
                        "task.capture_occurrence",
                        &payload,
                        &ctx,
                    ) {
                        tracing::warn!(target: "ptask::capture", error = %e, "occurrence event failed");
                    }
                    if let Err(e) = ptask_core::raw_items::mark_processed(&state.db, row.id) {
                        tracing::warn!(target: "ptask::capture", error = %e, "mark_processed failed");
                    }
                    let existing_pt: Option<String> = state
                        .db
                        .with_conn(|c| {
                            Ok(c.query_row(
                                "SELECT pt_id FROM tasks WHERE id = ?1",
                                [&existing_uuid],
                                |r| r.get::<_, Option<String>>(0),
                            )?)
                        })
                        .unwrap_or(None);
                    tracing::info!(
                        target: "ptask::capture",
                        severity = sev,
                        matched_by = how,
                        score = score,
                        pt_id = existing_pt.as_deref().unwrap_or("-"),
                        "fast-lane incident deduped onto existing task"
                    );
                    return (
                        StatusCode::OK,
                        Json(CaptureResp {
                            id: row.id,
                            source_type: row.source_type,
                            source_date: row.source_date,
                            duplicate: true,
                            task_uuid: Some(existing_uuid),
                            pt_id: existing_pt,
                        }),
                    )
                        .into_response();
                }
                Err(e) => {
                    // Fail open: fall through to normal create.
                    tracing::warn!(target: "ptask::capture", error = %e, "occurrence refresh failed — creating new task");
                }
            }
        }

        let new = ptask_core::NewTask {
            title,
            description: text.clone(),
            priority,
            deadline: None,
            source_type: "incident".into(),
            ai_confidence: 1.0,
            ai_reasoning: format!("fast-lane capture severity {} from {}", sev, source),
        };
        let ctx = EventCtx {
            actor: identity.client_id.clone(),
            source: "capture".into(),
            event_uuid: Some(format!("capture:{}", row.id)),
        };
        match ptask_core::tasks::create_with_extensions(
            &state.db,
            new,
            ptask_core::Extensions::default(),
            &ctx,
        ) {
            Ok(t) => {
                task_uuid = Some(t.id.clone());
                pt_id = t.pt_id.clone();
                if let Some(key) = capture_key.as_deref() {
                    let set = state.db.with_conn(|c| {
                        c.execute(
                            "UPDATE tasks SET capture_key = ?1 WHERE id = ?2",
                            rusqlite::params![key, t.id],
                        )?;
                        Ok(())
                    });
                    if let Err(e) = set {
                        tracing::warn!(target: "ptask::capture", error = %e, "capture_key set failed");
                    }
                }
                if let Err(e) = ptask_core::raw_items::mark_processed(&state.db, row.id) {
                    tracing::warn!(target: "ptask::capture", error = %e, "mark_processed failed");
                }
                if let Err(e) = ptask_core::scoring::run_once(&state.db, false) {
                    tracing::warn!(target: "ptask::capture", error = %e, "rescore failed");
                }
                tracing::info!(
                    target: "ptask::capture",
                    severity = sev,
                    pt_id = pt_id.as_deref().unwrap_or("-"),
                    actor = %identity.client_id,
                    "fast-lane incident task created"
                );
            }
            Err(e) => {
                // The raw_items record survives; distill will pick it up —
                // degraded latency but no data loss.
                tracing::error!(target: "ptask::capture", error = %e, "fast-lane create failed");
            }
        }
    }

    (
        StatusCode::CREATED,
        Json(CaptureResp {
            id: row.id,
            source_type: row.source_type,
            source_date: row.source_date,
            duplicate: false,
            task_uuid,
            pt_id,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn incident(text: &str) -> Option<i64> {
        let req = CaptureReq {
            text: text.into(),
            source: None,
            source_file: None,
            severity: None,
            client_key: None,
        };
        effective_severity(&req, "puresentinel:incident:ceph")
    }

    #[test]
    fn severity_marker_is_found_past_an_earlier_sev_word() {
        assert_eq!(incident("several OSDs down [puresentinel sev4]"), Some(4));
        assert_eq!(incident("[puresentinel sev5] mon quorum lost"), Some(5));
    }

    #[test]
    fn incident_without_a_marker_is_still_critical() {
        assert_eq!(incident("severe latency on fox-n1"), Some(3));
    }

    #[test]
    fn explicit_severity_wins_and_non_incidents_have_none() {
        let req = CaptureReq {
            text: "[puresentinel sev5]".into(),
            source: None,
            source_file: None,
            severity: Some(2),
            client_key: None,
        };
        assert_eq!(effective_severity(&req, "puresentinel:incident:x"), Some(2));
        let req = CaptureReq {
            severity: None,
            ..req
        };
        assert_eq!(effective_severity(&req, "telegram"), None);
    }
}
