//! pTask HTTP server.
//!
//! Phase 4 of v1.0.0. `pt serve` exposes:
//!   - GET  /healthz  liveness (returns "ok")
//!   - GET  /version  binary + crate versions
//!   - GET  /          banner
//!
//! Subsequent v0.3.x sub-versions add `/capture`, `/sync`, `/webhook/*`,
//! and `/metrics`.

mod auth;
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
        .merge(routes::email::router())
        .merge(routes::sync::router())
        .merge(routes::read::router())
        .merge(routes::metrics::router())
        .merge(routes::webhook_git::router())
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

/// Run the server on `addr` until SIGINT / SIGTERM. Blocks the current task.
pub async fn serve(db: Db, addr: SocketAddr) -> Result<()> {
    let state = AppState { db };
    let app = router(state);
    auth::warn_if_unconfigured();
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

    // Serializes tests that read/write the process-global PTASK_API_TOKEN env.
    // ANY test that exercises a token-gated route (/sync, /capture, /email) MUST
    // hold this lock, or a concurrent token-setting test can flip its request to
    // 401. (This is why the env-mutating + every gated-route test below lock it.)
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
        // Hits the token-gated /capture route; hold ENV_LOCK so a concurrent
        // test that sets PTASK_API_TOKEN can't flip this to 401.
        let _env = ENV_LOCK.lock().await;
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
        // Gated /capture route; hold ENV_LOCK (a leaked token would 401 before
        // the 400 we assert).
        let _env = ENV_LOCK.lock().await;
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
        // Gated /sync route; hold ENV_LOCK so a concurrent token-setting test
        // can't flip these 200s to 401.
        let _env = ENV_LOCK.lock().await;
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

    /// POST a /sync body and return the parsed JSON response (asserts HTTP 200).
    async fn post_sync(app: &Router, body: &serde_json::Value) -> serde_json::Value {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sync")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn sync_round_trip_priority_edit_reopen() {
        let _env = ENV_LOCK.lock().await;
        let db = open_test_db();
        let app = router(AppState { db: db.clone() });

        // Create a task.
        let created = post_sync(
            &app,
            &serde_json::json!({
                "sync_token": "*",
                "commands": [{ "type": "task_create", "uuid": "c-1", "temp_id": "t1",
                               "args": { "text": "ship it" } }]
            }),
        )
        .await;
        assert_eq!(created["sync_status"]["c-1"], "ok");
        let uuid = created["temp_id_mapping"]["t1"]
            .as_str()
            .unwrap()
            .to_string();

        // task_priority → critical (5).
        let pri = post_sync(
            &app,
            &serde_json::json!({
                "sync_token": "*",
                "commands": [{ "type": "task_priority", "uuid": "c-2",
                               "args": { "task_uuid": uuid, "priority": 5 } }]
            }),
        )
        .await;
        assert_eq!(pri["sync_status"]["c-2"], "ok");
        let t = pri["resources"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == uuid)
            .unwrap();
        assert_eq!(t["priority"], 5);

        // Idempotent replay of the same command uuid → ok, and NO double-apply.
        // Replay carries a DIFFERENT priority (1) under the same uuid c-2: if the
        // event-log guard ever broke, this would flip the task to 1. The assertion
        // that it stays 5 is what proves the short-circuit (a replay of the same
        // value could not distinguish "skipped" from "re-applied").
        let replay = post_sync(
            &app,
            &serde_json::json!({
                "sync_token": "*",
                "commands": [{ "type": "task_priority", "uuid": "c-2",
                               "args": { "task_uuid": uuid, "priority": 1 } }]
            }),
        )
        .await;
        assert_eq!(replay["sync_status"]["c-2"], "ok");
        let t = replay["resources"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == uuid)
            .unwrap();
        assert_eq!(
            t["priority"], 5,
            "replayed command must NOT re-apply (priority stays 5, not 1)"
        );

        // task_edit → set a deadline.
        let edited = post_sync(
            &app,
            &serde_json::json!({
                "sync_token": "*",
                "commands": [{ "type": "task_edit", "uuid": "c-3",
                               "args": { "task_uuid": uuid, "deadline": "2026-09-01" } }]
            }),
        )
        .await;
        assert_eq!(edited["sync_status"]["c-3"], "ok");
        let t = edited["resources"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == uuid)
            .unwrap();
        assert!(t["deadline"].as_str().unwrap().starts_with("2026-09-01"));

        // task_done, then task_reopen → status back to pending.
        post_sync(
            &app,
            &serde_json::json!({
                "sync_token": "*",
                "commands": [{ "type": "task_done", "uuid": "c-4", "args": { "task_uuid": uuid } }]
            }),
        )
        .await;
        let reopened = post_sync(
            &app,
            &serde_json::json!({
                "sync_token": "*",
                "commands": [{ "type": "task_reopen", "uuid": "c-5", "args": { "task_uuid": uuid } }]
            }),
        )
        .await;
        assert_eq!(reopened["sync_status"]["c-5"], "ok");
        let t = reopened["resources"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == uuid)
            .unwrap();
        assert_eq!(t["status"], "pending");
    }

    #[tokio::test]
    async fn task_retext_and_read_routes() {
        let _env = ENV_LOCK.lock().await;
        let db = open_test_db();
        let app = router(AppState { db: db.clone() });

        // Create a ready task with a label so /detail has something to show.
        let created = post_sync(
            &app,
            &serde_json::json!({
                "sync_token": "*",
                "commands": [{ "type": "task_create", "uuid": "k-1", "temp_id": "t1",
                               "args": { "text": "ready task @ops" } }]
            }),
        )
        .await;
        let uuid = created["temp_id_mapping"]["t1"]
            .as_str()
            .unwrap()
            .to_string();

        // task_retext → change the title.
        let retitled = post_sync(
            &app,
            &serde_json::json!({
                "sync_token": "*",
                "commands": [{ "type": "task_retext", "uuid": "k-2",
                               "args": { "task_uuid": uuid, "title": "renamed task" } }]
            }),
        )
        .await;
        assert_eq!(retitled["sync_status"]["k-2"], "ok");
        let t = retitled["resources"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == uuid)
            .unwrap();
        assert_eq!(t["title"], "renamed task");

        // GET /next — the task has no deps, so it is ready.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/next?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            parsed["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["id"] == uuid),
            "the ready task must appear in /next"
        );

        // GET /detail/{uuid} — the @ops label was captured by quick-add.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/detail/{uuid}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let detail: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(detail["labels"][0], "ops");
    }

    #[tokio::test]
    async fn sync_token_is_stable_when_idle() {
        // Guards the cursor-before-delta snapshot order: the returned token
        // must cover the request's own commands, so an idle re-sync with that
        // token yields an empty delta (no perpetual self-redelivery) and the
        // same token back.
        let _env = ENV_LOCK.lock().await;
        let db = open_test_db();
        let app = router(AppState { db: db.clone() });

        let req1 = serde_json::json!({
            "sync_token": "*",
            "commands": [{
                "type": "task_create",
                "uuid": "cmd-idle-1",
                "temp_id": "tmp-idle",
                "args": { "text": "water plants" }
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
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let token = parsed["sync_token"].as_str().unwrap().to_string();

        let req2 = serde_json::json!({ "sync_token": token, "commands": [] });
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/sync")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req2).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            parsed["resources"]["tasks"].as_array().unwrap().len(),
            0,
            "idle re-sync must not re-deliver the client's own events"
        );
        assert_eq!(parsed["sync_token"].as_str().unwrap(), token);
    }

    #[tokio::test]
    async fn sync_delta_sees_local_cli_mutations_and_deletions() {
        // THE acceptance test for event-log unification: a task created via
        // the core function (the CLI/TUI/bot path — no /sync involved) must
        // show up in a remote client's delta, and a local deletion must
        // arrive as a tombstone. Pre-unification both were invisible.
        let _env = ENV_LOCK.lock().await;
        let db = open_test_db();
        let app = router(AppState { db: db.clone() });

        // Seed one event so the baseline cursor is non-zero (a "0" token is
        // the full-sync sentinel and would mask the delta behaviour).
        ptask_core::tasks::create(&db, ptask_core::tasks::NewTask::minimal("seed")).unwrap();

        // Client baseline: grab a cursor.
        let req = serde_json::json!({ "sync_token": "*", "commands": [] });
        let resp = app
            .clone()
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
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let token = parsed["sync_token"].as_str().unwrap().to_string();

        // Local mutations on the canonical host — CLI path, not /sync.
        let kept = ptask_core::tasks::create(&db, ptask_core::tasks::NewTask::minimal("cli kept"))
            .unwrap();
        let doomed =
            ptask_core::tasks::create(&db, ptask_core::tasks::NewTask::minimal("cli doomed"))
                .unwrap();
        ptask_core::tasks::delete_task(&db, &doomed.id).unwrap();

        // Delta sync from the old cursor.
        let req = serde_json::json!({ "sync_token": token, "commands": [] });
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
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let delta_ids: Vec<&str> = parsed["resources"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap())
            .collect();
        assert!(
            delta_ids.contains(&kept.id.as_str()),
            "CLI-created task missing from delta: {:?}",
            delta_ids
        );
        let deleted: Vec<&str> = parsed["deleted_task_uuids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(deleted, vec![doomed.id.as_str()]);
    }

    #[tokio::test]
    async fn sync_full_sync_returns_existing_tasks_without_events() {
        // Gated /sync route; hold ENV_LOCK against concurrent token-setting test.
        let _env = ENV_LOCK.lock().await;
        let db = open_test_db();
        // Raw SQL insert: a task that genuinely predates pt_event_log (every
        // tasks::create now records an event, so simulate the legacy rows
        // directly).
        let existing_id = "legacy-uuid-1".to_string();
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO tasks (id, title, created_at, updated_at)
                 VALUES (?1, 'preexisting', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00')",
                [&existing_id],
            )?;
            Ok(())
        })
        .unwrap();
        let existing = ptask_core::tasks::Task {
            id: existing_id,
            pt_id: None,
            title: "preexisting".into(),
            description: String::new(),
            priority: 2,
            status: "pending".into(),
            created_at: "2026-01-01T00:00:00+00:00".into(),
            updated_at: "2026-01-01T00:00:00+00:00".into(),
            deadline: None,
            source_type: "manual".into(),
            ai_reasoning: String::new(),
        };
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

    #[tokio::test(flavor = "current_thread")]
    async fn sync_requires_api_token_when_configured() {
        let _env = ENV_LOCK.lock().await;
        unsafe {
            std::env::set_var("PTASK_API_TOKEN", "test-token");
        }
        let db = open_test_db();
        let app = router(AppState { db });
        let req = serde_json::json!({"sync_token": "*", "commands": []});

        let resp = app
            .clone()
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
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/sync")
                    .method("POST")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer test-token")
                    .body(Body::from(serde_json::to_vec(&req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        unsafe {
            std::env::remove_var("PTASK_API_TOKEN");
        }
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

    // /metrics is now token-gated (enforce-if-configured), so this test holds
    // ENV_LOCK and asserts the back-compat path: token UNSET → scrape allowed.
    #[tokio::test(flavor = "current_thread")]
    async fn metrics_returns_prometheus_text() {
        let _env = ENV_LOCK.lock().await;
        unsafe {
            std::env::remove_var("PTASK_API_TOKEN");
        }
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

    // PTASK_METRICS_TOKEN: read-only credential for /metrics. Accepted on
    // the read path, rejected on the write path; the write token still
    // covers reads (write ⊇ read).
    #[tokio::test(flavor = "current_thread")]
    async fn metrics_token_is_read_only() {
        let _env = ENV_LOCK.lock().await;
        unsafe {
            std::env::set_var("PTASK_API_TOKEN", "write-secret");
            std::env::set_var("PTASK_METRICS_TOKEN", "scrape-secret");
        }
        let db = open_test_db();
        let app = router(AppState { db });

        // Metrics token → /metrics 200.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .header("authorization", "Bearer scrape-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Write token still works on /metrics.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .header("authorization", "Bearer write-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Metrics token must NOT authorize a write route.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/sync")
                    .method("POST")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer scrape-secret")
                    .body(Body::from(r#"{"sync_token":"*","commands":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        unsafe {
            std::env::remove_var("PTASK_API_TOKEN");
            std::env::remove_var("PTASK_METRICS_TOKEN");
        }
    }

    // env set + missing/wrong credential → 401; env set + correct → 200.
    #[tokio::test(flavor = "current_thread")]
    async fn metrics_requires_api_token_when_configured() {
        let _env = ENV_LOCK.lock().await;
        unsafe {
            std::env::set_var("PTASK_API_TOKEN", "metrics-token");
        }
        let db = open_test_db();
        let app = router(AppState { db });

        // Missing credential → 401.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Wrong credential → 401.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .header("authorization", "Bearer wrong-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Correct bearer credential → 200 + Prometheus body.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .header("authorization", "Bearer metrics-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            std::str::from_utf8(&body)
                .unwrap()
                .contains("pt_tasks_total")
        );

        // The X-PTask-Token header path is accepted too (parity with writes).
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .header("x-ptask-token", "metrics-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        unsafe {
            std::env::remove_var("PTASK_API_TOKEN");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gitea_webhook_closes_pt_n_on_fixes() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let _env = ENV_LOCK.lock().await;
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
                { "id": "abc123", "message": format!("Fixes {}: ship it", pt) },
                { "id": "def456", "message": format!("Closes {}: duplicate in same push", pt) }
            ]
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(b"test-secret").unwrap();
        mac.update(&body_bytes);
        let sig = hex::encode(mac.finalize().into_bytes());
        let app = router(AppState { db: db.clone() });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/webhook/gitea")
                    .method("POST")
                    .header("content-type", "application/json")
                    .header("X-Gitea-Signature", &sig)
                    .body(Body::from(body_bytes.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Duplicate provider delivery should be idempotent: no second
        // status_change row, and the git-originated task event is in
        // pt_event_log so /sync clients can see the change.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/webhook/gitea")
                    .method("POST")
                    .header("content-type", "application/json")
                    .header("X-Gitea-Signature", &sig)
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
            let interactions: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM interactions
                     WHERE task_id=?1 AND action='status_change'",
                    [&t.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(interactions, 1);
            let events: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM pt_event_log
                     WHERE event_type='task.completed' AND task_uuid=?1",
                    [&t.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(events, 1);
            Ok(())
        })
        .unwrap();
        // Cleanup.
        unsafe {
            std::env::remove_var("PTASK_GITEA_WEBHOOK_SECRET");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gitea_webhook_rejects_bad_signature() {
        let _env = ENV_LOCK.lock().await;
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
    async fn email_endpoint_parses_subject_and_body() {
        // Gated /email route; hold ENV_LOCK against concurrent token-setting test.
        let _env = ENV_LOCK.lock().await;
        let db = open_test_db();
        let app = router(AppState { db: db.clone() });
        let raw = "Subject: Buy bread tomorrow\r\n\
From: ops@example.test\r\n\
Message-ID: <abc@example.test>\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Don't forget the sourdough.\r\n";
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/email")
                    .method("POST")
                    .header("content-type", "message/rfc822")
                    .body(Body::from(raw))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["subject"], "Buy bread tomorrow");
        assert!(parsed["id"].as_i64().unwrap() > 0);
        // The raw_items row landed.
        db.with_conn(|c| {
            let n: i64 = c
                .query_row("SELECT COUNT(*) FROM raw_items", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 1);
            let text: String = c
                .query_row("SELECT text FROM raw_items", [], |r| r.get(0))
                .unwrap();
            assert!(text.contains("Buy bread tomorrow"));
            assert!(text.contains("sourdough"));
            Ok(())
        })
        .unwrap();
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
