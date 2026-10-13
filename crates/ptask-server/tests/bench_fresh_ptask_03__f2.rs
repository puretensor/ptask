//! The cockpit's title bound is "3-400 chars" (error text, the page's
//! `maxlength=400`, and the Python sidecar's `len(str)`), so `pt serve` must
//! count characters, not UTF-8 bytes, on both create and edit.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use ptask_server::{AppState, router};
use tower::ServiceExt;

fn app() -> (axum::Router, AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(
        ptask_core::Db::open(dir.path().join("test.db")).unwrap(),
        Default::default(),
        Default::default(),
    );
    (router(state.clone()), state, dir)
}

async fn post(app: &axum::Router, uri: &str, body: serde_json::Value) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// (label, title, accepted) — bounds in characters, no quick-add tokens.
fn cases() -> Vec<(&'static str, String, bool)> {
    let mixed: String = "þ".repeat(209) + "a";
    assert!(
        mixed.chars().count() == 210 && mixed.len() == 419,
        "fixture must straddle the byte bound"
    );
    vec![
        ("2 chars / 4 bytes", "ðþ".to_string(), false),
        ("3 chars / 6 bytes", "ðþá".to_string(), true),
        ("210 chars / >400 bytes", mixed, true),
        ("400 chars / 800 bytes", "þ".repeat(400), true),
        ("401 chars / 802 bytes", "þ".repeat(401), false),
    ]
}

#[tokio::test]
async fn create_counts_title_length_in_chars_not_bytes() {
    let mut wrong = Vec::new();
    for (label, title, accepted) in cases() {
        let (app, _state, _guard) = app();
        let (status, body) = post(&app, "/api/tasks", serde_json::json!({ "title": title })).await;
        let want = if accepted { StatusCode::OK } else { StatusCode::BAD_REQUEST };
        if status != want {
            wrong.push(format!("create {label}: got {status}, want {want}: {body}"));
        }
    }
    assert!(wrong.is_empty(), "title length must count chars:\n{}", wrong.join("\n"));
}

#[tokio::test]
async fn edit_counts_title_length_in_chars_not_bytes() {
    let mut wrong = Vec::new();
    for (label, title, accepted) in cases() {
        let (app, state, _guard) = app();
        let task = ptask_core::tasks::create(
            &state.db,
            ptask_core::NewTask::minimal("original title"),
            &ptask_core::event_log::EventCtx::test(),
        )
        .unwrap();
        let (status, body) = post(
            &app,
            &format!("/api/tasks/{}/edit", task.id),
            serde_json::json!({ "title": title }),
        )
        .await;
        let want = if accepted { StatusCode::OK } else { StatusCode::BAD_REQUEST };
        if status != want {
            wrong.push(format!("edit {label}: got {status}, want {want}: {body}"));
        }
    }
    assert!(wrong.is_empty(), "title length must count chars:\n{}", wrong.join("\n"));
}
