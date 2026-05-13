//! Base routes: health, version, banner.

use crate::AppState;
use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Json};
use axum::routing::get;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(banner))
        .route("/healthz", get(healthz))
        .route("/version", get(version))
}

async fn banner(State(_s): State<AppState>) -> impl IntoResponse {
    format!(
        "pt {} — sovereign task manager (pt serve)\nEndpoints: /healthz /version (more in later v0.3.x)\n",
        ptask_core::VERSION
    )
}

async fn healthz() -> impl IntoResponse {
    "ok"
}

async fn version() -> impl IntoResponse {
    Json(serde_json::json!({
        "ptask_core": ptask_core::VERSION,
    }))
}
