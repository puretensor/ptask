//! Base routes: health, version, banner.

use crate::AppState;
use axum::Router;
use axum::response::{IntoResponse, Json};
use axum::routing::get;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(crate::routes::dashboard::root))
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
