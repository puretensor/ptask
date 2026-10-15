use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use ptask_core::Db;
use ptask_server::{AppState, router};
use tower::ServiceExt;

fn record(db: &Db, event_type: &str, ts: &str) {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO pt_event_log (uuid, ts, event_type, payload) VALUES (?1, ?2, ?3, '{}')",
            rusqlite::params![format!("{event_type}:{ts}"), ts, event_type],
        )?;
        Ok(())
    })
    .unwrap();
}

async fn last_run_ok(app: &axum::Router) -> i64 {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("pt_distill_last_run_ok "))
        .expect("distill health gauge")
        .parse()
        .unwrap()
}

#[tokio::test]
async fn latest_failure_in_the_same_second_is_not_reported_as_success() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("metrics.db")).unwrap();
    let app = router(AppState::new(
        db.clone(),
        Default::default(),
        Default::default(),
    ));
    assert_eq!(last_run_ok(&app).await, 1, "no recorded failures");
    record(&db, "distill.run", "2026-10-01T12:00:00.100000Z");
    record(&db, "distill.failed", "2026-10-01T12:00:00.200000Z");
    assert_eq!(
        last_run_ok(&app).await,
        0,
        "a failure after a success in the same second must trip the health gauge"
    );
    record(&db, "distill.run", "2026-10-01T12:00:00.300000Z");
    assert_eq!(last_run_ok(&app).await, 1, "a later recovery clears it");
}
