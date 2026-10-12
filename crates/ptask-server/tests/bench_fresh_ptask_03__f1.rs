//! `pt serve` hands out the cockpit page at `GET /`, and that page gates
//! `startApp()` on `fetch('/api/config')` (dashboard/www/index.html). The
//! Rust dashboard router must therefore answer `/api/config`, as the Python
//! sidecar does; otherwise the served cockpit never starts (board stuck STALE).

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use ptask_core::config::DashConfig;
use ptask_server::{AppState, router};
use std::path::PathBuf;
use tower::ServiceExt;

fn cockpit_www() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dashboard/www")
}

fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(
        ptask_core::Db::open(dir.path().join("test.db")).unwrap(),
        Default::default(),
        Default::default(),
    )
    .with_dash(DashConfig {
        www_dir: cockpit_www(),
        ..Default::default()
    });
    (router(state), dir)
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Vec<u8>) {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = to_bytes(resp.into_body(), 1 << 20).await.unwrap().to_vec();
    (status, body)
}

#[tokio::test]
async fn served_cockpit_can_load_its_api_config() {
    let (app, _guard) = app();

    // The page is served...
    let (status, html) = get(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    let html = String::from_utf8(html).unwrap();
    // ...and it blocks startup on /api/config.
    assert!(
        html.contains("fetch('/api/config'"),
        "cockpit no longer fetches /api/config; update this test"
    );

    let (status, body) = get(&app, "/api/config").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "pt serve serves the cockpit but /api/config is not routed, so startApp() never runs"
    );
    let cfg: serde_json::Value = serde_json::from_slice(&body).expect("/api/config must be JSON");
    assert!(cfg.is_object(), "/api/config must return a JSON object");
}
