//! pTask HTTP server.
//!
//! Phase 4 of v1.0.0. `pt serve` exposes:
//!   - GET  /healthz  liveness (returns "ok")
//!   - GET  /version  binary + crate versions
//!   - GET  /          banner
//!
//! Subsequent v0.3.x sub-versions add `/capture`, `/sync`, `/webhook/*`,
//! and `/metrics`.

mod routes;

use anyhow::Result;
use axum::Router;
use ptask_core::Db;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
}

/// Build the axum Router. Exposed so tests can hit it without binding a port.
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(routes::base::router())
        .merge(routes::capture::router())
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

/// Run the server on `addr` until SIGINT / SIGTERM. Blocks the current task.
pub async fn serve(db: Db, addr: SocketAddr) -> Result<()> {
    let state = AppState { db };
    let app = router(state);
    info!(target: "ptask::server", %addr, "starting pt serve");
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    info!(target: "ptask::server", "pt serve stopped");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        let mut sig = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            Ok(s) => s,
            Err(_) => return,
        };
        sig.recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    info!(target: "ptask::server", "shutdown signal received");
}

/// Default `pt serve` bind. Leaves :9500 free for the legacy Python FastAPI
/// during the parallel-ops window (until v0.9).
pub fn default_bind() -> SocketAddr {
    "127.0.0.1:9501".parse().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn open_test_db() -> Db {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        // Minimal Python schema stub: tasks (FK target for migrations) +
        // raw_items (capture endpoint writes here).
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    created_at TEXT NOT NULL
                 );
                 CREATE TABLE raw_items (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    text TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    source_file TEXT NOT NULL,
                    source_date TEXT NOT NULL,
                    commitment_score REAL DEFAULT 0.0,
                    processed INTEGER DEFAULT 0,
                    created_at TEXT NOT NULL,
                    classification TEXT,
                    classification_confidence REAL DEFAULT 0.0,
                    classification_reasoning TEXT DEFAULT ''
                 );",
            )
            .unwrap();
        }
        // Leak the tempdir so the Db outlives this function. Tests are
        // short-lived; the OS cleans /tmp on reboot.
        std::mem::forget(dir);
        Db::open(&path).unwrap()
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let db = open_test_db();
        let app = router(AppState { db });
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn version_returns_crate_version() {
        let db = open_test_db();
        let app = router(AppState { db });
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["ptask_core"], ptask_core::VERSION);
    }

    #[tokio::test]
    async fn capture_inserts_raw_item() {
        let db = open_test_db();
        let app = router(AppState { db: db.clone() });
        let body = serde_json::json!({"text": "buy bread", "source": "http-test"});
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/capture")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(parsed["id"].as_i64().unwrap() > 0);
        assert_eq!(parsed["source_type"], "http-test");
        // Row landed.
        assert_eq!(ptask_core::raw_items::unprocessed_count(&db).unwrap(), 1);
    }

    #[tokio::test]
    async fn capture_rejects_empty_text() {
        let db = open_test_db();
        let app = router(AppState { db });
        let body = serde_json::json!({"text": "   "});
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/capture")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn root_banner() {
        let db = open_test_db();
        let app = router(AppState { db });
        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let s = std::str::from_utf8(&body).unwrap();
        assert!(s.starts_with("pt "));
    }
}
