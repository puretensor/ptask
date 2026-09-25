//! Approval inbox HTTP API.
//!
//! GET  /api/approvals              (read)
//! GET  /api/approvals/{id}         (read)
//! POST /api/approvals              (write)
//! POST /api/approvals/{id}/withdraw (write; requester only)
//! POST /api/approvals/{id}/decide  (admin)

use crate::AppState;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use ptask_core::approvals::{
    self, ApprovalError, DecidedVia, Decision, PayloadSource, RequestInput,
};
use ptask_core::event_log::EventCtx;
use ptask_core::tokens::Scope;
use serde::Deserialize;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/approvals", get(list).post(create))
        .route("/api/approvals/{id}", get(get_one))
        .route("/api/approvals/{id}/withdraw", post(withdraw))
        .route("/api/approvals/{id}/decide", post(decide))
}

#[derive(Debug, Deserialize)]
struct ListParams {
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateReq {
    kind: String,
    title: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    payload: Option<String>,
    #[serde(default)]
    payload_json: Option<serde_json::Value>,
    #[serde(default)]
    digest: Option<String>,
    #[serde(default)]
    payload_name: Option<String>,
    #[serde(default)]
    task: Option<String>,
    #[serde(default)]
    expires_in: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DecideReq {
    decision: String,
    #[serde(default)]
    note: Option<String>,
}

fn err_status(e: &ptask_core::Error) -> StatusCode {
    match e {
        ptask_core::Error::Approval(ApprovalError::NotFound(_)) => StatusCode::NOT_FOUND,
        ptask_core::Error::Approval(ApprovalError::Forbidden(_)) => StatusCode::FORBIDDEN,
        ptask_core::Error::Approval(ApprovalError::Conflict(_)) => StatusCode::CONFLICT,
        ptask_core::Error::Approval(ApprovalError::Invalid(_)) => StatusCode::BAD_REQUEST,
        ptask_core::Error::PtIdNotFound(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn err_resp(e: ptask_core::Error) -> Response {
    (
        err_status(&e),
        Json(serde_json::json!({"error": e.to_string()})),
    )
        .into_response()
}

fn ctx_for(actor: &str, source: &str) -> EventCtx {
    EventCtx {
        actor: actor.to_string(),
        source: source.into(),
        event_uuid: None,
    }
}

async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    crate::blocking::db_response(move || {
        if let Some(resp) = crate::auth::require_read_token(&state.db, &state.auth, &headers) {
            return resp;
        }
        let status = params
            .status
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        match approvals::list(&state.db, status) {
            Ok(items) => {
                Json(items.iter().map(|a| a.to_json(None)).collect::<Vec<_>>()).into_response()
            }
            Err(e) => err_resp(e),
        }
    })
    .await
}

async fn get_one(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    crate::blocking::db_response(move || {
        if let Some(resp) = crate::auth::require_read_token(&state.db, &state.auth, &headers) {
            return resp;
        }
        match approvals::get(&state.db, &id) {
            Ok(ap) => {
                let events = approvals::events(&state.db, &ap.uuid).unwrap_or_default();
                Json(ap.to_json(Some(&events))).into_response()
            }
            Err(e) => err_resp(e),
        }
    })
    .await
}

fn payload_from_create(req: &CreateReq) -> ptask_core::Result<PayloadSource> {
    let n =
        req.payload.is_some() as u8 + req.payload_json.is_some() as u8 + req.digest.is_some() as u8;
    if n != 1 {
        return Err(ptask_core::Error::Approval(ApprovalError::Invalid(
            "exactly one of payload, payload_json, digest is required".into(),
        )));
    }
    if let Some(text) = &req.payload {
        let bytes = text.as_bytes().to_vec();
        if bytes.len() > approvals::MAX_PAYLOAD_BYTES {
            return Err(ptask_core::Error::Approval(ApprovalError::Invalid(
                format!(
                    "payload exceeds {} bytes (256 KiB); use --digest for large payloads",
                    approvals::MAX_PAYLOAD_BYTES
                ),
            )));
        }
        return Ok(PayloadSource::File {
            bytes,
            name: req.payload_name.clone(),
            reference: None,
        });
    }
    if let Some(value) = &req.payload_json {
        return approvals::payload_from_json_value(value);
    }
    approvals::payload_from_digest(req.digest.as_deref().unwrap_or(""))
}

async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateReq>,
) -> impl IntoResponse {
    // Token check (a write: it stamps last_used_at) and the insert run on
    // the blocking pool together; only the Telegram ping stays async.
    let st = state.clone();
    let requested = crate::blocking::db_value(move || -> Result<_, Box<Response>> {
        let identity = crate::auth::authenticate(&st.db, &st.auth, &headers, Scope::Write)
            .map_err(Box::new)?;
        let payload = payload_from_create(&req).map_err(|e| Box::new(err_resp(e)))?;
        let input = RequestInput {
            kind: req.kind,
            title: req.title,
            request_note: req.note,
            payload,
            task_pt_id: req.task,
            expires_in: req.expires_in,
        };
        let ctx = ctx_for(&identity.client_id, "api");
        approvals::request(&st.db, input, &ctx).map_err(|e| Box::new(err_resp(e)))
    })
    .await;
    let outcome = match requested {
        Ok(Ok(o)) => o,
        Ok(Err(resp)) => return *resp,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "internal error"})),
            )
                .into_response();
        }
    };
    let created = outcome.created;
    if created {
        let _ = ptask_notify::notify_approval(
            &state.db,
            &state.notify,
            state.dash.url.as_deref(),
            state.tg_approval_buttons,
            &outcome.approval,
        )
        .await;
    }
    let db = state.db.clone();
    let uuid = outcome.approval.uuid.clone();
    let ap = match crate::blocking::db_value(move || approvals::get(&db, &uuid)).await {
        Ok(Ok(ap)) => ap,
        _ => outcome.approval,
    };
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    (status, Json(ap.to_json(None))).into_response()
}

async fn withdraw(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    crate::blocking::db_response(move || {
        let identity =
            match crate::auth::authenticate(&state.db, &state.auth, &headers, Scope::Write) {
                Ok(id) => id,
                Err(resp) => return resp,
            };
        let ctx = ctx_for(&identity.client_id, "api");
        match approvals::withdraw(&state.db, &id, &ctx) {
            Ok(ap) => Json(ap.to_json(None)).into_response(),
            Err(e) => err_resp(e),
        }
    })
    .await
}

async fn decide(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<DecideReq>,
) -> impl IntoResponse {
    crate::blocking::db_response(move || {
        let identity =
            match crate::auth::authenticate(&state.db, &state.auth, &headers, Scope::Admin) {
                Ok(id) => id,
                Err(resp) => return resp,
            };
        let decision = match Decision::parse(&req.decision) {
            Ok(d) => d,
            Err(e) => return err_resp(e.into()),
        };
        let ctx = ctx_for(&identity.client_id, "api");
        match approvals::decide(
            &state.db,
            &id,
            decision,
            DecidedVia::Api,
            req.note.as_deref(),
            &ctx,
        ) {
            Ok(ap) => Json(ap.to_json(None)).into_response(),
            Err(e) => err_resp(e),
        }
    })
    .await
}
