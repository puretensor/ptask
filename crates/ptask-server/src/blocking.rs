//! Offload blocking SQLite work off the async executor.
//!
//! Every `Db` call is synchronous: `db.get()` parks the calling thread for up
//! to the r2d2 `connection_timeout` (30s) when all 8 pooled connections are
//! checked out, and a write then parks again for up to the SQLite
//! `busy_timeout` (30s) behind another writer. Run straight from an axum
//! handler that is a tokio *worker* thread being parked for up to a minute —
//! on a small node the runtime has as few workers as the pool has
//! connections, so a burst of writers stalls unrelated requests (`/healthz`,
//! reads, timers) that touch no database at all.
//!
//! [`db_response`] moves that work onto tokio's blocking pool, which exists
//! precisely to absorb thread-parking work and grows past the worker count.
//! A panic inside the closure becomes a 500 instead of unwinding a worker.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use tracing::error;

/// Run a fully blocking handler body on the blocking pool.
///
/// The closure owns everything it needs (`AppState` is `Clone`, extractors
/// hand over owned values), so the usual shape is a whole handler body moved
/// verbatim into `db_response(move || { … })`.
pub async fn db_response<F>(f: F) -> Response
where
    F: FnOnce() -> Response + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(resp) => resp,
        Err(e) => {
            error!(target: "ptask::blocking", error = %e, "blocking database task aborted");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "internal error"})),
            )
                .into_response()
        }
    }
}

/// Run blocking work that yields a value rather than a `Response`.
///
/// `Err` is the join failure (the closure panicked); callers map it to their
/// own error shape.
pub async fn db_value<F, T>(f: F) -> std::result::Result<T, tokio::task::JoinError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn db_response_maps_a_panic_to_500_instead_of_unwinding_the_worker() {
        let resp = db_response(|| panic!("boom")).await;
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn db_value_reports_the_join_failure() {
        let out: std::result::Result<(), _> = db_value(|| panic!("boom")).await;
        assert!(out.is_err());
    }
}
