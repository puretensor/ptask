//! Base routes: health and version.

use crate::AppState;
use axum::Router;
use axum::response::{IntoResponse, Json};
use axum::routing::get;

pub fn router() -> Router<AppState> {
    // `/` serves the cockpit, so it lives in the dashboard router behind the
    // Host guard and the Basic-auth throttle.
    Router::new()
        .route("/healthz", get(healthz))
        .route("/version", get(version))
}

async fn healthz() -> impl IntoResponse {
    "ok"
}

async fn version() -> impl IntoResponse {
    Json(serde_json::json!({
        "ptask_core": ptask_core::VERSION,
    }))
}
