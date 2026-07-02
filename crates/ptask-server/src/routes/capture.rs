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
    Router::new().route("/capture", post(capture))
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
}

#[derive(Debug, Serialize)]
pub struct CaptureResp {
    pub id: i64,
    pub source_type: String,
    pub source_date: String,
    /// Set when the fast lane created a task synchronously.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pt_id: Option<String>,
}

/// Severity from the explicit field, else parsed from a puresentinel
/// incident marker (`[puresentinel sevN]`) when the source says incident.
fn effective_severity(req: &CaptureReq, source: &str) -> Option<i64> {
    if let Some(s) = req.severity {
        return Some(s);
    }
    if source.starts_with("puresentinel:incident:") {
        if let Some(idx) = req.text.find("sev") {
            let tail = &req.text[idx + 3..];
            let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<i64>() {
                return Some(n);
            }
        }
        // Incident source without a parsable marker still counts as critical.
        return Some(3);
    }
    None
}

async fn capture(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CaptureReq>,
) -> impl IntoResponse {
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
    let source_file = req
        .source_file
        .clone()
        .unwrap_or_else(|| "http://capture".into());

    let row = match ptask_core::raw_items::insert(&state.db, &text, &source, &source_file) {
        Ok(r) => r,
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
    let severity = effective_severity(&req, &source);
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
            task_uuid,
            pt_id,
        }),
    )
        .into_response()
}
