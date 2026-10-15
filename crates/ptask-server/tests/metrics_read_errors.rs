use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use ptask_server::{AppState, router};
use tower::ServiceExt;

#[tokio::test]
async fn metrics_fails_when_a_gauge_cannot_be_read() {
    for table in ["raw_items", "pt_views", "pt_recurrence", "pt_event_log"] {
        let dir = tempfile::tempdir().unwrap();
        let db = ptask_core::Db::open(dir.path().join("metrics.db")).unwrap();
        db.with_conn(|conn| {
            conn.execute(&format!("DROP TABLE {table}"), [])?;
            Ok(())
        })
        .unwrap();
        let app = router(AppState::new(db, Default::default(), Default::default()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "missing {table} must fail the scrape instead of publishing invented gauges:\n{body}"
        );
        assert!(body.contains("pt_metrics_render_error"), "{body}");
    }
}
