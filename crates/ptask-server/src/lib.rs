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
mod blocking;
mod dedup;
pub mod mcp;
mod routes;
pub mod webhooks;

use anyhow::Result;
use axum::Router;
use ptask_core::Db;
use ptask_core::config::{AuthConfig, DashConfig, WebhookConfig};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub auth: Arc<AuthConfig>,
    pub webhooks: Arc<WebhookConfig>,
    pub dash: Arc<DashConfig>,
}

impl AppState {
    pub fn new(db: Db, auth: AuthConfig, webhooks: WebhookConfig) -> Self {
        Self {
            db,
            auth: Arc::new(auth),
            webhooks: Arc::new(webhooks),
            dash: Arc::new(DashConfig::default()),
        }
    }

    pub fn with_dash(mut self, dash: DashConfig) -> Self {
        self.dash = Arc::new(dash);
        self
    }
}

/// Gate `/mcp` to the **hal** named token (write scope). The MCP mount is
/// HAL's surface by design — per-request identity can't reach rmcp tool
/// handlers, so attribution is pinned to hal and the gate enforces that
/// only hal's credential gets in. Other agents use the scoped REST API.
async fn require_hal_bearer(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let bearer = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let ok = bearer.is_some_and(|tok| {
        ptask_core::tokens::resolve(&state.db, &tok)
            .ok()
            .flatten()
            .is_some_and(|id| id.client_id == "hal" && id.scope >= ptask_core::tokens::Scope::Write)
    });
    if !ok {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "mcp requires the hal token"})),
        )
            .into_response();
    }
    next.run(req).await
}

/// Build the axum Router. Exposed so tests can hit it without binding a port.
pub fn router(state: AppState) -> Router {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    let mcp_db = state.db.clone();
    let mcp_service = StreamableHttpService::new(
        move || Ok(mcp::PtaskMcp::new(mcp_db.clone(), "hal".into())),
        LocalSessionManager::default().into(),
        // rmcp's DNS-rebinding host allowlist defaults to localhost-only,
        // but this mount binds the tailnet IP and sits behind the hal
        // bearer gate — a rebinding page can't present that token, so
        // allow-all (empty) is sound here.
        StreamableHttpServerConfig::default().with_allowed_hosts(Vec::<String>::new()),
    );
    let mcp_router: Router =
        Router::new()
            .fallback_service(mcp_service)
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_hal_bearer,
            ));
    Router::new()
        .merge(routes::base::router())
        .merge(routes::capture::router())
        .merge(routes::dashboard::router())
        .merge(routes::email::router())
        .merge(routes::sync::router())
        .merge(routes::tg::router())
        .merge(routes::read::router())
        .merge(routes::metrics::router())
        .merge(routes::webhook_git::router())
        .with_state(state)
        .nest_service("/mcp", mcp_router)
        .layer(TraceLayer::new_for_http())
}

/// Run the server on `addr` until SIGINT / SIGTERM. Blocks the current task.
/// `auth`/`webhooks` come from the entrypoint's one `Config::from_env()` —
/// the server itself never reads the process environment.
pub async fn serve(
    db: Db,
    addr: SocketAddr,
    auth_cfg: AuthConfig,
    webhook_cfg: WebhookConfig,
    dash_cfg: DashConfig,
) -> Result<()> {
    if let Err(e) = auth::validate_bind_auth(&addr, &auth_cfg, &dash_cfg) {
        anyhow::bail!(e);
    }
    auth::warn_if_unconfigured(&auth_cfg);
    let state = AppState::new(db, auth_cfg, webhook_cfg).with_dash(dash_cfg);
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
    use ptask_core::event_log::EventCtx;
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
        let app = router(AppState::new(db, Default::default(), Default::default()));
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
        let app = router(AppState::new(db, Default::default(), Default::default()));
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
        let app = router(AppState::new(
            db.clone(),
            Default::default(),
            Default::default(),
        ));
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
        let app = router(AppState::new(db, Default::default(), Default::default()));
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
    async fn capture_resolve_reports_database_failures() {
        let db = open_test_db();
        db.with_conn(|conn| {
            conn.execute("ALTER TABLE tasks RENAME TO unavailable_tasks", [])?;
            Ok(())
        })
        .unwrap();
        let app = router(AppState::new(db, Default::default(), Default::default()));
        let body = serde_json::json!({"client_key": "incident-123"});
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/capture/resolve")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn sync_round_trip_create_then_done() {
        let db = open_test_db();
        let app = router(AppState::new(
            db.clone(),
            Default::default(),
            Default::default(),
        ));

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
        let db = open_test_db();
        let app = router(AppState::new(
            db.clone(),
            Default::default(),
            Default::default(),
        ));

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
        // Schema v2: the wire status carries the v2 vocabulary (reopen → todo).
        assert_eq!(t["status"], "todo");
    }

    #[tokio::test]
    async fn task_retext_and_read_routes() {
        let db = open_test_db();
        let app = router(AppState::new(
            db.clone(),
            Default::default(),
            Default::default(),
        ));

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
            .clone()
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

        // GET /resolve — remote clients can resolve one task server-side
        // instead of full-syncing the whole task table.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/resolve?query=renamed%20task")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let resolved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(resolved["task"]["id"], uuid);
    }

    #[tokio::test]
    async fn sync_token_is_stable_when_idle() {
        // Guards the cursor-before-delta snapshot order: the returned token
        // must cover the request's own commands, so an idle re-sync with that
        // token yields an empty delta (no perpetual self-redelivery) and the
        // same token back.
        let db = open_test_db();
        let app = router(AppState::new(
            db.clone(),
            Default::default(),
            Default::default(),
        ));

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
        let db = open_test_db();
        let app = router(AppState::new(
            db.clone(),
            Default::default(),
            Default::default(),
        ));

        // Seed one event so the baseline cursor is non-zero (a "0" token is
        // the full-sync sentinel and would mask the delta behaviour).
        ptask_core::tasks::create(
            &db,
            ptask_core::tasks::NewTask::minimal("seed"),
            &EventCtx::test(),
        )
        .unwrap();

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
        let kept = ptask_core::tasks::create(
            &db,
            ptask_core::tasks::NewTask::minimal("cli kept"),
            &EventCtx::test(),
        )
        .unwrap();
        let doomed = ptask_core::tasks::create(
            &db,
            ptask_core::tasks::NewTask::minimal("cli doomed"),
            &EventCtx::test(),
        )
        .unwrap();
        ptask_core::tasks::delete_task(&db, &doomed.id, &EventCtx::test()).unwrap();

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

        let app = router(AppState::new(db, Default::default(), Default::default()));
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
        let db = open_test_db();
        let auth = AuthConfig {
            api_token: Some("test-token".into()),
            ..Default::default()
        };
        let app = router(AppState::new(db, auth, Default::default()));
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

    // Back-compat path: no token configured → scrape allowed.
    #[tokio::test(flavor = "current_thread")]
    async fn metrics_returns_prometheus_text() {
        let db = open_test_db();
        let app = router(AppState::new(db, Default::default(), Default::default()));
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

    #[tokio::test(flavor = "current_thread")]
    async fn metrics_reports_render_failures_as_server_errors() {
        let db = open_test_db();
        db.with_conn(|conn| {
            conn.execute("ALTER TABLE tasks RENAME TO unavailable_tasks", [])?;
            Ok(())
        })
        .unwrap();
        let app = router(AppState::new(db, Default::default(), Default::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    // PTASK_METRICS_TOKEN: read-only credential for /metrics. Accepted on
    // the read path, rejected on the write path; the write token still
    // covers reads (write ⊇ read).
    #[tokio::test(flavor = "current_thread")]
    async fn metrics_token_is_read_only() {
        let db = open_test_db();
        let auth = AuthConfig {
            api_token: Some("write-secret".into()),
            metrics_token: Some("scrape-secret".into()),
            ..Default::default()
        };
        let app = router(AppState::new(db, auth, Default::default()));

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
    }

    #[tokio::test(flavor = "current_thread")]
    async fn metrics_token_configuration_disables_anonymous_access() {
        let db = open_test_db();
        let auth = AuthConfig {
            metrics_token: Some("scrape-secret".into()),
            ..Default::default()
        };
        let app = router(AppState::new(db, auth, Default::default()));

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

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sync")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"sync_token":"*","commands":[]}"#))
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
                    .header("authorization", "Bearer scrape-secret")
                    .body(Body::from(r#"{"sync_token":"*","commands":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // token set + missing/wrong credential → 401; token set + correct → 200.
    #[tokio::test(flavor = "current_thread")]
    async fn metrics_requires_api_token_when_configured() {
        let db = open_test_db();
        let auth = AuthConfig {
            api_token: Some("metrics-token".into()),
            ..Default::default()
        };
        let app = router(AppState::new(db, auth, Default::default()));

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
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gitea_webhook_closes_pt_n_on_fixes() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let db = open_test_db();
        // Create a task we can close via the magic word.
        let t = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("close me"),
            &EventCtx::test(),
        )
        .unwrap();
        let pt = t.pt_id.clone().unwrap();
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
        let hooks = WebhookConfig {
            gitea_secret: "test-secret".into(),
            ..Default::default()
        };
        let app = router(AppState::new(db.clone(), Default::default(), hooks));
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
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gitea_webhook_rejects_bad_signature() {
        let db = open_test_db();
        let hooks = WebhookConfig {
            gitea_secret: "test-secret".into(),
            ..Default::default()
        };
        let body = serde_json::json!({"ref": "refs/heads/main", "commits": []});
        let app = router(AppState::new(db, Default::default(), hooks));
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
    }

    #[tokio::test]
    async fn email_endpoint_parses_subject_and_body() {
        let db = open_test_db();
        let app = router(AppState::new(
            db.clone(),
            Default::default(),
            Default::default(),
        ));
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
        let app = router(AppState::new(db, Default::default(), Default::default()));
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
    /// PHASE-3 GATE (server side): a named scoped token authenticates, its
    /// client_id becomes the journal actor, and a read-scoped token cannot
    /// write.
    #[tokio::test(flavor = "current_thread")]
    async fn named_token_identity_lands_in_journal() {
        let db = open_test_db();
        let plain =
            ptask_core::tokens::create(&db, "hal", ptask_core::tokens::Scope::Write).unwrap();
        let reader =
            ptask_core::tokens::create(&db, "watcher", ptask_core::tokens::Scope::Read).unwrap();
        let auth = AuthConfig {
            api_token: Some("env-token".into()),
            ..Default::default()
        };
        let app = router(AppState::new(db.clone(), auth, Default::default()));

        let body = serde_json::json!({
            "sync_token": "*",
            "commands": [{
                "type": "task_create",
                "uuid": "cmd-hal-1",
                "temp_id": "tmp-hal",
                "args": {"text": "attributed task"}
            }]
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/sync")
                    .method("POST")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {plain}"))
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let (actor, payload): (String, String) = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT actor, payload FROM pt_event_log WHERE uuid='cmd-hal-1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();
        assert_eq!(actor, "hal", "journal actor is the token's client_id");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["actor"], "hal");
        assert_eq!(v["source"], "sync");

        // Read scope must not authorize /sync.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/sync")
                    .method("POST")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {reader}"))
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({"sync_token":"*","commands":[]}))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// PHASE-7 GATE (server side): the absorbed cockpit surface. Basic auth
    /// gates every /api route (401 without, 200 with); the task list carries
    /// the sidecar's derived fields; and a full browser triage loop
    /// (snooze → reopen → dismiss) lands in the journal as actor=dashboard.
    #[tokio::test(flavor = "current_thread")]
    async fn dashboard_surface_auth_shapes_and_triage_loop() {
        use ptask_core::config::DashConfig;
        let db = open_test_db();
        let t = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("triage me from a browser"),
            &EventCtx::test(),
        )
        .unwrap();
        let dash = DashConfig {
            user: "ops".into(),
            pass: Some("cockpit-pw".into()),
            ..Default::default()
        };
        let app = router(
            AppState::new(db.clone(), Default::default(), Default::default()).with_dash(dash),
        );
        let basic = format!(
            "Basic {}",
            base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                b"ops:cockpit-pw"
            )
        );

        // No credentials → 401 + WWW-Authenticate.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(resp.headers().contains_key("www-authenticate"));

        // Credentials → 200 + the sidecar's stats shape.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .header("authorization", &basic)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let stats: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(stats["pending_total"].as_i64().unwrap() >= 1);
        assert!(stats["by_priority"].is_object());
        assert!(stats["by_status"].is_object());

        // Task list carries pt_id + derived age_days.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/tasks?status=pending&limit=10")
                    .header("authorization", &basic)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let tasks: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let row = tasks["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == t.id.as_str())
            .expect("created task appears in /api/tasks");
        assert_eq!(row["pt_id"], t.pt_id.clone().unwrap().as_str());
        assert!(row["age_days"].is_number());

        // Triage loop: snooze → reopen → dismiss (verbs the cockpit never had).
        for (verb, body_json) in [
            ("snooze", serde_json::json!({"days": 2})),
            ("dismiss", serde_json::json!({})),
            ("reopen", serde_json::json!({})),
            ("dismiss", serde_json::json!({})),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/tasks/{}/{}", t.id, verb))
                        .method("POST")
                        .header("content-type", "application/json")
                        .header("authorization", &basic)
                        .body(Body::from(serde_json::to_vec(&body_json).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "verb {} must succeed", verb);
        }
        db.with_conn(|c| {
            let status: String = c
                .query_row("SELECT status_v2 FROM tasks WHERE id=?1", [&t.id], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(status, "dismissed");
            let dash_events: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM pt_event_log WHERE task_uuid=?1 AND actor='dashboard'",
                    [&t.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(dash_events >= 3, "triage verbs journaled as dashboard");
            Ok(())
        })
        .unwrap();

        // Per-task journal endpoint for the detail drawer.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/tasks/{}/events", t.id))
                    .header("authorization", &basic)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let hist: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(hist["events"].as_array().unwrap().len() >= 3);

        // Browser create with quick-add tokens via explicit fields.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/tasks")
                    .method("POST")
                    .header("content-type", "application/json")
                    .header("authorization", &basic)
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "title": "browser-created task @web",
                            "priority": 4
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(created["ok"].as_bool().unwrap());
        assert!(created["pt_id"].as_str().unwrap().starts_with("PT-"));
    }

    /// /api/stats flux: per-window added split by INTENT — `human` =
    /// operator-typed OR Claude-Code-on-request (manual/mcp/…); `robot` =
    /// autonomously generated (distilled/incident/…). Done counted by
    /// updated_at recency. The wider 7d window must pull in an older task the
    /// 24h window excludes.
    #[tokio::test(flavor = "current_thread")]
    async fn dashboard_stats_reports_windowed_flux() {
        let db = open_test_db();
        // Fresh: 2 human (manual, mcp-on-request) + 1 robot (distilled).
        for (title, source) in [
            ("operator task", "manual"),
            ("hal-on-request task", "mcp"),
            ("distiller task", "distilled"),
        ] {
            let mut new = ptask_core::NewTask::minimal(title);
            new.source_type = source.into();
            ptask_core::tasks::create(&db, new, &EventCtx::test()).unwrap();
        }
        // A ROBOT task added 3 days ago: inside the 7d window, not the 24h.
        let mid = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("incident 3d ago"),
            &EventCtx::test(),
        )
        .unwrap();
        // An old task completed just now: counts in done for every window.
        let old = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("old but just done"),
            &EventCtx::test(),
        )
        .unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET created_at = datetime('now','-3 days'),
                        source_type = 'incident' WHERE id = ?1",
                rusqlite::params![mid.id],
            )?;
            c.execute(
                "UPDATE tasks SET created_at = datetime('now','-10 days'),
                        status = 'done' WHERE id = ?1",
                rusqlite::params![old.id],
            )?;
            Ok(())
        })
        .unwrap();

        let app = router(AppState::new(db, Default::default(), Default::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let stats: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let flux = &stats["flux"];
        assert_eq!(
            flux["windows"],
            serde_json::json!(["30m", "1h", "6h", "24h", "7d"])
        );
        let w24 = &flux["by_window"]["24h"];
        assert_eq!(w24["added"], 3); // the three fresh tasks (mid is 3d old)
        assert_eq!(w24["added_human"], 2); // manual + mcp
        assert_eq!(w24["added_robot"], 1); // distilled
        assert_eq!(w24["done"], 1); // only `old`, updated just now
        let w7 = &flux["by_window"]["7d"];
        assert_eq!(w7["added"], 4); // + the 3-day-old incident task
        assert_eq!(w7["added_human"], 2); // unchanged
        assert_eq!(w7["added_robot"], 2); // distilled + incident
        assert_eq!(w7["done"], 1);
    }

    /// The Recently Added rail's contract: order=created returns newest-first
    /// across ALL statuses (a task generated and closed ten minutes ago is
    /// still recent activity), and unknown order keys 400 before touching SQL.
    #[tokio::test(flavor = "current_thread")]
    async fn dashboard_tasks_order_created_newest_first() {
        let db = open_test_db();
        let old = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("old and already done"),
            &EventCtx::test(),
        )
        .unwrap();
        let fresh = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("fresh arrival"),
            &EventCtx::test(),
        )
        .unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET created_at = datetime('now','-2 days'),
                        status = 'done' WHERE id = ?1",
                rusqlite::params![old.id],
            )?;
            c.execute(
                "UPDATE tasks SET project = 'corp-tax' WHERE id = ?1",
                rusqlite::params![fresh.id],
            )?;
            Ok(())
        })
        .unwrap();
        ptask_core::tasks::modify_labels(
            &db,
            &fresh.id,
            &["domain:mgmt".into(), "finance".into()],
            &[],
            &EventCtx::test(),
        )
        .unwrap();

        let app = router(AppState::new(db, Default::default(), Default::default()));
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/tasks?status=all&order=created&limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let ids: Vec<&str> = v["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec![fresh.id.as_str(), old.id.as_str()]);
        assert_eq!(v["tasks"][1]["status"], "done");
        // v3.6 domain-classifier fields: project on the wire, labels as a
        // real JSON array ([] when the task has no label rows).
        assert_eq!(v["tasks"][0]["project"], "corp-tax");
        let mut labels: Vec<&str> = v["tasks"][0]["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap())
            .collect();
        labels.sort_unstable();
        assert_eq!(labels, vec!["domain:mgmt", "finance"]);
        assert_eq!(v["tasks"][1]["labels"], serde_json::json!([]));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/tasks?order=neglect")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Cross-origin writes must be rejected even with valid Basic creds —
    /// browsers attach cached credentials cross-origin (CSRF).
    #[tokio::test(flavor = "current_thread")]
    async fn dashboard_rejects_cross_origin_writes() {
        use ptask_core::config::DashConfig;
        let db = open_test_db();
        let t = ptask_core::tasks::create(
            &db,
            ptask_core::NewTask::minimal("csrf target"),
            &EventCtx::test(),
        )
        .unwrap();
        let dash = DashConfig {
            user: "ops".into(),
            pass: Some("pw".into()),
            ..Default::default()
        };
        let app = router(AppState::new(db, Default::default(), Default::default()).with_dash(dash));
        let basic = format!(
            "Basic {}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"ops:pw")
        );
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/tasks/{}/done", t.id))
                    .method("POST")
                    .header("content-type", "application/json")
                    .header("authorization", &basic)
                    .header("host", "ptask.puretensor.ai")
                    .header("origin", "https://evil.example")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // Matching origin passes.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/tasks/{}/done", t.id))
                    .method("POST")
                    .header("content-type", "application/json")
                    .header("authorization", &basic)
                    .header("host", "ptask.puretensor.ai")
                    .header("origin", "https://ptask.puretensor.ai")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
