//! Read-only query routes (v1.9.0): `GET /next`, `GET /detail/{uuid}`.
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
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, Deserialize)]
struct NextParams {
    #[serde(default = "default_limit")]
    limit: usize,
}

/// `GET /next?limit=N` — DAG-ready tasks (every `depends_on` predecessor done),
/// ordered by composite priority. Mirrors local `pt next`.
async fn next(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<NextParams>,
) -> impl IntoResponse {
    if let Some(resp) = crate::auth::require_read_token(&headers) {
        return resp;
    }
    match ptask_core::dag::next_ready(&state.db, params.limit) {
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
    if let Some(resp) = crate::auth::require_read_token(&headers) {
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
