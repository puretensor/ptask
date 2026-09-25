//! Operator approval inbox.
//!
//! Agents request; only the operator decides. An approval binds to an exact
//! payload that pTask stores and hashes itself. Executors consume an approved
//! payload exactly once.

use crate::dates;
use crate::error::{Error, Result};
use crate::event_log::EventCtx;
use crate::storage::Db;
use rusqlite::OptionalExtension;
use rusqlite::params;
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Stored payload must fit in SQLite and in the operator's preview. Larger
/// artefacts are referenced by digest only.
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;

const KINDS: &[&str] = &[
    "email", "ebay", "spend", "destroy", "external", "budget", "other",
];
const STATUSES: &[&str] = &["pending", "approved", "rejected", "withdrawn", "expired"];
const TERMINAL: &[&str] = &["rejected", "withdrawn", "expired"];

/// Domain errors for the approval inbox. CLI maps a subset to process exit
/// codes 3..=6; everything else is a generic failure.
#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    #[error("approval not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Conflict(String),
    #[error("approval is pending")]
    Pending,
    #[error("approval is {0}")]
    Terminal(String),
    #[error("payload digest does not match")]
    DigestMismatch,
    #[error("approval already consumed")]
    AlreadyConsumed,
}

impl ApprovalError {
    /// CLI verify/consume exit code, if this error is one of the gated
    /// outcomes. `None` means the caller should use the generic exit 1.
    pub fn verify_exit_code(&self) -> Option<i32> {
        match self {
            Self::Pending => Some(3),
            Self::Terminal(_) => Some(4),
            Self::DigestMismatch => Some(5),
            Self::AlreadyConsumed => Some(6),
            _ => None,
        }
    }
}

/// How the payload was supplied at request time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    File,
    Json,
}

impl PayloadKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Json => "json",
        }
    }
}

/// One of the three mutually exclusive payload sources.
#[derive(Debug, Clone)]
pub enum PayloadSource {
    File {
        bytes: Vec<u8>,
        name: Option<String>,
        reference: Option<String>,
    },
    Json(Vec<u8>),
    Digest(String),
}

/// Fields an agent (or HTTP/MCP caller) supplies when requesting approval.
#[derive(Debug, Clone)]
pub struct RequestInput {
    pub kind: String,
    pub title: String,
    pub request_note: Option<String>,
    pub payload: PayloadSource,
    pub task_pt_id: Option<String>,
    pub expires_in: Option<String>,
}

/// Approve or reject a pending request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Reject,
}

impl Decision {
    pub fn parse(s: &str) -> std::result::Result<Self, ApprovalError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "approve" => Ok(Self::Approve),
            "reject" => Ok(Self::Reject),
            other => Err(ApprovalError::Invalid(format!(
                "decision must be approve or reject, got {other:?}"
            ))),
        }
    }

    fn status(self) -> &'static str {
        match self {
            Self::Approve => "approved",
            Self::Reject => "rejected",
        }
    }

    fn event_type(self) -> &'static str {
        match self {
            Self::Approve => "approval.approved",
            Self::Reject => "approval.rejected",
        }
    }
}

/// Surface that recorded a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecidedVia {
    Cli,
    Dashboard,
    Telegram,
    Api,
}

impl DecidedVia {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Dashboard => "dashboard",
            Self::Telegram => "telegram",
            Self::Api => "api",
        }
    }
}

/// One attributed journal entry attached to an approval.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub actor: Option<String>,
    pub at: String,
}

/// Request + decide + consume state for one inbox row.
#[derive(Debug, Clone)]
pub struct Approval {
    pub uuid: String,
    pub seq: i64,
    pub kind: String,
    pub title: String,
    pub request_note: Option<String>,
    pub payload: Option<Vec<u8>>,
    pub payload_kind: Option<String>,
    pub payload_name: Option<String>,
    pub payload_bytes: Option<i64>,
    pub payload_ref: Option<String>,
    pub digest: String,
    pub requester: String,
    pub task_uuid: Option<String>,
    pub task_pt_id: Option<String>,
    pub status: String,
    pub decided_by: Option<String>,
    pub decided_via: Option<String>,
    pub decision_note: Option<String>,
    pub created_at: String,
    pub decided_at: Option<String>,
    pub expires_at: Option<String>,
    pub notified_at: Option<String>,
    pub consumed_at: Option<String>,
    pub consumed_by: Option<String>,
}

impl Approval {
    pub fn ap_id(&self) -> String {
        format_ap_id(self.seq)
    }

    pub fn payload_stored(&self) -> bool {
        self.payload.is_some()
    }

    pub fn preview(&self) -> String {
        render_preview(
            self.payload.as_deref(),
            self.payload_kind.as_deref(),
            self.payload_stored(),
        )
    }

    /// Machine object matching the contract JSON shape. `events` is set
    /// only for `show`.
    pub fn to_json(&self, events: Option<&[ApprovalEvent]>) -> serde_json::Value {
        let mut v = serde_json::json!({
            "id": self.ap_id(),
            "uuid": self.uuid,
            "kind": self.kind,
            "title": self.title,
            "request_note": self.request_note,
            "preview": self.preview(),
            "payload_stored": self.payload_stored(),
            "payload_kind": self.payload_kind,
            "payload_name": self.payload_name,
            "payload_bytes": self.payload_bytes,
            "payload_ref": self.payload_ref,
            "digest": self.digest,
            "requester": self.requester,
            "task": self.task_pt_id,
            "status": self.status,
            "decided_by": self.decided_by,
            "decided_via": self.decided_via,
            "decision_note": self.decision_note,
            "created_at": self.created_at,
            "decided_at": self.decided_at,
            "expires_at": self.expires_at,
            "notified_at": self.notified_at,
            "consumed_at": self.consumed_at,
            "consumed_by": self.consumed_by,
        });
        if let Some(events) = events {
            v["events"] = serde_json::to_value(events).unwrap_or(serde_json::Value::Array(vec![]));
        }
        v
    }
}

/// Outcome of [`request`]: `created` is false on an idempotent re-request
/// of a still-pending digest.
#[derive(Debug, Clone)]
pub struct RequestOutcome {
    pub approval: Approval,
    pub created: bool,
}

pub fn format_ap_id(seq: i64) -> String {
    format!("AP-{seq}")
}

pub fn parse_ap_id(s: &str) -> Option<i64> {
    let rest = s
        .trim()
        .strip_prefix("AP-")
        .or_else(|| s.trim().strip_prefix("ap-"))?;
    rest.parse::<i64>().ok().filter(|n| *n > 0)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

pub fn is_digest_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Canonical JSON bytes: recursively sorted keys, compact separators,
/// non-ASCII kept as UTF-8. Matches Python
/// `json.dumps(..., sort_keys=True, separators=(",", ":"), ensure_ascii=False)`.
pub fn canonicalize_json(value: &serde_json::Value) -> Result<Vec<u8>> {
    serde_json::to_vec(&sort_json(value))
        .map_err(|e| Error::Approval(ApprovalError::Invalid(format!("canonical JSON: {e}"))))
}

pub fn parse_json_payload(raw: &str) -> Result<Vec<u8>> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|e| {
        Error::Approval(ApprovalError::Invalid(format!("invalid JSON payload: {e}")))
    })?;
    canonicalize_json(&value)
}

fn sort_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), sort_json(&map[k]));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(sort_json).collect())
        }
        other => other.clone(),
    }
}

pub fn render_preview(payload: Option<&[u8]>, kind: Option<&str>, stored: bool) -> String {
    if !stored {
        return "Payload is not stored; only the digest was recorded.".into();
    }
    let Some(bytes) = payload else {
        return "Payload is not stored; only the digest was recorded.".into();
    };
    if kind == Some("json") {
        match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(v) => serde_json::to_string_pretty(&v)
                .unwrap_or_else(|_| String::from_utf8_lossy(bytes).into_owned()),
            Err(_) => match std::str::from_utf8(bytes) {
                Ok(s) => s.to_string(),
                Err(_) => format!("<binary {} bytes>", bytes.len()),
            },
        }
    } else {
        match std::str::from_utf8(bytes) {
            Ok(s) => s.to_string(),
            Err(_) => format!("<binary {} bytes>", bytes.len()),
        }
    }
}

pub fn parse_expires_in(spec: &str) -> Result<String> {
    let spec = spec.trim();
    let invalid = || {
        Error::Approval(ApprovalError::Invalid(format!(
            "invalid --expires-in {spec:?}; use <n>{{s|m|h|d}}"
        )))
    };
    // Split on the last char, not the last byte: "5日" must be an error,
    // not a panic on a non-char-boundary split.
    let unit = spec.chars().next_back().ok_or_else(invalid)?;
    let n: i64 = spec[..spec.len() - unit.len_utf8()]
        .parse()
        .map_err(|_| invalid())?;
    if n < 0 {
        return Err(Error::Approval(ApprovalError::Invalid(
            "--expires-in must be non-negative".into(),
        )));
    }
    // The infallible Span setters panic past jiff's unit bounds
    // ("99999999d" from an HTTP or MCP caller).
    let span = match unit {
        's' => jiff::Span::new().try_seconds(n),
        'm' => jiff::Span::new().try_minutes(n),
        'h' => jiff::Span::new().try_hours(n),
        'd' => jiff::Span::new().try_days(n),
        _ => return Err(invalid()),
    }
    .map_err(|e| {
        Error::Approval(ApprovalError::Invalid(format!(
            "--expires-in out of range: {e}"
        )))
    })?;
    let now = dates::now_in_operator_tz()?;
    let until = now.checked_add(span).map_err(|e| {
        Error::Approval(ApprovalError::Invalid(format!("expires-in overflow: {e}")))
    })?;
    Ok(dates::format_iso(&until))
}

fn validate_kind(kind: &str) -> Result<()> {
    if KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(Error::Approval(ApprovalError::Invalid(format!(
            "invalid kind {kind:?}; expected one of {}",
            KINDS.join(", ")
        ))))
    }
}

fn validate_status_filter(status: &str) -> Result<()> {
    if status == "all" || STATUSES.contains(&status) {
        Ok(())
    } else {
        Err(Error::Approval(ApprovalError::Invalid(format!(
            "invalid status {status:?}"
        ))))
    }
}

fn local_event_uuid(ctx: &EventCtx) -> String {
    ctx.event_uuid
        .clone()
        .unwrap_or_else(|| format!("local:{}", Uuid::new_v4()))
}

fn record_event(
    tx: &rusqlite::Connection,
    ctx: &EventCtx,
    approval_uuid: &str,
    event_type: &str,
    extra: serde_json::Value,
) -> Result<()> {
    let uuid = local_event_uuid(ctx);
    crate::event_log::record_in_conn(tx, &uuid, Some(approval_uuid), event_type, &extra, ctx)
        .map(|_| ())
}

fn is_unique_constraint(err: &Error) -> bool {
    match err {
        Error::Sqlite(e) => matches!(
            e.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation)
        ),
        _ => false,
    }
}

const SELECT_SQL: &str = "SELECT a.id, a.seq, a.kind, a.title, a.request_note,
       a.payload, a.payload_kind, a.payload_name, a.payload_bytes, a.payload_ref,
       a.digest, a.requester, a.task_uuid, t.pt_id, a.status,
       a.decided_by, a.decided_via, a.decision_note,
       a.created_at, a.decided_at, a.expires_at, a.notified_at,
       a.consumed_at, a.consumed_by
FROM approvals a
LEFT JOIN tasks t ON t.id = a.task_uuid";

fn map_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Approval> {
    Ok(Approval {
        uuid: r.get(0)?,
        seq: r.get(1)?,
        kind: r.get(2)?,
        title: r.get(3)?,
        request_note: r.get(4)?,
        payload: r.get(5)?,
        payload_kind: r.get(6)?,
        payload_name: r.get(7)?,
        payload_bytes: r.get(8)?,
        payload_ref: r.get(9)?,
        digest: r.get(10)?,
        requester: r.get(11)?,
        task_uuid: r.get(12)?,
        task_pt_id: r.get(13)?,
        status: r.get(14)?,
        decided_by: r.get(15)?,
        decided_via: r.get(16)?,
        decision_note: r.get(17)?,
        created_at: r.get(18)?,
        decided_at: r.get(19)?,
        expires_at: r.get(20)?,
        notified_at: r.get(21)?,
        consumed_at: r.get(22)?,
        consumed_by: r.get(23)?,
    })
}

fn get_pending_by_digest(db: &Db, digest: &str) -> Result<Option<Approval>> {
    let conn = db.get()?;
    let found = conn
        .query_row(
            &format!("{SELECT_SQL} WHERE a.digest = ?1 AND a.status = 'pending'"),
            [digest],
            map_row,
        )
        .optional()?;
    Ok(found)
}

fn load_by_uuid_conn(conn: &rusqlite::Connection, uuid: &str) -> Result<Option<Approval>> {
    Ok(conn
        .query_row(&format!("{SELECT_SQL} WHERE a.id = ?1"), [uuid], map_row)
        .optional()?)
}

fn load_by_seq_conn(conn: &rusqlite::Connection, seq: i64) -> Result<Option<Approval>> {
    Ok(conn
        .query_row(&format!("{SELECT_SQL} WHERE a.seq = ?1"), [seq], map_row)
        .optional()?)
}

/// Resolve `AP-n` or the row uuid.
pub fn get(db: &Db, id: &str) -> Result<Approval> {
    let conn = db.get()?;
    get_in_conn(&conn, id)
}

fn get_in_conn(conn: &rusqlite::Connection, id: &str) -> Result<Approval> {
    let found = if let Some(seq) = parse_ap_id(id) {
        load_by_seq_conn(conn, seq)?
    } else {
        load_by_uuid_conn(conn, id.trim())?
    };
    found.ok_or_else(|| Error::Approval(ApprovalError::NotFound(id.trim().to_string())))
}

pub fn list(db: &Db, status: Option<&str>) -> Result<Vec<Approval>> {
    let status = status.unwrap_or("pending");
    validate_status_filter(status)?;
    let conn = db.get()?;
    if status == "all" {
        let mut stmt = conn.prepare(&format!("{SELECT_SQL} ORDER BY a.seq ASC"))?;
        let rows = stmt.query_map([], map_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    } else {
        let mut stmt = conn.prepare(&format!(
            "{SELECT_SQL} WHERE a.status = ?1 ORDER BY a.seq ASC"
        ))?;
        let rows = stmt.query_map([status], map_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

pub fn payload_bytes(db: &Db, id: &str) -> Result<Vec<u8>> {
    let ap = get(db, id)?;
    match ap.payload {
        Some(bytes) => Ok(bytes),
        None => Err(Error::Approval(ApprovalError::Invalid(format!(
            "{} has no stored payload (digest-only request)",
            ap.ap_id()
        )))),
    }
}

pub fn events(db: &Db, approval_uuid: &str) -> Result<Vec<ApprovalEvent>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT event_type, actor, ts FROM pt_event_log
         WHERE task_uuid = ?1 AND event_type LIKE 'approval.%'
         ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([approval_uuid], |r| {
        Ok(ApprovalEvent {
            event_type: r.get(0)?,
            actor: r.get(1)?,
            at: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

struct ResolvedPayload {
    bytes: Option<Vec<u8>>,
    kind: Option<String>,
    name: Option<String>,
    reference: Option<String>,
    len: Option<i64>,
    digest: String,
}

fn resolve_payload(src: &PayloadSource) -> Result<ResolvedPayload> {
    match src {
        PayloadSource::File {
            bytes,
            name,
            reference,
        } => {
            if bytes.len() > MAX_PAYLOAD_BYTES {
                return Err(Error::Approval(ApprovalError::Invalid(format!(
                    "payload exceeds {MAX_PAYLOAD_BYTES} bytes (256 KiB); use --digest for large payloads"
                ))));
            }
            Ok(ResolvedPayload {
                bytes: Some(bytes.clone()),
                kind: Some(PayloadKind::File.as_str().to_string()),
                name: name.clone(),
                reference: reference.clone(),
                len: Some(bytes.len() as i64),
                digest: sha256_hex(bytes),
            })
        }
        PayloadSource::Json(bytes) => {
            if bytes.len() > MAX_PAYLOAD_BYTES {
                return Err(Error::Approval(ApprovalError::Invalid(format!(
                    "payload exceeds {MAX_PAYLOAD_BYTES} bytes (256 KiB); use --digest for large payloads"
                ))));
            }
            Ok(ResolvedPayload {
                bytes: Some(bytes.clone()),
                kind: Some(PayloadKind::Json.as_str().to_string()),
                name: None,
                reference: None,
                len: Some(bytes.len() as i64),
                digest: sha256_hex(bytes),
            })
        }
        PayloadSource::Digest(h) => {
            let h = h.trim();
            if !is_digest_hex(h) {
                return Err(Error::Approval(ApprovalError::Invalid(
                    "digest must be 64 lowercase hex characters".into(),
                )));
            }
            Ok(ResolvedPayload {
                bytes: None,
                kind: None,
                name: None,
                reference: None,
                len: None,
                digest: h.to_string(),
            })
        }
    }
}

/// Insert a new approval, or return the existing pending row with the same
/// digest (idempotent re-request).
pub fn request(db: &Db, input: RequestInput, ctx: &EventCtx) -> Result<RequestOutcome> {
    validate_kind(&input.kind)?;
    let title = input.title.trim();
    if title.is_empty() {
        return Err(Error::Approval(ApprovalError::Invalid(
            "title must not be empty".into(),
        )));
    }
    let requester = ctx.actor.trim();
    if requester.is_empty() {
        return Err(Error::Approval(ApprovalError::Invalid(
            "requester (actor) must not be empty".into(),
        )));
    }
    let resolved = resolve_payload(&input.payload)?;
    let digest = resolved.digest.clone();
    let payload = resolved.bytes;
    let payload_kind = resolved.kind;
    let payload_name = resolved.name;
    let payload_ref = resolved.reference;
    let payload_bytes = resolved.len;
    let expires_at = match input.expires_in.as_deref() {
        Some(s) if !s.trim().is_empty() => Some(parse_expires_in(s)?),
        _ => None,
    };
    let task_uuid = match input.task_pt_id.as_deref() {
        Some(pt) if !pt.trim().is_empty() => {
            let conn = db.get()?;
            Some(
                crate::pt_id::lookup_uuid(&conn, pt.trim()).map_err(|e| match e {
                    Error::PtIdNotFound(id) => {
                        Error::Approval(ApprovalError::Invalid(format!("task not found: {id}")))
                    }
                    other => other,
                })?,
            )
        }
        _ => None,
    };
    let note = input
        .request_note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // A pending row past its expiry must not satisfy the digest dedupe:
    // the caller would get back a request the operator can no longer
    // decide, and the partial unique index would refuse a fresh one.
    expire(db, &EventCtx::system("approvals"))?;
    if let Some(existing) = get_pending_by_digest(db, &digest)? {
        return Ok(RequestOutcome {
            approval: existing,
            created: false,
        });
    }

    let now = dates::format_iso(&dates::now_in_operator_tz()?);
    let uuid = Uuid::new_v4().to_string();
    let insert = (|| {
        let mut conn = db.get()?;
        let tx = conn.transaction()?;
        let seq: i64 = tx.query_row(
            "UPDATE pt_counters SET value = value + 1 WHERE name='approval_id' RETURNING value",
            [],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO approvals (
                id, seq, kind, title, request_note,
                payload, payload_kind, payload_name, payload_bytes, payload_ref,
                digest, requester, task_uuid, status, created_at, expires_at
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, 'pending', ?14, ?15
             )",
            params![
                uuid,
                seq,
                input.kind,
                title,
                note,
                payload,
                payload_kind,
                payload_name,
                payload_bytes,
                payload_ref,
                digest,
                requester,
                task_uuid,
                now,
                expires_at,
            ],
        )?;
        record_event(
            &tx,
            ctx,
            &uuid,
            "approval.requested",
            serde_json::json!({"approval_id": format_ap_id(seq), "digest": digest}),
        )?;
        tx.commit()?;
        Ok(seq)
    })();

    match insert {
        Ok(_) => {
            let approval = get(db, &uuid)?;
            Ok(RequestOutcome {
                approval,
                created: true,
            })
        }
        Err(e) if is_unique_constraint(&e) => match get_pending_by_digest(db, &digest)? {
            Some(approval) => Ok(RequestOutcome {
                approval,
                created: false,
            }),
            None => Err(e),
        },
        Err(e) => Err(e),
    }
}

pub fn decide(
    db: &Db,
    id: &str,
    decision: Decision,
    via: DecidedVia,
    note: Option<&str>,
    ctx: &EventCtx,
) -> Result<Approval> {
    let decider = ctx.actor.trim();
    if decider.is_empty() {
        return Err(Error::Approval(ApprovalError::Invalid(
            "decider (actor) must not be empty".into(),
        )));
    }
    let now_z = dates::now_in_operator_tz()?;
    let now = dates::format_iso(&now_z);
    let note = note.map(str::trim).filter(|s| !s.is_empty());
    let mut conn = db.get()?;
    // IMMEDIATE: a deferred transaction that reads first gets SQLITE_BUSY
    // on the write upgrade without waiting out busy_timeout.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let current = get_in_conn(&tx, id)?;
    if current.status != "pending" {
        return Err(Error::Approval(ApprovalError::Conflict(format!(
            "{} is {}, not pending",
            current.ap_id(),
            current.status
        ))));
    }
    if is_past(current.expires_at.as_deref(), &now_z) {
        mark_expired(&tx, &current, &now, &EventCtx::system("approvals"))?;
        tx.commit()?;
        return Err(Error::Approval(ApprovalError::Conflict(format!(
            "{} expired at {}, not pending",
            current.ap_id(),
            current.expires_at.as_deref().unwrap_or_default()
        ))));
    }
    if current.requester == decider {
        return Err(Error::Approval(ApprovalError::Forbidden(
            "the requester cannot decide their own approval; only the operator can".into(),
        )));
    }
    tx.execute(
        "UPDATE approvals
         SET status = ?1, decided_by = ?2, decided_via = ?3, decision_note = ?4, decided_at = ?5
         WHERE id = ?6 AND status = 'pending'",
        params![
            decision.status(),
            decider,
            via.as_str(),
            note,
            now,
            current.uuid,
        ],
    )?;
    if tx.changes() != 1 {
        return Err(Error::Approval(ApprovalError::Conflict(format!(
            "{} is no longer pending",
            current.ap_id()
        ))));
    }
    record_event(
        &tx,
        ctx,
        &current.uuid,
        decision.event_type(),
        serde_json::json!({"approval_id": current.ap_id(), "via": via.as_str()}),
    )?;
    tx.commit()?;
    get(db, &current.uuid)
}

pub fn withdraw(db: &Db, id: &str, ctx: &EventCtx) -> Result<Approval> {
    let actor = ctx.actor.trim();
    let now = dates::format_iso(&dates::now_in_operator_tz()?);
    let mut conn = db.get()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let current = get_in_conn(&tx, id)?;
    if current.status != "pending" {
        return Err(Error::Approval(ApprovalError::Conflict(format!(
            "{} is {}, not pending",
            current.ap_id(),
            current.status
        ))));
    }
    if current.requester != actor {
        return Err(Error::Approval(ApprovalError::Forbidden(
            "only the requester can withdraw this approval".into(),
        )));
    }
    tx.execute(
        "UPDATE approvals SET status = 'withdrawn', decided_by = ?1, decided_at = ?2
         WHERE id = ?3 AND status = 'pending'",
        params![actor, now, current.uuid],
    )?;
    record_event(
        &tx,
        ctx,
        &current.uuid,
        "approval.withdrawn",
        serde_json::json!({"approval_id": current.ap_id()}),
    )?;
    tx.commit()?;
    get(db, &current.uuid)
}

fn offered_digest(src: &PayloadSource) -> Result<String> {
    Ok(resolve_payload(src)?.digest)
}

fn gate_status_and_digest(ap: &Approval, offered: &str) -> Result<()> {
    if ap.status == "pending" {
        return Err(Error::Approval(ApprovalError::Pending));
    }
    if TERMINAL.contains(&ap.status.as_str()) {
        return Err(Error::Approval(ApprovalError::Terminal(ap.status.clone())));
    }
    if ap.status != "approved" {
        return Err(Error::Approval(ApprovalError::Conflict(format!(
            "{} has unexpected status {}",
            ap.ap_id(),
            ap.status
        ))));
    }
    if offered != ap.digest {
        return Err(Error::Approval(ApprovalError::DigestMismatch));
    }
    if ap.consumed_at.is_some() {
        return Err(Error::Approval(ApprovalError::AlreadyConsumed));
    }
    Ok(())
}

/// Check that `id` is approved, the offered payload matches, and it has
/// not been consumed. Does not latch.
pub fn verify(db: &Db, id: &str, offered: &PayloadSource) -> Result<Approval> {
    let ap = get(db, id)?;
    let digest = offered_digest(offered)?;
    gate_status_and_digest(&ap, &digest)?;
    Ok(ap)
}

/// Atomic check-and-set: same gates as [`verify`], then latch
/// `consumed_at`/`consumed_by`. A digest mismatch does not latch.
pub fn consume(db: &Db, id: &str, offered: &PayloadSource, ctx: &EventCtx) -> Result<Approval> {
    let digest = offered_digest(offered)?;
    let now = dates::format_iso(&dates::now_in_operator_tz()?);
    let actor = ctx.actor.trim();
    let mut conn = db.get()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let current = get_in_conn(&tx, id)?;
    gate_status_and_digest(&current, &digest)?;
    tx.execute(
        "UPDATE approvals SET consumed_at = ?1, consumed_by = ?2
         WHERE id = ?3 AND status = 'approved' AND consumed_at IS NULL",
        params![now, actor, current.uuid],
    )?;
    if tx.changes() != 1 {
        return Err(Error::Approval(ApprovalError::AlreadyConsumed));
    }
    record_event(
        &tx,
        ctx,
        &current.uuid,
        "approval.consumed",
        serde_json::json!({"approval_id": current.ap_id()}),
    )?;
    tx.commit()?;
    get(db, &current.uuid)
}

/// Mark pending rows whose `expires_at` is in the past as expired.
/// Idempotent: already-expired rows are left alone. Returns how many
/// newly expired.
pub fn expire(db: &Db, ctx: &EventCtx) -> Result<usize> {
    let now = dates::now_in_operator_tz()?;
    let now_iso = dates::format_iso(&now);
    let pending = list(db, Some("pending"))?;
    let mut n = 0usize;
    for ap in pending {
        if !is_past(ap.expires_at.as_deref(), &now) {
            continue;
        }
        let mut conn = db.get()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if mark_expired(&tx, &ap, &now_iso, ctx)? {
            n += 1;
        }
        tx.commit()?;
    }
    Ok(n)
}

/// True when `expires_at` parses and is at or before `now`. Unparseable or
/// absent means "never expires".
fn is_past(expires_at: Option<&str>, now: &jiff::Zoned) -> bool {
    expires_at
        .and_then(dates::parse_iso_to_utc)
        .is_some_and(|z| z.timestamp() <= now.timestamp())
}

/// Flip one pending row to expired inside `tx`. False if it was no longer
/// pending.
fn mark_expired(
    tx: &rusqlite::Transaction<'_>,
    ap: &Approval,
    now_iso: &str,
    ctx: &EventCtx,
) -> Result<bool> {
    let changed = tx.execute(
        "UPDATE approvals SET status = 'expired', decided_at = ?1
         WHERE id = ?2 AND status = 'pending'",
        params![now_iso, ap.uuid],
    )?;
    if changed == 1 {
        record_event(
            tx,
            ctx,
            &ap.uuid,
            "approval.expired",
            serde_json::json!({"approval_id": ap.ap_id()}),
        )?;
    }
    Ok(changed == 1)
}

pub fn mark_notified(db: &Db, uuid: &str) -> Result<()> {
    let now = dates::format_iso(&dates::now_in_operator_tz()?);
    let conn = db.get()?;
    conn.execute(
        "UPDATE approvals SET notified_at = ?1
         WHERE id = ?2 AND status = 'pending' AND notified_at IS NULL",
        params![now, uuid],
    )?;
    Ok(())
}

pub fn pending_unnotified(db: &Db) -> Result<Vec<Approval>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(&format!(
        "{SELECT_SQL} WHERE a.status = 'pending' AND a.notified_at IS NULL ORDER BY a.seq ASC"
    ))?;
    let rows = stmt.query_map([], map_row)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Build a file payload source from a path, enforcing the size cap.
pub fn payload_from_file(path: &std::path::Path) -> Result<PayloadSource> {
    let meta = std::fs::metadata(path).map_err(|e| {
        Error::Approval(ApprovalError::Invalid(format!(
            "cannot read payload file {}: {e}",
            path.display()
        )))
    })?;
    if meta.len() as usize > MAX_PAYLOAD_BYTES {
        return Err(Error::Approval(ApprovalError::Invalid(format!(
            "payload exceeds {MAX_PAYLOAD_BYTES} bytes (256 KiB); use --digest for large payloads"
        ))));
    }
    let bytes = std::fs::read(path).map_err(|e| {
        Error::Approval(ApprovalError::Invalid(format!(
            "cannot read payload file {}: {e}",
            path.display()
        )))
    })?;
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(Error::Approval(ApprovalError::Invalid(format!(
            "payload exceeds {MAX_PAYLOAD_BYTES} bytes (256 KiB); use --digest for large payloads"
        ))));
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string);
    let reference = Some(path.display().to_string());
    Ok(PayloadSource::File {
        bytes,
        name,
        reference,
    })
}

pub fn payload_from_json_str(raw: &str) -> Result<PayloadSource> {
    Ok(PayloadSource::Json(parse_json_payload(raw)?))
}

pub fn payload_from_json_value(value: &serde_json::Value) -> Result<PayloadSource> {
    Ok(PayloadSource::Json(canonicalize_json(value)?))
}

/// The payload of an HTTP or MCP approval request: exactly one of inline
/// UTF-8 text (`name` labels it), a JSON value, or a digest. The size cap is
/// enforced by [`request`], like every other source.
pub fn payload_from_wire(
    text: Option<String>,
    name: Option<String>,
    json: Option<&serde_json::Value>,
    digest: Option<&str>,
) -> Result<PayloadSource> {
    match (text, json, digest) {
        (Some(text), None, None) => Ok(PayloadSource::File {
            bytes: text.into_bytes(),
            name,
            reference: None,
        }),
        (None, Some(value), None) => payload_from_json_value(value),
        (None, None, Some(hex)) => payload_from_digest(hex),
        _ => Err(Error::Approval(ApprovalError::Invalid(
            "exactly one of payload, payload_json, digest is required".into(),
        ))),
    }
}

pub fn payload_from_digest(hex: &str) -> Result<PayloadSource> {
    let hex = hex.trim();
    if !is_digest_hex(hex) {
        return Err(Error::Approval(ApprovalError::Invalid(
            "digest must be 64 lowercase hex characters".into(),
        )));
    }
    Ok(PayloadSource::Digest(hex.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::EventCtx;
    use crate::storage::Db;

    fn fresh() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("t.db")).unwrap();
        (dir, db)
    }

    fn ctx(actor: &str) -> EventCtx {
        EventCtx::local(actor)
    }

    fn file_src(bytes: &[u8]) -> PayloadSource {
        PayloadSource::File {
            bytes: bytes.to_vec(),
            name: Some("x.txt".into()),
            reference: Some("x.txt".into()),
        }
    }

    #[test]
    fn wire_payload_takes_exactly_one_source() {
        let v = serde_json::json!({"a": 1});
        assert!(payload_from_wire(None, None, None, None).is_err());
        assert!(payload_from_wire(Some("x".into()), None, Some(&v), None).is_err());
        assert!(matches!(
            payload_from_wire(Some("x".into()), Some("n.txt".into()), None, None),
            Ok(PayloadSource::File { name: Some(n), .. }) if n == "n.txt"
        ));
        assert!(matches!(
            payload_from_wire(None, None, Some(&v), None),
            Ok(PayloadSource::Json(_))
        ));
        assert!(payload_from_wire(None, None, None, Some("nothex")).is_err());
    }

    #[test]
    fn canonical_json_sorts_keys_and_keeps_utf8() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"b":1,"a":"café","nested":{"z":true,"m":0}}"#).unwrap();
        let bytes = canonicalize_json(&v).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"a":"café","b":1,"nested":{"m":0,"z":true}}"#
        );
    }

    #[test]
    fn digest_hex_rejects_uppercase_and_short() {
        assert!(!is_digest_hex("A".repeat(64).as_str()));
        assert!(!is_digest_hex("xyz"));
        assert!(is_digest_hex(&"ab".repeat(32)));
    }

    #[test]
    fn request_is_idempotent_on_pending_digest() {
        let (_d, db) = fresh();
        let input = RequestInput {
            kind: "email".into(),
            title: "Send".into(),
            request_note: Some("please".into()),
            payload: file_src(b"hello"),
            task_pt_id: None,
            expires_in: None,
        };
        let a = request(&db, input.clone(), &ctx("hal")).unwrap();
        let b = request(&db, input, &ctx("hal")).unwrap();
        assert!(a.created && !b.created);
        assert_eq!(a.approval.ap_id(), b.approval.ap_id());
        assert_eq!(a.approval.digest, sha256_hex(b"hello"));
    }

    fn expiring(expires_in: &str) -> RequestInput {
        RequestInput {
            kind: "email".into(),
            title: "Send before the deadline".into(),
            request_note: None,
            payload: file_src(b"time-bound"),
            task_pt_id: None,
            expires_in: Some(expires_in.into()),
        }
    }

    #[test]
    fn deciding_past_expiry_expires_instead_of_approving() {
        let (_d, db) = fresh();
        let ap = request(&db, expiring("0s"), &ctx("hal")).unwrap().approval;
        let err = decide(
            &db,
            &ap.ap_id(),
            Decision::Approve,
            DecidedVia::Dashboard,
            None,
            &ctx("operator"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("expired"), "{err}");
        assert_eq!(get(&db, &ap.ap_id()).unwrap().status, "expired");
    }

    #[test]
    fn re_request_after_expiry_mints_a_fresh_pending_row() {
        let (_d, db) = fresh();
        let first = request(&db, expiring("0s"), &ctx("hal")).unwrap();
        let second = request(&db, expiring("1h"), &ctx("hal")).unwrap();
        assert!(
            second.created,
            "a stale pending row must not satisfy dedupe"
        );
        assert_ne!(first.approval.ap_id(), second.approval.ap_id());
        assert_eq!(get(&db, &first.approval.ap_id()).unwrap().status, "expired");
        assert_eq!(second.approval.status, "pending");
    }

    #[test]
    fn parse_expires_in_rejects_instead_of_panicking() {
        for bad in ["", "d", "5日", "é", "99999999d", "-1h", "5w"] {
            assert!(parse_expires_in(bad).is_err(), "{bad:?}");
        }
        assert!(parse_expires_in(" 90m ").is_ok());
    }

    #[test]
    fn requester_cannot_decide() {
        let (_d, db) = fresh();
        let input = RequestInput {
            kind: "other".into(),
            title: "x".into(),
            request_note: None,
            payload: PayloadSource::Digest("c".repeat(64)),
            task_pt_id: None,
            expires_in: None,
        };
        let ap = request(&db, input, &ctx("hal")).unwrap().approval;
        let err = decide(
            &db,
            &ap.ap_id(),
            Decision::Approve,
            DecidedVia::Dashboard,
            None,
            &ctx("hal"),
        )
        .unwrap_err();
        match err {
            Error::Approval(ApprovalError::Forbidden(m)) => {
                assert!(m.contains("operator"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn consume_is_one_shot_and_mismatch_does_not_latch() {
        let (_d, db) = fresh();
        let input = RequestInput {
            kind: "email".into(),
            title: "x".into(),
            request_note: None,
            payload: file_src(b"exact"),
            task_pt_id: None,
            expires_in: None,
        };
        let ap = request(&db, input, &ctx("hal")).unwrap().approval;
        decide(
            &db,
            &ap.ap_id(),
            Decision::Approve,
            DecidedVia::Dashboard,
            Some("ok"),
            &ctx("operator"),
        )
        .unwrap();
        let err = consume(&db, &ap.ap_id(), &file_src(b"nope"), &ctx("hal")).unwrap_err();
        assert!(matches!(
            err,
            Error::Approval(ApprovalError::DigestMismatch)
        ));
        assert!(get(&db, &ap.ap_id()).unwrap().consumed_at.is_none());
        consume(&db, &ap.ap_id(), &file_src(b"exact"), &ctx("hal")).unwrap();
        let err = consume(&db, &ap.ap_id(), &file_src(b"exact"), &ctx("hal")).unwrap_err();
        assert!(matches!(
            err,
            Error::Approval(ApprovalError::AlreadyConsumed)
        ));
    }

    #[test]
    fn db_trigger_blocks_digest_tamper() {
        let (_d, db) = fresh();
        let input = RequestInput {
            kind: "email".into(),
            title: "x".into(),
            request_note: None,
            payload: file_src(b"orig"),
            task_pt_id: None,
            expires_in: None,
        };
        let ap = request(&db, input, &ctx("hal")).unwrap().approval;
        let err = db
            .with_conn(|c| {
                c.execute(
                    "UPDATE approvals SET digest = ?1 WHERE id = ?2",
                    params!["0".repeat(64), ap.uuid],
                )?;
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(err, Error::Sqlite(_)));
    }

    #[test]
    fn preview_from_stored_text_and_digest_only() {
        assert!(render_preview(Some(b"hello world"), Some("file"), true).contains("hello world"));
        assert!(
            render_preview(None, None, false)
                .to_ascii_lowercase()
                .contains("not stored")
        );
        assert_eq!(
            render_preview(Some(&[0xff, 0x00]), Some("file"), true),
            "<binary 2 bytes>"
        );
    }
}
