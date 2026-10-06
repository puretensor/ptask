//! POST /email — inbound email capture.
//!
//! Accepts a raw RFC 822 message body (Content-Type: `message/rfc822` or
//! `text/plain`). Mail-parser extracts Subject + body; the text is dropped
//! into `raw_items` as a capture with `source='email'`. The native distill
//! pipeline picks it up downstream (until v0.9).
//!
//! For provider-shaped JSON envelopes (Mailgun, Postmark, SendGrid) deploy
//! a tiny forwarder upstream that hands us the raw `.eml` — keeps this
//! endpoint provider-agnostic.

use crate::AppState;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use mail_parser::decoders::base64::base64_decode;
use mail_parser::decoders::quoted_printable::quoted_printable_decode;
use mail_parser::{HeaderName, MessageParser, MessagePart, MimeHeaders, PartType};
use serde::Serialize;
use std::collections::HashMap;

/// Largest accepted message (axum's implicit default, made explicit).
const EMAIL_BODY_LIMIT: usize = 2 * 1024 * 1024;
/// Deepest embedded-message nesting a capture may have. mail-parser builds
/// one nested `Message` per message/rfc822 level with no limit of its own,
/// and its tree conversion and drop recurse once per level: ~10k levels
/// (320 KB) overflowed a 2 MiB thread and aborted pt serve.
const MAX_MESSAGE_DEPTH: usize = 32;
/// Most transfer-encoded embedded messages nested in one another. mail-parser
/// copies a decoded buffer once per message nested in it, so each layer
/// multiplies the work; real gateways add one.
const MAX_ENCODED_LAYERS: usize = 2;
/// Stack for the parse thread, which drops the structure probe's tree: an
/// unencoded chain under EMAIL_BODY_LIMIT nests ~60k deep before the depth
/// check can refuse it (a debug build drops that in under 32 MiB).
const PARSE_STACK_BYTES: usize = 64 << 20;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/email",
        post(email).layer(DefaultBodyLimit::max(EMAIL_BODY_LIMIT)),
    )
}

#[derive(Debug, Serialize)]
pub struct EmailResp {
    pub id: i64,
    pub subject: String,
    pub source_file: String,
}

async fn email(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    crate::blocking::db_response(move || email_blocking(state, headers, body)).await
}

/// The fields a capture keeps, owned so the parsed tree can be dropped on
/// the thread that built it.
struct ParsedEmail {
    subject: String,
    body_text: String,
    message_id: String,
}

enum ParseOutcome {
    Parsed(ParsedEmail),
    Unparseable,
    Refused(&'static str),
}

/// mail-parser's default field table (parsers/header.rs `parse_headers`),
/// except that `Content-Transfer-Encoding` is ignored: the parser then walks
/// every part undecoded, so an encoded embedded message is never decoded
/// and re-parsed. The real parser decodes such a part and copies its whole
/// decoded buffer once per message nested inside it (`into_owned`), so a
/// 2 MB body could demand tens of GB.
fn structure_parser() -> MessageParser {
    MessageParser::new()
        .header_text(HeaderName::Subject)
        .header_text(HeaderName::Comments)
        .header_text(HeaderName::ContentDescription)
        .header_text(HeaderName::ContentLocation)
        .header_address(HeaderName::From)
        .header_address(HeaderName::To)
        .header_address(HeaderName::Cc)
        .header_address(HeaderName::Bcc)
        .header_address(HeaderName::ReplyTo)
        .header_address(HeaderName::Sender)
        .header_address(HeaderName::ResentTo)
        .header_address(HeaderName::ResentFrom)
        .header_address(HeaderName::ResentBcc)
        .header_address(HeaderName::ResentCc)
        .header_address(HeaderName::ResentSender)
        .header_address(HeaderName::ListArchive)
        .header_address(HeaderName::ListHelp)
        .header_address(HeaderName::ListId)
        .header_address(HeaderName::ListOwner)
        .header_address(HeaderName::ListPost)
        .header_address(HeaderName::ListSubscribe)
        .header_address(HeaderName::ListUnsubscribe)
        .header_date(HeaderName::Date)
        .header_date(HeaderName::ResentDate)
        .header_id(HeaderName::MessageId)
        .header_id(HeaderName::References)
        .header_id(HeaderName::InReplyTo)
        .header_id(HeaderName::ReturnPath)
        .header_id(HeaderName::ContentId)
        .header_id(HeaderName::ResentMessageId)
        .header_comma_separated(HeaderName::Keywords)
        .header_comma_separated(HeaderName::ContentLanguage)
        .header_received(HeaderName::Received)
        .header_raw(HeaderName::MimeVersion)
        .header_content_type(HeaderName::ContentType)
        .header_content_type(HeaderName::ContentDisposition)
        .ignore_header(HeaderName::ContentTransferEncoding)
}

/// A part mail-parser parses as an embedded message: message/rfc822 or
/// message/global, or untyped inside a multipart/digest.
fn is_embedded_message(part: &MessagePart<'_>, in_digest: bool) -> bool {
    match part.content_type() {
        Some(ct) => {
            ct.ctype().eq_ignore_ascii_case("message")
                && ct.subtype().is_some_and(|s| {
                    s.eq_ignore_ascii_case("rfc822") || s.eq_ignore_ascii_case("global")
                })
        }
        None => in_digest,
    }
}

/// A part's transfer encoding, read from the raw header bytes (the probe
/// ignored its value).
enum TransferEncoding {
    /// None declared, or 7bit / 8bit / binary.
    Identity,
    Base64,
    QuotedPrintable,
    /// Anything else, which mail-parser leaves undecoded.
    Other,
}

fn transfer_encoding(part: &MessagePart<'_>, raw: &[u8]) -> TransferEncoding {
    let Some(value) = part
        .headers
        .iter()
        .rev()
        .find(|h| h.name == HeaderName::ContentTransferEncoding)
        .and_then(|h| raw.get(h.offset_start as usize..h.offset_end as usize))
        .map(<[u8]>::trim_ascii)
    else {
        return TransferEncoding::Identity;
    };
    let is = |name: &str| value.eq_ignore_ascii_case(name.as_bytes());
    if value.is_empty() || is("7bit") || is("8bit") || is("binary") {
        TransferEncoding::Identity
    } else if is("base64") {
        TransferEncoding::Base64
    } else if is("quoted-printable") {
        TransferEncoding::QuotedPrintable
    } else {
        TransferEncoding::Other
    }
}

/// Probe one buffer's undecoded structure, its root `base_depth` levels
/// down, walking without recursion. Nesting past MAX_MESSAGE_DEPTH is
/// refused. An embedded message in base64 or quoted-printable (RFC 2046
/// 5.2.1 forbids it, but Exchange-style gateways send it) is the one part
/// the real parser decodes and re-parses: it is decoded here and probed in
/// turn, so its nesting counts too. Recursion is bounded by
/// MAX_ENCODED_LAYERS.
fn check_structure(
    bytes: &[u8],
    base_depth: usize,
    encoded_layers: usize,
) -> Result<(), &'static str> {
    // A buffer that doesn't parse is stored by the real parser as an opaque
    // part: nothing nests in it.
    let Some(probe) = structure_parser().parse(bytes) else {
        return Ok(());
    };
    let mut stack = vec![(&probe, base_depth)];
    while let Some((msg, depth)) = stack.pop() {
        if depth > MAX_MESSAGE_DEPTH {
            return Err("embedded messages nested too deeply");
        }
        // Each part's enclosing multipart: its boundary, and whether it is
        // a digest (whose untyped parts are messages).
        let mut parent: HashMap<usize, (Option<&str>, bool)> = HashMap::new();
        for p in &msg.parts {
            if let PartType::Multipart(children) = &p.body {
                let ct = p.content_type();
                let boundary = ct.and_then(|ct| ct.attribute("boundary"));
                let digest = ct.is_some_and(|ct| {
                    ct.subtype()
                        .is_some_and(|s| s.eq_ignore_ascii_case("digest"))
                });
                for &child in children {
                    parent.insert(child as usize, (boundary, digest));
                }
            }
        }
        for (i, part) in msg.parts.iter().enumerate() {
            let (boundary, in_digest) = parent.get(&i).copied().unwrap_or((None, false));
            let encoding = if is_embedded_message(part, in_digest) {
                transfer_encoding(part, bytes)
            } else {
                TransferEncoding::Identity
            };
            let decoded = match encoding {
                TransferEncoding::Identity | TransferEncoding::Other => {
                    if let PartType::Message(inner) = &part.body {
                        stack.push((inner, depth + 1));
                    }
                    continue;
                }
                // The probe saw only the encoded text (as text, or parsed
                // as a junk message); the real structure is in the decoded
                // bytes.
                TransferEncoding::Base64 | TransferEncoding::QuotedPrintable
                    if encoded_layers >= MAX_ENCODED_LAYERS =>
                {
                    return Err("too many transfer-encoded embedded messages");
                }
                TransferEncoding::Base64 => base64_decode(encoded_body(part, bytes, boundary)),
                TransferEncoding::QuotedPrintable => {
                    quoted_printable_decode(encoded_body(part, bytes, boundary))
                }
            };
            if let Some(decoded) = decoded {
                check_structure(&decoded, depth + 1, encoded_layers + 1)?;
            }
        }
    }
    Ok(())
}

/// A part's undecoded body bytes, up to its enclosing multipart's next
/// boundary (the last part's range runs past the closing delimiter), as
/// the real parser's MIME decoders stop there.
fn encoded_body<'b>(part: &MessagePart<'_>, bytes: &'b [u8], boundary: Option<&str>) -> &'b [u8] {
    let body = bytes
        .get(part.offset_body as usize..part.offset_end as usize)
        .unwrap_or_default();
    let Some(boundary) = boundary else {
        return body;
    };
    let delimiter = format!("\n--{boundary}");
    let delimiter = delimiter.as_bytes();
    if body.starts_with(&delimiter[1..]) {
        return &[];
    }
    body.windows(delimiter.len())
        .position(|w| w == delimiter)
        .map_or(body, |end| &body[..end])
}

/// Probe the structure, then parse and extract, on a dedicated big-stack
/// thread: the probe's tree may nest arbitrarily deep before it is refused,
/// and dropping it recurses once per level.
fn parse_email(body: Bytes) -> std::io::Result<ParseOutcome> {
    let worker = std::thread::Builder::new()
        .name("ptask-email-parse".into())
        .stack_size(PARSE_STACK_BYTES)
        .spawn(move || {
            if let Err(why) = check_structure(&body, 0, 0) {
                return ParseOutcome::Refused(why);
            }
            let Some(msg) = MessageParser::default().parse(&body[..]) else {
                return ParseOutcome::Unparseable;
            };
            ParseOutcome::Parsed(ParsedEmail {
                subject: msg.subject().unwrap_or("(no subject)").to_string(),
                body_text: msg.body_text(0).map(|s| s.to_string()).unwrap_or_default(),
                message_id: msg.message_id().unwrap_or("none").to_string(),
            })
        })?;
    // A parser panic is an unparseable message, not a dead request.
    Ok(worker.join().unwrap_or(ParseOutcome::Unparseable))
}

fn email_blocking(state: AppState, headers: HeaderMap, body: Bytes) -> axum::response::Response {
    if let Some(resp) = crate::auth::require_write_token(&state.db, &state.auth, &headers) {
        return resp;
    }
    let ParsedEmail {
        subject,
        body_text,
        message_id,
    } = match parse_email(body) {
        Ok(ParseOutcome::Parsed(p)) => p,
        Ok(ParseOutcome::Refused(why)) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("refused: {why}")})),
            )
                .into_response();
        }
        Ok(ParseOutcome::Unparseable) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "could not parse RFC 822 message"})),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(target: "ptask::email", error = %e, "parse thread spawn failed");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "could not start the parser"})),
            )
                .into_response();
        }
    };

    // Compose the raw_items text: subject + blank line + body. Distill's
    // speech-act classifier handles the rest. Keep it short — anything too
    // long gets truncated by the LLM downstream anyway.
    let text = if body_text.is_empty() {
        subject.clone()
    } else {
        format!("{}\n\n{}", subject, body_text)
    };
    let source_file = format!("email:{}", message_id);

    // Two deliveries of one Message-ID (or two id-less mails with one body) hit
    // the unique index; the idempotent insert answers 200 for the repeat.
    match ptask_core::raw_items::insert_idempotent(&state.db, &text, "email", &source_file) {
        Ok((r, duplicate)) => (
            if duplicate {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            },
            Json(EmailResp {
                id: r.id,
                subject,
                source_file: r.source_file,
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(target: "ptask::email", error = %e, "insert failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{}", e)})),
            )
                .into_response()
        }
    }
}
