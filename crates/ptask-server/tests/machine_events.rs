use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use ptask_core::event_log::{self, CommandFingerprint, EventCtx};
use ptask_core::tasks::{self as task_ops, NewTask};
use ptask_core::tokens::{self, Scope};
use ptask_server::{AppState, router};
use serde_json::{Value, json};
use tower::ServiceExt;

fn setup() -> (tempfile::TempDir, ptask_core::Db, String, axum::Router) {
    let dir = tempfile::tempdir().unwrap();
    let db = ptask_core::Db::open(dir.path().join("events.db")).unwrap();
    let token = tokens::create(&db, "augmentation-reader", Scope::Read).unwrap();
    let app = router(AppState::new(
        db.clone(),
        Default::default(),
        Default::default(),
    ));
    (dir, db, token, app)
}

fn record(db: &ptask_core::Db, key: &str, kind: &str, payload: Value) {
    event_log::record(
        db,
        key,
        Some("f38f1ca2-09b4-4ca8-8d9d-cde739cc57bf"),
        kind,
        &payload,
        &EventCtx::local("test-worker"),
    )
    .unwrap();
}

async fn get(app: axum::Router, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::builder().uri(uri);
    if let Some(token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let response = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn status_with_header(app: axum::Router, uri: &str, name: &str, value: &str) -> StatusCode {
    let req = Request::builder().uri(uri).header(name, value);
    app.oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn events_require_machine_credentials_even_in_anonymous_development() {
    let (_dir, _db, token, app) = setup();
    assert_eq!(
        get(app.clone(), "/api/events", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(app.clone(), "/api/events", Some("wrong")).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(app.clone(), "/api/events", Some(&token)).await.0,
        StatusCode::OK
    );
    let dir = tempfile::tempdir().unwrap();
    let db = ptask_core::Db::open(dir.path().join("anonymous.db")).unwrap();
    let anonymous = router(AppState::new(db, Default::default(), Default::default()));
    assert_eq!(
        get(anonymous, "/api/events", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let named = tokens::create(&_db, "anonymous", Scope::Read).unwrap();
    assert_eq!(
        get(app, "/api/events", Some(&named)).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn credentials_are_parsed_like_every_other_machine_route() {
    let (_dir, _db, token, app) = setup();
    // The shared parser accepts a lowercase scheme, as /list and /detail do.
    assert_eq!(
        status_with_header(
            app,
            "/api/events",
            "Authorization",
            &format!("bearer {token}")
        )
        .await,
        StatusCode::OK
    );
    // A blank credential is no credential, even where anonymous access is open.
    let dir = tempfile::tempdir().unwrap();
    let db = ptask_core::Db::open(dir.path().join("anonymous.db")).unwrap();
    let anonymous = router(AppState::new(db, Default::default(), Default::default()));
    for (name, value) in [("Authorization", "Bearer   "), ("X-PTask-Token", "  ")] {
        assert_eq!(
            status_with_header(anonymous.clone(), "/api/events", name, value).await,
            StatusCode::UNAUTHORIZED,
            "{name}: {value:?}"
        );
    }
}

#[tokio::test]
async fn empty_is_explicit_and_read_credentials_do_not_allow_mutations() {
    let (_dir, _db, token, app) = setup();
    let (status, body) = get(app.clone(), "/api/events", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"], json!([]));
    assert_eq!(body["count"], 0);
    assert_eq!(body["total"], 0);
    assert_eq!(body["next_cursor"], 0);
    assert_eq!(body["resync"], false);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sync")
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .body(Body::from("{\"commands\":[]}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pages_preserve_snapshot_and_deliver_multiple_events_for_one_task() {
    let (_dir, db, token, app) = setup();
    record(&db, "event-one", "task.created", json!({"priority":3}));
    record(
        &db,
        "event-two",
        "task.updated",
        json!({"status":"in_progress"}),
    );
    let (_, first) = get(app.clone(), "/api/events?limit=1", Some(&token)).await;
    assert_eq!(first["count"], 1);
    assert_eq!(first["total"], 2);
    assert_eq!(first["has_more"], true);
    record(
        &db,
        "event-three",
        "task.completed",
        json!({"status":"done"}),
    );
    let uri = format!(
        "/api/events?cursor={}&snapshot={}&limit=1",
        first["next_cursor"], first["snapshot_cursor"]
    );
    let (_, second) = get(app.clone(), &uri, Some(&token)).await;
    assert_eq!(second["total"], 1);
    assert_eq!(second["has_more"], false);
    assert_eq!(second["items"][0]["event_id"], "event-two");
    assert_eq!(second["items"][0]["task_id"], first["items"][0]["task_id"]);
    let uri = format!("/api/events?cursor={}", second["next_cursor"]);
    let (_, tail) = get(app, &uri, Some(&token)).await;
    assert_eq!(tail["items"][0]["event_id"], "event-three");
}

#[tokio::test]
async fn payload_does_not_export_credentials_prompts_or_arbitrary_nested_text() {
    let (_dir, db, token, app) = setup();
    record(
        &db,
        "safe",
        "task.updated",
        json!({
            "status":"in_progress", "priority":4,
            "description":"secret credential", "prompt":"private request", "token":"sk-ant-example",
            "note":"raw note", "nested":{"text":"password=secret"}, "from":"unsafe token",
        }),
    );
    let (_, body) = get(app, "/api/events", Some(&token)).await;
    let item = &body["items"][0];
    assert_eq!(
        item["payload"],
        json!({"status":"in_progress","priority":4})
    );
    assert_eq!(item["payload_redacted"], true);
    assert_eq!(item["source"], "ptask");
    assert_eq!(item["actor"], "test-worker");
    assert!(!body.to_string().contains("secret"));
    assert!(!body.to_string().contains("private request"));
}

#[tokio::test]
async fn payload_redacted_ignores_the_envelope_and_task_references() {
    let (_dir, db, token, app) = setup();
    // record_in_conn adds actor and source to every payload; a keyed command
    // also carries cmd. None of these is content, so nothing is withheld.
    record(&db, "plain", "task.completed", json!({"status":"done"}));
    let mut keyed = EventCtx::sync("augmentation-writer", "keyed-done");
    keyed.command = Some(CommandFingerprint::new("done", "{}"));
    event_log::record(
        &db,
        "keyed-done",
        Some("f38f1ca2-09b4-4ca8-8d9d-cde739cc57bf"),
        "task.completed",
        &json!({"task_uuid":"f38f1ca2-09b4-4ca8-8d9d-cde739cc57bf","pt_id":"PT-7","status":"dismissed"}),
        &keyed,
    )
    .unwrap();
    record(
        &db,
        "legacy",
        "task.updated",
        json!({"status":"delayed","old_status":"pending"}),
    );
    // A status outside the model, or any other content key, is withheld.
    record(
        &db,
        "unknown",
        "task.updated",
        json!({"status":"cancelled"}),
    );
    record(
        &db,
        "extra",
        "task.updated",
        json!({"status":"todo","woke_from_snooze":true}),
    );
    let (_, body) = get(app, "/api/events", Some(&token)).await;
    let items = body["items"].as_array().unwrap();
    let got: Vec<_> = items
        .iter()
        .map(|i| (i["payload"].clone(), i["payload_redacted"].clone()))
        .collect();
    assert_eq!(
        got,
        vec![
            (json!({"status":"done"}), json!(false)),
            (json!({"status":"dismissed"}), json!(false)),
            (
                json!({"status":"delayed","old_status":"pending"}),
                json!(false)
            ),
            (json!({}), json!(true)),
            (json!({"status":"todo"}), json!(true)),
        ]
    );
}

#[tokio::test]
async fn snooze_transition_exports_its_status() {
    let (_dir, db, token, app) = setup();
    let ctx = EventCtx::local("test-worker");
    let task = task_ops::create(&db, NewTask::minimal("snooze me"), &ctx).unwrap();
    task_ops::snooze(&db, &task.id, "2099-01-01", &ctx).unwrap();
    let (_, body) = get(app, "/api/events", Some(&token)).await;
    let items = body["items"].as_array().unwrap();
    let snooze = items
        .iter()
        .find(|i| i["event_type"] == "task.updated")
        .expect("snooze event exported");
    assert_eq!(snooze["task_id"], json!(task.id));
    assert_eq!(snooze["payload"], json!({"status":"snoozed"}));
    // snoozed_until is content this endpoint withholds.
    assert_eq!(snooze["payload_redacted"], true);
}

#[tokio::test]
async fn invalid_inputs_fail_structurally_and_restore_requires_resync() {
    let (_dir, db, token, app) = setup();
    // Malformed parameters are client errors, never a resync instruction.
    for query in [
        "cursor=-1",
        "limit=0",
        "limit=201",
        "limit=no",
        "scope=all",
        "snapshot=-1",
        "cursor=2&snapshot=1",
    ] {
        let (status, body) = get(app.clone(), &format!("/api/events?{query}"), Some(&token)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert!(body["error"].is_string());
        assert!(body.get("resync").is_none(), "{query}");
    }
    let (status, body) = get(app.clone(), "/api/events?cursor=42", Some(&token)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["resync"], true);
    record(&db, "removed", "task.created", json!({}));
    record(&db, "kept", "task.completed", json!({}));
    db.with_conn(|c| {
        c.execute("DELETE FROM pt_event_log WHERE uuid='removed'", [])?;
        Ok(())
    })
    .unwrap();
    let (_, body) = get(app, "/api/events", Some(&token)).await;
    assert_eq!(body["resync"], true);
    assert_eq!(body["oldest_cursor"], 2);
    assert_eq!(body["items"][0]["event_id"], "kept");
}

#[tokio::test]
async fn missing_prefix_resync_stops_at_the_exact_oldest_cursor_boundary() {
    let (_dir, db, token, app) = setup();
    for key in ["removed", "retained-two", "retained-three"] {
        record(&db, key, "task.updated", json!({}));
    }
    db.with_conn(|c| {
        c.execute("DELETE FROM pt_event_log WHERE id=1", [])?;
        Ok(())
    })
    .unwrap();
    let (_, behind) = get(app.clone(), "/api/events?cursor=0", Some(&token)).await;
    assert_eq!(behind["oldest_cursor"], 2);
    assert_eq!(behind["resync"], true);
    let (status, boundary) = get(app, "/api/events?cursor=1", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(boundary["oldest_cursor"], 2);
    assert_eq!(boundary["resync"], false);
    assert_eq!(boundary["count"], 2);
    assert_eq!(boundary["items"][0]["event_id"], "retained-two");
}

#[tokio::test]
async fn arbitrary_journal_deletion_is_not_a_missing_prefix_certificate() {
    let (_dir, db, token, app) = setup();
    for key in ["first", "interior", "last"] {
        record(&db, key, "task.updated", json!({}));
    }
    db.with_conn(|c| {
        c.execute("DELETE FROM pt_event_log WHERE uuid='interior'", [])?;
        Ok(())
    })
    .unwrap();
    let (status, hole) = get(app.clone(), "/api/events?cursor=1", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hole["oldest_cursor"], 1);
    assert_eq!(hole["resync"], false);
    assert_eq!(hole["count"], 1);
    assert_eq!(hole["total"], 1);
    assert_eq!(hole["next_cursor"], 3);
    assert_eq!(hole["items"][0]["event_id"], "last");

    db.with_conn(|c| {
        c.execute("DELETE FROM pt_event_log", [])?;
        Ok(())
    })
    .unwrap();
    let (status, empty) = get(app.clone(), "/api/events?cursor=0", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["resync"], false);
    assert_eq!(empty["items"], json!([]));
    assert_eq!(empty["count"], 0);
    assert_eq!(empty["total"], 0);
    let (status, ahead) = get(app, "/api/events?cursor=3", Some(&token)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(ahead["resync"], true);
}

#[tokio::test]
async fn snapshot_continuation_and_event_identity_survive_database_reopen() {
    let (dir, db, token, app) = setup();
    for key in ["one", "two", "three"] {
        record(&db, key, "task.updated", json!({}));
    }
    let (_, first) = get(app.clone(), "/api/events?limit=1", Some(&token)).await;
    let continuation = format!(
        "/api/events?cursor={}&snapshot={}",
        first["next_cursor"], first["snapshot_cursor"]
    );
    let path = db.path().to_path_buf();
    drop(app);
    drop(db);
    let reopened = ptask_core::Db::open(path).unwrap();
    record(&reopened, "four", "task.updated", json!({}));
    let app = router(AppState::new(
        reopened,
        Default::default(),
        Default::default(),
    ));
    let (status, rest) = get(app.clone(), &continuation, Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rest["snapshot_cursor"], 3);
    assert_eq!(rest["count"], 2);
    assert_eq!(rest["items"][0]["event_id"], "two");
    assert_eq!(rest["items"][1]["event_id"], "three");
    let (_, repeat) = get(app.clone(), "/api/events?limit=1", Some(&token)).await;
    assert_eq!(
        repeat["items"][0]["event_id"],
        first["items"][0]["event_id"]
    );
    let (_, tail) = get(app, "/api/events?cursor=3", Some(&token)).await;
    assert_eq!(tail["count"], 1);
    assert_eq!(tail["items"][0]["event_id"], "four");
    drop(dir);
}

#[tokio::test]
async fn restoring_a_physical_snapshot_preserves_the_documented_catchup_limit() {
    let (dir, db, token, app) = setup();
    for key in ["one", "two"] {
        record(&db, key, "task.updated", json!({}));
    }
    let backup = dir.path().join("backup.db");
    db.with_conn(|c| {
        c.execute("VACUUM INTO ?1", [backup.to_str().unwrap()])?;
        Ok(())
    })
    .unwrap();
    for key in ["original-three", "original-four"] {
        record(&db, key, "task.updated", json!({}));
    }
    let (_, original) = get(app.clone(), "/api/events?cursor=3", Some(&token)).await;
    assert_eq!(original["next_cursor"], 4);
    assert_eq!(original["items"][0]["event_id"], "original-four");
    drop(app);
    drop(db);
    let restored_path = dir.path().join("restored.db");
    std::fs::copy(backup, &restored_path).unwrap();
    let restored = ptask_core::Db::open(restored_path).unwrap();
    let app = router(AppState::new(
        restored.clone(),
        Default::default(),
        Default::default(),
    ));
    let (status, behind) = get(app.clone(), "/api/events?cursor=4", Some(&token)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(behind["resync"], true);
    for key in ["replacement-three", "replacement-four", "replacement-five"] {
        record(&restored, key, "task.updated", json!({}));
    }
    let (status, caught_up) = get(app.clone(), "/api/events?cursor=4", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(caught_up["resync"], false);
    assert_eq!(caught_up["count"], 1);
    assert_eq!(caught_up["items"][0]["event_id"], "replacement-five");
    let (_, anchor) = get(app, "/api/events?cursor=3&limit=1", Some(&token)).await;
    assert_eq!(anchor["items"][0]["id"], 4);
    assert_eq!(anchor["items"][0]["event_id"], "replacement-four");
    assert_ne!(
        anchor["items"][0]["event_id"],
        original["items"][0]["event_id"]
    );
}

#[tokio::test]
async fn continuation_reauthenticates_after_token_revocation() {
    let (_dir, db, token_a, app) = setup();
    let token_b = tokens::create(&db, "second-reader", Scope::Read).unwrap();
    record(&db, "first", "task.updated", json!({}));
    record(&db, "second", "task.updated", json!({}));
    let (_, first) = get(app.clone(), "/api/events?limit=1", Some(&token_a)).await;
    let continuation = format!(
        "/api/events?cursor={}&snapshot={}",
        first["next_cursor"], first["snapshot_cursor"]
    );
    assert_eq!(tokens::revoke(&db, "augmentation-reader").unwrap(), 1);
    assert_eq!(
        get(app.clone(), &continuation, Some(&token_a)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, second) = get(app.clone(), &continuation, Some(&token_b)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["items"][0]["event_id"], "second");
    assert_eq!(
        status_with_header(app.clone(), &continuation, "X-PTask-Token", &token_b).await,
        StatusCode::OK
    );
    assert_eq!(
        status_with_header(
            app.clone(),
            &continuation,
            "Authorization",
            "Basic dXNlcjpwYXNz"
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri(&continuation)
                .header("Authorization", "Bearer invalid")
                .header("X-PTask-Token", token_b)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
