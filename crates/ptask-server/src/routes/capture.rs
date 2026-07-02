//! POST /capture — write to `raw_items` for downstream distill.

use crate::AppState;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::post;
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
}

#[derive(Debug, Serialize)]
pub struct CaptureResp {
    pub id: i64,
    pub source_type: String,
    pub source_date: String,
}

async fn capture(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CaptureReq>,
) -> impl IntoResponse {
    if let Some(resp) = crate::auth::require_write_token(&state.auth, &headers) {
        return resp;
    }
    let text = req.text.trim();
    if text.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "text must be non-empty"})),
        )
            .into_response();
    }
    let source = req.source.unwrap_or_else(|| "http".into());
    let source_file = req.source_file.unwrap_or_else(|| "http://capture".into());
    match ptask_core::raw_items::insert(&state.db, text, &source, &source_file) {
        Ok(r) => (
            StatusCode::CREATED,
            Json(CaptureResp {
                id: r.id,
                source_type: r.source_type,
                source_date: r.source_date,
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(target: "ptask::capture", error = %e, "insert failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{}", e)})),
            )
                .into_response()
        }
    }
}
