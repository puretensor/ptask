//! Read-only query routes: `GET /next`, `GET /detail/{uuid}`,
//! `GET /resolve`.
//!
//! These expose server-side state the `/sync` delta can't carry: the DAG-ready
//! ordering (`depends_on` edges aren't in the wire `Task` shape) and a task's
//! side-table detail (labels/project/deps/recurrence via `load_detail`). Gated
//! by the same read token as `/metrics` — open when `PTASK_API_TOKEN` is unset,
//! enforced once it's configured.

use crate::AppState;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use serde::Deserialize;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/next", get(next))
        .route("/detail/{uuid}", get(detail))
        .route("/resolve", get(resolve))
}

fn default_limit() -> usize {
    20
}

const MAX_NEXT_LIMIT: usize = 500;

#[derive(Debug, Deserialize)]
struct NextParams {
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
struct ResolveParams {
    query: String,
    #[serde(default)]
    include_terminal: bool,
}

/// `GET /next?limit=N` — DAG-ready tasks (every `depends_on` predecessor done),
/// ordered by composite priority. Mirrors local `pt next`.
async fn next(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<NextParams>,
) -> impl IntoResponse {
    if let Some(resp) = crate::auth::require_read_token(&state.auth, &headers) {
        return resp;
    }
    let limit = params.limit.clamp(1, MAX_NEXT_LIMIT);
    match ptask_core::dag::next_ready(&state.db, limit) {
        Ok(tasks) => Json(serde_json::json!({ "tasks": tasks })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{e}") })),
        )
            .into_response(),
    }
}

/// `GET /detail/{uuid}` — side-table detail for one task by UUID (labels,
/// project, duration, dependencies, recurrence). Returns defaults for a
/// missing row, matching `load_detail`.
async fn detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(uuid): Path<String>,
) -> impl IntoResponse {
    if let Some(resp) = crate::auth::require_read_token(&state.auth, &headers) {
        return resp;
    }
    match ptask_core::tasks::load_detail(&state.db, &uuid) {
        Ok(d) => Json(d).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{e}") })),
        )
            .into_response(),
    }
}

/// `GET /resolve?query=...&include_terminal=false` — resolve a PT-N, bare
/// integer, or title substring on the server so remote mutating verbs no longer
/// full-sync every task just to find one UUID.
async fn resolve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ResolveParams>,
) -> impl IntoResponse {
    if let Some(resp) = crate::auth::require_read_token(&state.auth, &headers) {
        return resp;
    }
    match ptask_core::tasks::resolve_for_lookup(&state.db, &params.query, params.include_terminal) {
        Ok(task) => Json(serde_json::json!({ "task": task })).into_response(),
        Err(e) => (
            resolve_error_status(&e),
            Json(serde_json::json!({ "error": format!("{e}") })),
        )
            .into_response(),
    }
}

fn resolve_error_status(e: &ptask_core::Error) -> StatusCode {
    let msg = e.to_string();
    match e {
        ptask_core::Error::PtIdNotFound(_) => StatusCode::NOT_FOUND,
        ptask_core::Error::Other(_) if msg == "empty task query" => StatusCode::BAD_REQUEST,
        ptask_core::Error::Other(_)
            if msg.starts_with("no active task matching")
                || msg.starts_with("no task matching") =>
        {
            StatusCode::NOT_FOUND
        }
        ptask_core::Error::Other(_) if msg.contains(" tasks match ") => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
