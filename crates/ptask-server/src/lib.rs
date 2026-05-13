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
pub mod webhooks;

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
        .merge(routes::sync::router())
        .merge(routes::metrics::router())
        .merge(routes::webhook_git::router())
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
        // Production-shape stubs: tasks + interactions + raw_items. Mirrors
        // what `ptask_core::tasks` tests do — but extended with raw_items
        // for the /capture endpoint.
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (
                    id               TEXT PRIMARY KEY,
                    title            TEXT NOT NULL,
                    description      TEXT DEFAULT '',
                    priority         INTEGER DEFAULT 2,
                    status           TEXT DEFAULT 'pending',
                    created_at       TEXT NOT NULL,
                    updated_at       TEXT NOT NULL,
                    deadline         TEXT,
                    source_type      TEXT DEFAULT 'manual',
                    source_files     TEXT DEFAULT '[]',
                    ai_confidence    REAL DEFAULT 1.0,
                    ai_reasoning     TEXT DEFAULT '',
                    depends_on       TEXT DEFAULT '[]',
                    blocks_tasks     TEXT DEFAULT '[]',
                    escalation_level INTEGER DEFAULT 0,
                    dismissal_count  INTEGER DEFAULT 0,
                    last_reminded    TEXT,
                    next_reminder    TEXT,
                    priority_score   REAL DEFAULT 0.0,
                    score_urgency    REAL DEFAULT 0.0,
                    score_dependency REAL DEFAULT 0.0,
                    score_neglect    REAL DEFAULT 0.0,
                    subtasks         TEXT DEFAULT '[]',
                    task_type        TEXT DEFAULT 'operational',
                    cluster_keywords TEXT DEFAULT '[]'
                 );
                 CREATE TABLE interactions (
                    id      INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    action  TEXT NOT NULL,
                    ts      TEXT NOT NULL,
                    details TEXT DEFAULT ''
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
    async fn sync_round_trip_create_then_done() {
        let db = open_test_db();
        let app = router(AppState { db: db.clone() });

        // First call: create one task via sync command.
        let req1 = serde_json::json!({
            "sync_token": "*",
            "commands": [{
                "type": "task_create",
                "uuid": "cmd-1",
                "temp_id": "tmp-a",
                "args": { "text": "buy bread" }
            }]
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sync")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req1).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["sync_status"]["cmd-1"], "ok");
        let real_uuid = parsed["temp_id_mapping"]["tmp-a"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!real_uuid.is_empty());
        let first_token = parsed["sync_token"].as_str().unwrap().to_string();
        assert_eq!(parsed["resources"]["tasks"].as_array().unwrap().len(), 1);

        // Second call with same uuid → idempotent "ok", no double-create.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sync")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req1).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["sync_status"]["cmd-1"], "ok");
        assert_eq!(
            parsed["temp_id_mapping"]["tmp-a"].as_str().unwrap(),
            real_uuid
        );

        // Third call: mark done by task_uuid; sync_token = first_token so
        // we only get the delta from the done event.
        let req3 = serde_json::json!({
            "sync_token": first_token,
            "commands": [{
                "type": "task_done",
                "uuid": "cmd-2",
                "args": { "task_uuid": real_uuid }
            }]
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/sync")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req3).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["sync_status"]["cmd-2"], "ok");
        let tasks = parsed["resources"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["status"], "done");
    }

    #[tokio::test]
    async fn sync_full_sync_returns_existing_tasks_without_events() {
        let db = open_test_db();
        let existing =
            ptask_core::tasks::create(&db, ptask_core::NewTask::minimal("preexisting")).unwrap();
        assert_eq!(ptask_core::event_log::current_cursor(&db).unwrap(), 0);

        let app = router(AppState { db });
        let req = serde_json::json!({"sync_token": "*", "commands": []});
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/sync")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let tasks = parsed["resources"]["tasks"].as_array().unwrap();
        assert!(
            tasks
                .iter()
                .any(|task| task["id"].as_str() == Some(existing.id.as_str())),
            "full sync should include tasks that predate pt_event_log"
        );
    }

    #[test]
    fn webhook_sign_is_stable_hex() {
        let sig = webhooks::sign(b"hello world", "secret-key");
        // Known answer for HMAC-SHA256 with these inputs.
        assert_eq!(
            sig,
            "095d5a21fe6d0646db223fdf3de6436bb8dfb2fab0b51677ecf6441fcf5f2a67"
        );
    }

    #[test]
    fn webhook_sign_empty_secret_returns_empty() {
        assert_eq!(webhooks::sign(b"hello", ""), "");
    }

    #[test]
    fn webhook_log_records_outbound_send() {
        // Force-record a log row via the public helper to prove the schema
        // bind path works against the test stub.
        let db = open_test_db();
        let id = ptask_core::webhook_log::record(
            &db,
            ptask_core::webhook_log::Direction::Out,
            "https://example.test/hook",
            &serde_json::json!({"event": "task.created"}),
            true,
        )
        .unwrap();
        assert!(id > 0);
    }

    #[tokio::test]
    async fn metrics_returns_prometheus_text() {
        let db = open_test_db();
        let app = router(AppState { db });
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(ct.starts_with("text/plain"));
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let s = std::str::from_utf8(&body).unwrap();
        assert!(s.contains("# TYPE pt_tasks_total gauge"));
        assert!(s.contains("pt_raw_items_unprocessed "));
        assert!(s.contains("pt_event_log_cursor "));
        assert!(s.contains("pt_views_total "));
    }

    #[tokio::test]
    async fn gitea_webhook_closes_pt_n_on_fixes() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let db = open_test_db();
        // Create a task we can close via the magic word.
        let t = ptask_core::tasks::create(&db, ptask_core::NewTask::minimal("close me")).unwrap();
        let pt = t.pt_id.clone().unwrap();
        // Set the secret for this test scope.
        unsafe {
            std::env::set_var("PTASK_GITEA_WEBHOOK_SECRET", "test-secret");
        }
        let body = serde_json::json!({
            "ref": "refs/heads/main",
            "commits": [
                { "id": "abc123", "message": format!("Fixes {}: ship it", pt) }
            ]
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(b"test-secret").unwrap();
        mac.update(&body_bytes);
        let sig = hex::encode(mac.finalize().into_bytes());
        let app = router(AppState { db: db.clone() });
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/webhook/gitea")
                    .method("POST")
                    .header("content-type", "application/json")
                    .header("X-Gitea-Signature", sig)
                    .body(Body::from(body_bytes))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // Task should now be done.
        db.with_conn(|c| {
            let status: String = c
                .query_row("SELECT status FROM tasks WHERE id=?1", [&t.id], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(status, "done");
            Ok(())
        })
        .unwrap();
        // Cleanup.
        unsafe {
            std::env::remove_var("PTASK_GITEA_WEBHOOK_SECRET");
        }
    }

    #[tokio::test]
    async fn gitea_webhook_rejects_bad_signature() {
        let db = open_test_db();
        unsafe {
            std::env::set_var("PTASK_GITEA_WEBHOOK_SECRET", "test-secret");
        }
        let body = serde_json::json!({"ref": "refs/heads/main", "commits": []});
        let app = router(AppState { db });
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/webhook/gitea")
                    .method("POST")
                    .header("content-type", "application/json")
                    .header("X-Gitea-Signature", "deadbeef")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        unsafe {
            std::env::remove_var("PTASK_GITEA_WEBHOOK_SECRET");
        }
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
