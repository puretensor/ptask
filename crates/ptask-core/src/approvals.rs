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

/// Requester-supplied text is bounded on every surface (MCP and HTTP
/// accept arbitrary strings): a title is a one-line summary, a note is
/// prose the operator reads, a payload name is a file name.
pub const MAX_TITLE_CHARS: usize = 300;
pub const MAX_NOTE_CHARS: usize = 16 * 1024;
pub const MAX_NAME_CHARS: usize = 255;

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

    /// The payload gate's verdict right now: approved, not past
    /// `expires_at`, not consumed. What `payload` releases on and what a
    /// poller should wait for (`status` stays "approved" after expiry and
    /// after consume).
    pub fn in_force(&self) -> bool {
        dates::now_in_operator_tz()
            .is_ok_and(|now| gate_status(self, &now).is_ok() && self.consumed_at.is_none())
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
            "in_force": self.in_force(),
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
/// of a still-pending digest by the same requester. The existing row is
/// returned unchanged; `notice` names any requested field that was not
/// applied (a new `expires_in`, title, note, kind or task), so a caller
/// never mistakes the old deadline for the one it just asked for.
#[derive(Debug, Clone)]
pub struct RequestOutcome {
    pub approval: Approval,
    pub created: bool,
    pub notice: Option<String>,
}

impl RequestOutcome {
    /// The approval's JSON plus `deduplicated` and, when set, `notice`.
    pub fn to_json(&self) -> serde_json::Value {
        let mut v = self.approval.to_json(None);
        v["deduplicated"] = serde_json::json!(!self.created);
        if let Some(n) = &self.notice {
            v["notice"] = serde_json::json!(n);
        }
        v
    }
}

/// What a same-requester re-request asked for that the existing pending
/// row keeps instead. `None` when nothing differs.
fn dedupe_notice(
    existing: &Approval,
    input: &RequestInput,
    title: &str,
    note: Option<&str>,
) -> Option<String> {
    let mut ignored: Vec<String> = Vec::new();
    if let Some(spec) = input.expires_in.as_deref().map(str::trim)
        && !spec.is_empty()
    {
        ignored.push(format!(
            "expires_in {spec:?} (kept expires_at {})",
            existing.expires_at.as_deref().unwrap_or("none")
        ));
    }
    if existing.kind != input.kind {
        ignored.push("kind".into());
    }
    if existing.title != title {
        ignored.push("title".into());
    }
    if existing.request_note.as_deref() != note {
        ignored.push("note".into());
    }
    let task = input
        .task_pt_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if task.is_some() && existing.task_pt_id.as_deref() != task {
        ignored.push("task".into());
    }
    if ignored.is_empty() {
        return None;
    }
    Some(format!(
        "{} is already pending for this payload and was returned unchanged; not applied: {}. \
         Withdraw it and request again to change them.",
        existing.ap_id(),
        ignored.join(", ")
    ))
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
/// non-ASCII kept as UTF-8, floats in Python `repr` form. For every value
/// this accepts, the bytes equal Python
/// `json.dumps(..., sort_keys=True, separators=(",", ":"), ensure_ascii=False)`.
///
/// The digest must name one payload, so a number that cannot be carried
/// exactly is refused rather than rounded: integers beyond 64 bits and
/// non-integers of magnitude 2^53 or more (an f64 there is integral and may
/// be a rounded big integer). Send such values as strings.
pub fn canonicalize_json(value: &serde_json::Value) -> Result<Vec<u8>> {
    check_numbers(value)?;
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(
        &mut out,
        PythonFloats(serde_json::ser::CompactFormatter),
    );
    serde::Serialize::serialize(&sort_json(value), &mut ser)
        .map_err(|e| Error::Approval(ApprovalError::Invalid(format!("canonical JSON: {e}"))))?;
    Ok(out)
}

/// Parse JSON text strictly and canonicalise it. On top of
/// [`canonicalize_json`], refuses what the parsed value can no longer show:
/// duplicate object keys (serde keeps the last, other parsers the first)
/// and number literals with more precision than the f64 they parse to.
pub fn parse_json_payload(raw: &str) -> Result<Vec<u8>> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|e| {
        Error::Approval(ApprovalError::Invalid(format!("invalid JSON payload: {e}")))
    })?;
    check_json_text(raw)?;
    canonicalize_json(&value)
}

fn invalid_json(msg: String) -> Error {
    Error::Approval(ApprovalError::Invalid(format!(
        "invalid JSON payload: {msg}"
    )))
}

/// 2^53: below it every integer-valued f64 is exactly the integer written.
const MAX_EXACT_F64: f64 = 9_007_199_254_740_992.0;

fn check_numbers(value: &serde_json::Value) -> Result<()> {
    match value {
        serde_json::Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() && f.abs() >= MAX_EXACT_F64 => Err(invalid_json(format!(
                "number {n} cannot be represented exactly (integers must fit 64 bits, \
                 other numbers must be below 2^53 in magnitude); send it as a string"
            ))),
            _ => Ok(()),
        },
        serde_json::Value::Array(items) => items.iter().try_for_each(check_numbers),
        serde_json::Value::Object(map) => map.values().try_for_each(check_numbers),
        _ => Ok(()),
    }
}

/// Scan syntactically valid JSON text for duplicate keys (compared after
/// unescaping, so `"a"` and `"a"` collide) and inexact number
/// literals.
fn check_json_text(raw: &str) -> Result<()> {
    enum Frame {
        Object {
            keys: std::collections::HashSet<String>,
            expect_key: bool,
        },
        Array,
    }
    let b = raw.as_bytes();
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' => {
                stack.push(Frame::Object {
                    keys: Default::default(),
                    expect_key: true,
                });
                i += 1;
            }
            b'[' => {
                stack.push(Frame::Array);
                i += 1;
            }
            b'}' | b']' => {
                stack.pop();
                i += 1;
            }
            b',' | b':' => {
                if let Some(Frame::Object { expect_key, .. }) = stack.last_mut() {
                    *expect_key = b[i] == b',';
                }
                i += 1;
            }
            b'"' => {
                let mut j = i + 1;
                while b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                if let Some(Frame::Object {
                    keys,
                    expect_key: true,
                }) = stack.last_mut()
                {
                    let key: String = serde_json::from_str(&raw[i..=j])
                        .map_err(|e| invalid_json(e.to_string()))?;
                    if !keys.insert(key.clone()) {
                        return Err(invalid_json(format!("duplicate key {key:?}")));
                    }
                }
                i = j + 1;
            }
            b'-' | b'0'..=b'9' => {
                let mut j = i;
                while j < b.len() && matches!(b[j], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    j += 1;
                }
                check_number_literal(&raw[i..j])?;
                i = j;
            }
            _ => i += 1,
        }
    }
    Ok(())
}

/// A non-integer literal may carry at most 17 significant digits, enough to
/// name any f64 and no more, so it reads as the same double in every
/// correctly rounding parser (Python's included) and nothing the writer
/// meant is dropped. Non-shortest forms such as `0.10000000000000001` (C
/// `%.17g`, jq 1.6, Postgres `extra_float_digits=3`) are fine;
/// `0.1000000000000000000001`, and a nonzero literal that underflows to
/// zero, are not. Integer literals parse exactly or overflow to f64, which
/// [`check_numbers`] refuses.
fn check_number_literal(lit: &str) -> Result<()> {
    let n: serde_json::Number =
        serde_json::from_str(lit).map_err(|e| invalid_json(format!("number {lit}: {e}")))?;
    let Some(f) = n.as_f64().filter(|_| n.is_f64()) else {
        return Ok(());
    };
    if lit.bytes().all(|c| c == b'-' || c.is_ascii_digit()) {
        // An integer literal that still became an f64: beyond 64 bits
        // (check_numbers refuses it) or "-0", which serde reads as the
        // float -0.0 and Python as the integer 0.
        return if f == 0.0 {
            Err(invalid_json(format!(
                "number {lit} is ambiguous; write 0 or -0.0"
            )))
        } else {
            Ok(())
        };
    }
    let digits = decimal_value(lit).map_or(usize::MAX, |(_, d, _)| d.len());
    if digits > 17 {
        return Err(invalid_json(format!(
            "number {lit} has {digits} significant digits; a 64-bit float holds 17, \
             send it as a string"
        )));
    }
    if f == 0.0 && digits > 0 {
        return Err(invalid_json(format!(
            "number {lit} underflows to zero; send it as a string"
        )));
    }
    Ok(())
}

/// (negative, significant digits, power of ten) of a decimal literal, with
/// leading and trailing zeros normalised away.
fn decimal_value(s: &str) -> Option<(bool, String, i64)> {
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let (mant, exp) = match s.find(['e', 'E']) {
        Some(at) => (&s[..at], s[at + 1..].parse::<i64>().ok()?),
        None => (s, 0),
    };
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    let mut exp = exp.checked_sub(i64::try_from(frac.len()).ok()?)?;
    let mut digits = format!("{int}{frac}").trim_start_matches('0').to_string();
    while digits.ends_with('0') {
        digits.pop();
        exp += 1;
    }
    if digits.is_empty() {
        return Some((neg, digits, 0));
    }
    Some((neg, digits, exp))
}

/// Shortest round-trip digits of `v`, closest to its exact value when there
/// is a tie in length (ryu, via serde_json). Rust's `{:e}` is shortest but
/// can pick the other candidate (797815578912564.3 for …564.2), which
/// Python's repr would not.
fn shortest(v: f64) -> String {
    serde_json::Number::from_f64(v).map_or_else(|| v.to_string(), |n| n.to_string())
}

/// Python's `repr(float)`: shortest round-trip digits, positional when the
/// decimal point falls in (-4, 16], else `d.ddde±XX`.
fn python_float_repr(v: f64) -> String {
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let Some((_, digits, exp10)) = decimal_value(&shortest(v.abs())) else {
        return shortest(v);
    };
    let decpt = digits.len() as i64 + exp10;
    let exp = decpt - 1;
    let sign = if v < 0.0 { "-" } else { "" };
    let body = if decpt <= -4 || decpt > 16 {
        let m = if digits.len() == 1 {
            digits
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    } else if decpt <= 0 {
        format!("0.{}{digits}", "0".repeat(decpt.unsigned_abs() as usize))
    } else if (decpt as usize) < digits.len() {
        format!(
            "{}.{}",
            &digits[..decpt as usize],
            &digits[decpt as usize..]
        )
    } else {
        format!("{digits}{}.0", "0".repeat(decpt as usize - digits.len()))
    };
    format!("{sign}{body}")
}

/// Wraps a serde_json formatter so floats print as Python `repr` does; the
/// structure (compact or pretty) is the inner formatter's.
struct PythonFloats<F>(F);

impl<F: serde_json::ser::Formatter> serde_json::ser::Formatter for PythonFloats<F> {
    fn write_f64<W: ?Sized + std::io::Write>(&mut self, w: &mut W, v: f64) -> std::io::Result<()> {
        w.write_all(python_float_repr(v).as_bytes())
    }
    fn begin_array<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.begin_array(w)
    }
    fn end_array<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_array(w)
    }
    fn begin_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        self.0.begin_array_value(w, first)
    }
    fn end_array_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_array_value(w)
    }
    fn begin_object<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.begin_object(w)
    }
    fn end_object<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_object(w)
    }
    fn begin_object_key<W: ?Sized + std::io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        self.0.begin_object_key(w, first)
    }
    fn end_object_key<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_object_key(w)
    }
    fn begin_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.begin_object_value(w)
    }
    fn end_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_object_value(w)
    }
}

/// Pretty-printed JSON for the operator's preview, floats printed exactly as
/// the canonical bytes carry them.
fn pretty_json(value: &serde_json::Value) -> Option<String> {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(
        &mut out,
        PythonFloats(serde_json::ser::PrettyFormatter::new()),
    );
    serde::Serialize::serialize(value, &mut ser).ok()?;
    String::from_utf8(out).ok()
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
            Ok(v) => pretty_json(&v).unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned()),
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

fn check_len(field: &str, value: &str, max: usize) -> Result<()> {
    if value.chars().count() > max {
        return Err(Error::Approval(ApprovalError::Invalid(format!(
            "{field} exceeds {max} characters"
        ))));
    }
    Ok(())
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

/// Requester identity comparison: trimmed, ASCII case-insensitive. "HAL"
/// and "hal" are one actor, so neither requester != decider nor
/// withdraw-own-rows can be dodged by changing case. ASCII folding matches
/// SQLite's `lower()`, which the pending dedupe index uses.
pub fn same_actor(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
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

/// The requester's own pending row for `digest`. Dedupe is per requester
/// (V020): another actor's request for the same payload is theirs, not
/// this caller's.
fn get_pending_by_digest(db: &Db, requester: &str, digest: &str) -> Result<Option<Approval>> {
    let conn = db.get()?;
    let found = conn
        .query_row(
            &format!(
                "{SELECT_SQL} WHERE a.digest = ?1 AND lower(a.requester) = lower(?2)
                   AND a.status = 'pending'"
            ),
            params![digest, requester],
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

/// The stored payload, for an executor: released only while the approval is
/// in force (approved, not past `expires_at`, not yet consumed), so an
/// executor that skips the status check cannot act on bytes the operator
/// never approved. Errors carry the verify/consume exit codes.
pub fn payload_bytes(db: &Db, id: &str) -> Result<Vec<u8>> {
    let ap = get(db, id)?;
    gate_status(&ap, &dates::now_in_operator_tz()?)?;
    if ap.consumed_at.is_some() {
        return Err(Error::Approval(ApprovalError::AlreadyConsumed));
    }
    stored_payload(ap)
}

/// The stored payload whatever the status: the operator inspecting bytes the
/// preview cannot show (binary files). Callers gate this to the operator.
pub fn inspect_payload_bytes(db: &Db, id: &str) -> Result<Vec<u8>> {
    stored_payload(get(db, id)?)
}

fn stored_payload(ap: Approval) -> Result<Vec<u8>> {
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

/// Insert a new approval, or return the caller's own pending row with the
/// same digest (idempotent re-request).
pub fn request(db: &Db, input: RequestInput, ctx: &EventCtx) -> Result<RequestOutcome> {
    validate_kind(&input.kind)?;
    let title = input.title.trim();
    if title.is_empty() {
        return Err(Error::Approval(ApprovalError::Invalid(
            "title must not be empty".into(),
        )));
    }
    check_len("title", title, MAX_TITLE_CHARS)?;
    if let Some(note) = input.request_note.as_deref() {
        check_len("note", note.trim(), MAX_NOTE_CHARS)?;
    }
    if let PayloadSource::File {
        name: Some(name), ..
    } = &input.payload
    {
        check_len("payload_name", name, MAX_NAME_CHARS)?;
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
    if let Some(existing) = get_pending_by_digest(db, requester, &digest)? {
        let notice = dedupe_notice(&existing, &input, title, note.as_deref());
        return Ok(RequestOutcome {
            approval: existing,
            created: false,
            notice,
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
                notice: None,
            })
        }
        Err(e) if is_unique_constraint(&e) => {
            match get_pending_by_digest(db, requester, &digest)? {
                Some(approval) => Ok(RequestOutcome {
                    notice: dedupe_notice(&approval, &input, title, note.as_deref()),
                    approval,
                    created: false,
                }),
                None => Err(e),
            }
        }
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
    if let Some(note) = note {
        check_len("note", note, MAX_NOTE_CHARS)?;
    }
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
        mark_expired(
            &tx,
            &current.uuid,
            current.seq,
            &now,
            &EventCtx::system("approvals"),
        )?;
        tx.commit()?;
        return Err(Error::Approval(ApprovalError::Conflict(format!(
            "{} expired at {}, not pending",
            current.ap_id(),
            current.expires_at.as_deref().unwrap_or_default()
        ))));
    }
    if same_actor(&current.requester, decider) {
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
    if !same_actor(&current.requester, actor) {
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

/// The approval is in force: approved and, unless already consumed, not past
/// `expires_at`. `expires_at` bounds the whole approval, not just the
/// decision window: an approval for "send before Friday" must not authorise
/// a send on Monday. The row stays `approved` (decided rows are frozen); the
/// gate reports it as expired.
fn gate_status(ap: &Approval, now: &jiff::Zoned) -> Result<()> {
    if ap.status == "pending" {
        // Not yet swept: it can no longer be approved, so say expired.
        if is_past(ap.expires_at.as_deref(), now) {
            return Err(Error::Approval(ApprovalError::Terminal(format!(
                "expired (pending past expires_at {})",
                ap.expires_at.as_deref().unwrap_or_default()
            ))));
        }
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
    if ap.consumed_at.is_none() && is_past(ap.expires_at.as_deref(), now) {
        return Err(Error::Approval(ApprovalError::Terminal(format!(
            "expired (approved, but expires_at {} has passed)",
            ap.expires_at.as_deref().unwrap_or_default()
        ))));
    }
    Ok(())
}

fn gate_status_and_digest(ap: &Approval, offered: &str, now: &jiff::Zoned) -> Result<()> {
    gate_status(ap, now)?;
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
    gate_status_and_digest(&ap, &digest, &dates::now_in_operator_tz()?)?;
    Ok(ap)
}

/// Atomic check-and-set: same gates as [`verify`], then latch
/// `consumed_at`/`consumed_by`. A digest mismatch does not latch.
pub fn consume(db: &Db, id: &str, offered: &PayloadSource, ctx: &EventCtx) -> Result<Approval> {
    let digest = offered_digest(offered)?;
    let now_z = dates::now_in_operator_tz()?;
    let now = dates::format_iso(&now_z);
    let actor = ctx.actor.trim();
    let mut conn = db.get()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let current = get_in_conn(&tx, id)?;
    gate_status_and_digest(&current, &digest, &now_z)?;
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
/// newly expired. Runs on every request, so it reads only the three
/// columns it needs, never the payload blobs.
pub fn expire(db: &Db, ctx: &EventCtx) -> Result<usize> {
    let now = dates::now_in_operator_tz()?;
    let now_iso = dates::format_iso(&now);
    let candidates: Vec<(String, i64, String)> = {
        let conn = db.get()?;
        let mut stmt = conn.prepare(
            "SELECT id, seq, expires_at FROM approvals
             WHERE status = 'pending' AND expires_at IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let mut n = 0usize;
    for (uuid, seq, expires_at) in candidates {
        if !is_past(Some(&expires_at), &now) {
            continue;
        }
        let mut conn = db.get()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if mark_expired(&tx, &uuid, seq, &now_iso, ctx)? {
            n += 1;
        }
        tx.commit()?;
    }
    Ok(n)
}

/// True when `expires_at` is at or before `now`. Absent means "never
/// expires"; present but unparseable fails closed (treated as past), since
/// pTask only ever writes ISO timestamps and anything else is damage.
fn is_past(expires_at: Option<&str>, now: &jiff::Zoned) -> bool {
    expires_at.is_some_and(|s| {
        dates::parse_iso_to_utc(s).is_none_or(|z| z.timestamp() <= now.timestamp())
    })
}

/// Flip one pending row to expired inside `tx`. False if it was no longer
/// pending.
fn mark_expired(
    tx: &rusqlite::Transaction<'_>,
    uuid: &str,
    seq: i64,
    now_iso: &str,
    ctx: &EventCtx,
) -> Result<bool> {
    let changed = tx.execute(
        "UPDATE approvals SET status = 'expired', decided_at = ?1
         WHERE id = ?2 AND status = 'pending'",
        params![now_iso, uuid],
    )?;
    if changed == 1 {
        record_event(
            tx,
            ctx,
            uuid,
            "approval.expired",
            serde_json::json!({"approval_id": format_ap_id(seq)}),
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

    fn canon(raw: &str) -> std::result::Result<String, String> {
        parse_json_payload(raw)
            .map(|b| String::from_utf8(b).unwrap())
            .map_err(|e| e.to_string())
    }

    #[test]
    fn canonical_json_rejects_ambiguous_input() {
        // Last-wins duplicates: a first-wins consumer reads a different
        // object from the same approved digest. "a" is "a".
        for raw in [r#"{"a":1,"a":2}"#, r#"{"x":{"a":1,"a":2}}"#] {
            let err = canon(raw).unwrap_err();
            assert!(err.contains("duplicate key"), "{raw}: {err}");
        }
        // Same key name in sibling objects is fine.
        assert!(canon(r#"[{"a":1},{"a":2}]"#).is_ok());
        // Precision a bignum-exact parser keeps but f64 loses.
        for raw in [
            "18446744073709551616",
            "-9223372036854775809",
            "9007199254740993.0",
            "1e300",
            "-0",
            "0.1000000000000000000001",
            "1e-400",
            r#"{"amount":12345678901234567890123}"#,
        ] {
            assert!(canon(raw).is_err(), "{raw} must be rejected");
        }
        // The same rule on the wire path, where the transport parsed it.
        let v = serde_json::json!({"n": 18446744073709551616.0_f64});
        assert!(canonicalize_json(&v).is_err());
    }

    #[test]
    fn canonical_json_matches_python_json_dumps() {
        // Expected bytes are Python's
        // json.dumps(json.loads(raw), sort_keys=True, separators=(",", ":"),
        //            ensure_ascii=False).
        for (raw, python) in [
            ("1.50", "1.5"),
            ("1E2", "100.0"),
            ("-0.0", "-0.0"),
            ("0.1", "0.1"),
            ("1e-5", "1e-05"),
            ("0.0001", "0.0001"),
            ("1.5e-7", "1.5e-07"),
            ("123456.789", "123456.789"),
            // Round-tripping but not shortest (C %.17g, jq 1.6, Postgres
            // extra_float_digits=3): same double, Python's digits.
            ("0.10000000000000001", "0.1"),
            ("8.6834497869073662e-7", "8.683449786907366e-07"),
            ("1.2345678901234567e-300", "1.2345678901234568e-300"),
            ("123456.78901234567", "123456.78901234567"),
            ("797815578912564.2", "797815578912564.2"),
            ("-221972496954942.62", "-221972496954942.62"),
            ("0.11237863004311455", "0.11237863004311455"),
            ("1e15", "1000000000000000.0"),
            ("18446744073709551615", "18446744073709551615"),
            ("-9223372036854775808", "-9223372036854775808"),
            (
                r#""\u007f \u001f\b\u0000/""#,
                "\"\u{7f}\u{2028}\\u001f\\b\\u0000/\"",
            ),
            (
                r#"{"é":1,"z":2,"a":[3,{"b":null,"a":true}]}"#,
                r#"{"a":[3,{"a":true,"b":null}],"z":2,"é":1}"#,
            ),
        ] {
            assert_eq!(canon(raw).as_deref(), Ok(python), "{raw}");
        }
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
        assert!(
            b.notice.is_none(),
            "an identical re-request has nothing to report"
        );
        assert_eq!(b.to_json()["deduplicated"], serde_json::json!(true));
    }

    #[test]
    fn re_request_says_what_it_did_not_change() {
        let (_d, db) = fresh();
        let first = RequestInput {
            kind: "email".into(),
            title: "Send".into(),
            request_note: None,
            payload: file_src(b"deadline"),
            task_pt_id: None,
            expires_in: Some("1h".into()),
        };
        let a = request(&db, first.clone(), &ctx("hal")).unwrap();
        let again = RequestInput {
            title: "Send now".into(),
            expires_in: Some("2h".into()),
            ..first
        };
        let b = request(&db, again, &ctx("hal")).unwrap();
        assert!(!b.created);
        assert_eq!(
            b.approval.expires_at, a.approval.expires_at,
            "expiry is not moved"
        );
        assert_eq!(b.approval.title, "Send");
        let notice = b.notice.clone().expect("a notice");
        assert!(notice.contains(&a.approval.ap_id()), "{notice}");
        assert!(notice.contains("expires_in"), "{notice}");
        assert!(notice.contains("title"), "{notice}");
        assert_eq!(b.to_json()["notice"], serde_json::json!(notice));
    }

    #[test]
    fn digest_dedupe_is_scoped_to_the_requester() {
        let (_d, db) = fresh();
        let input = RequestInput {
            kind: "spend".into(),
            title: "Pay ACME".into(),
            request_note: None,
            payload: file_src(b"pay 400 to ACME"),
            task_pt_id: None,
            expires_in: None,
        };
        let hal = request(&db, input.clone(), &ctx("hal")).unwrap();
        let other = request(&db, input.clone(), &ctx("ops-bot")).unwrap();
        assert!(other.created, "another requester must not get hal's row");
        assert_ne!(hal.approval.ap_id(), other.approval.ap_id());
        assert_eq!(other.approval.requester, "ops-bot");
        let again = request(&db, input, &ctx("HAL")).unwrap();
        assert!(!again.created);
        assert_eq!(again.approval.ap_id(), hal.approval.ap_id());
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
    fn request_text_fields_are_bounded() {
        let (_d, db) = fresh();
        let base = || RequestInput {
            kind: "other".into(),
            title: "ok".into(),
            request_note: None,
            payload: PayloadSource::Digest("e".repeat(64)),
            task_pt_id: None,
            expires_in: None,
        };
        let long_title = RequestInput {
            title: "t".repeat(MAX_TITLE_CHARS + 1),
            ..base()
        };
        let long_note = RequestInput {
            request_note: Some("n".repeat(2_000_000)),
            ..base()
        };
        let long_name = RequestInput {
            payload: PayloadSource::File {
                bytes: b"x".to_vec(),
                name: Some("f".repeat(MAX_NAME_CHARS + 1)),
                reference: None,
            },
            ..base()
        };
        for input in [long_title, long_note, long_name] {
            let err = request(&db, input, &ctx("hal")).unwrap_err();
            assert!(
                matches!(err, Error::Approval(ApprovalError::Invalid(_))),
                "{err:?}"
            );
        }
        let at_limit = RequestInput {
            title: "é".repeat(MAX_TITLE_CHARS),
            request_note: Some("n".repeat(MAX_NOTE_CHARS)),
            ..base()
        };
        assert!(request(&db, at_limit, &ctx("hal")).unwrap().created);
    }

    #[test]
    fn expire_never_reads_payloads() {
        let (_d, db) = fresh();
        let ap = request(&db, expiring("0s"), &ctx("hal")).unwrap().approval;
        // A pending row whose payload is not a blob: any sweep that maps
        // payload columns fails on it, one that skips them does not.
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO approvals (id, seq, kind, title, payload, payload_kind, digest,
                                        requester, status, created_at)
                 VALUES ('odd', 50, 'other', 'x', 42, 'file', printf('%.64c', 'f'), 'hal',
                         'pending', '2026-09-01T00:00:00Z')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        assert_eq!(expire(&db, &ctx("sweeper")).unwrap(), 1);
        assert_eq!(get(&db, &ap.ap_id()).unwrap().status, "expired");
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
    fn requester_identity_ignores_case_and_padding() {
        let (_d, db) = fresh();
        let input = RequestInput {
            kind: "other".into(),
            title: "x".into(),
            request_note: None,
            payload: PayloadSource::Digest("d".repeat(64)),
            task_pt_id: None,
            expires_in: None,
        };
        let ap = request(&db, input, &ctx("hal")).unwrap().approval;
        for actor in ["HAL", "Hal", " hal "] {
            let err = decide(
                &db,
                &ap.ap_id(),
                Decision::Approve,
                DecidedVia::Dashboard,
                None,
                &ctx(actor),
            )
            .unwrap_err();
            assert!(
                matches!(err, Error::Approval(ApprovalError::Forbidden(_))),
                "{actor:?}: {err:?}"
            );
        }
        withdraw(&db, &ap.ap_id(), &ctx("HAL")).unwrap();
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

    /// An approved row whose `expires_at` has passed. Inserted directly:
    /// a decided row is frozen, so the clock cannot be moved after approve.
    fn approved_past_expiry(db: &Db, bytes: &[u8]) -> String {
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO approvals (id, seq, kind, title, payload, payload_kind,
                                        payload_bytes, digest, requester, status,
                                        decided_by, decided_via, created_at, decided_at,
                                        expires_at)
                 VALUES ('stale', 99, 'email', 'x', ?1, 'file', ?2, ?3, 'hal', 'approved',
                         'operator', 'dashboard', '2026-09-01T00:00:00Z',
                         '2026-09-01T00:01:00Z', '2026-09-01T00:02:00Z')",
                params![bytes, bytes.len() as i64, sha256_hex(bytes)],
            )?;
            Ok(())
        })
        .unwrap();
        "AP-99".into()
    }

    /// A row inserted as-is (no sweep, no validation) with a stored payload.
    fn raw_row(db: &Db, seq: i64, status: &str, expires_at: &str, bytes: &[u8]) -> String {
        db.with_conn(|c| {
            c.execute(
                "INSERT INTO approvals (id, seq, kind, title, payload, payload_kind,
                                        payload_bytes, digest, requester, status,
                                        decided_by, decided_via, created_at, decided_at,
                                        expires_at)
                 VALUES (?1, ?2, 'email', 'x', ?3, 'file', ?4, ?5, 'hal', ?6,
                         CASE WHEN ?6 = 'pending' THEN NULL ELSE 'operator' END,
                         CASE WHEN ?6 = 'pending' THEN NULL ELSE 'dashboard' END,
                         '2026-09-01T00:00:00Z',
                         CASE WHEN ?6 = 'pending' THEN NULL ELSE '2026-09-01T00:01:00Z' END,
                         ?7)",
                params![
                    format!("raw-{seq}"),
                    seq,
                    bytes,
                    bytes.len() as i64,
                    sha256_hex(bytes),
                    status,
                    expires_at
                ],
            )?;
            Ok(())
        })
        .unwrap();
        format_ap_id(seq)
    }

    fn exit_code<T>(r: Result<T>) -> i32 {
        match r {
            Ok(_) => 0,
            Err(Error::Approval(e)) => e.verify_exit_code().unwrap_or(1),
            Err(other) => panic!("{other:?}"),
        }
    }

    #[test]
    fn unswept_pending_row_past_expiry_reports_expired_not_pending() {
        let (_d, db) = fresh();
        let id = raw_row(&db, 70, "pending", "2026-09-01T00:02:00Z", b"late");
        assert_eq!(exit_code(payload_bytes(&db, &id)), 4);
        assert_eq!(exit_code(verify(&db, &id, &file_src(b"late"))), 4);
        assert_eq!(
            exit_code(consume(&db, &id, &file_src(b"late"), &ctx("x"))),
            4
        );
    }

    #[test]
    fn json_in_force_tracks_the_payload_gate() {
        let (_d, db) = fresh();
        let in_force = |id: &str| get(&db, id).unwrap().to_json(None)["in_force"].clone();
        let pending = raw_row(&db, 80, "pending", "2999-01-01T00:00:00Z", b"p");
        let live = raw_row(&db, 81, "approved", "2999-01-01T00:00:00Z", b"a");
        let stale = raw_row(&db, 82, "approved", "2026-09-01T00:02:00Z", b"s");
        let rejected = raw_row(&db, 83, "rejected", "2999-01-01T00:00:00Z", b"r");
        assert_eq!(in_force(&pending), serde_json::json!(false));
        assert_eq!(in_force(&live), serde_json::json!(true));
        assert_eq!(
            in_force(&stale),
            serde_json::json!(false),
            "approved but expired"
        );
        assert_eq!(in_force(&rejected), serde_json::json!(false));
        consume(&db, &live, &file_src(b"a"), &ctx("x")).unwrap();
        assert_eq!(in_force(&live), serde_json::json!(false), "consumed");
    }

    #[test]
    fn unparseable_expiry_fails_closed() {
        let (_d, db) = fresh();
        let id = raw_row(&db, 71, "approved", "next tuesday", b"when");
        assert_eq!(exit_code(payload_bytes(&db, &id)), 4);
        assert_eq!(exit_code(verify(&db, &id, &file_src(b"when"))), 4);
    }

    #[test]
    fn approval_past_expiry_cannot_be_verified_or_consumed() {
        let (_d, db) = fresh();
        let id = approved_past_expiry(&db, b"late");
        let err = verify(&db, &id, &file_src(b"late")).unwrap_err();
        assert!(
            matches!(&err, Error::Approval(ApprovalError::Terminal(s)) if s.starts_with("expired")),
            "{err:?}"
        );
        let err = consume(&db, &id, &file_src(b"late"), &ctx("hal")).unwrap_err();
        assert!(
            matches!(&err, Error::Approval(e) if e.verify_exit_code() == Some(4)),
            "{err:?}"
        );
        assert!(get(&db, &id).unwrap().consumed_at.is_none());
    }

    #[test]
    fn payload_is_released_only_while_approved_and_unconsumed() {
        let (_d, db) = fresh();
        let code = |r: Result<Vec<u8>>| match r {
            Ok(_) => 0,
            Err(Error::Approval(e)) => e.verify_exit_code().unwrap_or(1),
            Err(other) => panic!("{other:?}"),
        };
        let mk = |body: &[u8]| {
            let input = RequestInput {
                kind: "email".into(),
                title: "x".into(),
                request_note: None,
                payload: file_src(body),
                task_pt_id: None,
                expires_in: None,
            };
            request(&db, input, &ctx("hal")).unwrap().approval.ap_id()
        };
        let decide_as = |id: &str, d: Decision| {
            decide(&db, id, d, DecidedVia::Dashboard, None, &ctx("operator")).unwrap();
        };

        let pending = mk(b"pending");
        assert_eq!(code(payload_bytes(&db, &pending)), 3);
        let rejected = mk(b"rejected");
        decide_as(&rejected, Decision::Reject);
        assert_eq!(code(payload_bytes(&db, &rejected)), 4);
        let approved = mk(b"approved");
        decide_as(&approved, Decision::Approve);
        assert_eq!(payload_bytes(&db, &approved).unwrap(), b"approved");
        consume(&db, &approved, &file_src(b"approved"), &ctx("hal")).unwrap();
        assert_eq!(code(payload_bytes(&db, &approved)), 6);
        let stale = approved_past_expiry(&db, b"late");
        assert_eq!(code(payload_bytes(&db, &stale)), 4);

        // The operator's inspection path ignores status.
        assert_eq!(inspect_payload_bytes(&db, &pending).unwrap(), b"pending");
        assert_eq!(inspect_payload_bytes(&db, &stale).unwrap(), b"late");
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
