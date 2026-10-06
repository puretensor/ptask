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
use mail_parser::parsers::MessageStream;
use mail_parser::{HeaderName, HeaderValue, MessageParser, MessagePart, MimeHeaders, PartType};
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
/// Concurrent parses allowed. Probing a body at the size limit costs ~40
/// MiB (plus the stack above), and 128 unbounded parallel parses reached
/// 4.7 GiB RSS; beyond this a sender gets 503 with Retry-After.
pub const MAX_CONCURRENT_PARSES: usize = 4;

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
) -> axum::response::Response {
    crate::blocking::db_response(move || email_blocking(state, headers, body))
        .await
        .into_response()
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
    // Read the value exactly as the real parser does: the last header,
    // unstructured (so RFC 2047 encoded words like `=?utf-8?q?base64?=`
    // decode), compared case-insensitively.
    let Some(value) = part
        .headers
        .iter()
        .rev()
        .find(|h| h.name == HeaderName::ContentTransferEncoding)
        .and_then(|h| raw.get(h.offset_start as usize..h.offset_end as usize))
    else {
        return TransferEncoding::Identity;
    };
    let HeaderValue::Text(value) = MessageStream::new(value).parse_unstructured() else {
        return TransferEncoding::Identity;
    };
    let is = |name: &str| value.eq_ignore_ascii_case(name);
    if is("7bit") || is("8bit") || is("binary") {
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
/// the real parser decodes and re-parses: it is decoded here exactly as the
/// real parser decodes it (mail-parser's own MIME decoders, from the part's
/// body offset, stopping at the boundary the parser itself would use) and
/// probed in turn, so its nesting counts too. Recursion is bounded by
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
    // Each entry carries the boundary an unencoded embedded message inherits
    // from its enclosing part: the real parser hands the current multipart's
    // boundary down (`mime_boundary: state.mime_boundary.take()`), and a part
    // with no multipart of its own inside that message decodes against it.
    let mut stack: Vec<(&mail_parser::Message<'_>, usize, Option<&str>)> =
        vec![(&probe, base_depth, None)];
    while let Some((msg, depth, inherited)) = stack.pop() {
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
            let (boundary, in_digest) = match parent.get(&i) {
                Some(&(boundary, digest)) => (boundary, digest),
                None => (inherited, false),
            };
            let encoding = if is_embedded_message(part, in_digest) {
                transfer_encoding(part, bytes)
            } else {
                TransferEncoding::Identity
            };
            let body = bytes.get(part.offset_body as usize..).unwrap_or_default();
            let boundary_bytes = boundary.map(str::as_bytes).unwrap_or(b"");
            let (end, decoded) = match encoding {
                TransferEncoding::Identity | TransferEncoding::Other => {
                    if let PartType::Message(inner) = &part.body {
                        stack.push((inner, depth + 1, boundary));
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
                TransferEncoding::Base64 => {
                    MessageStream::new(body).decode_base64_mime(boundary_bytes)
                }
                TransferEncoding::QuotedPrintable => {
                    MessageStream::new(body).decode_quoted_printable_mime(boundary_bytes)
                }
            };
            // Backstop that does not rely on matching the real decoder: a
            // lenient decode of the same text may hold no more embedded
            // messages than the depth limit allows.
            if embedded_message_markers(&lenient_decode(&encoding, body, boundary))
                > MAX_MESSAGE_DEPTH
            {
                return Err("embedded messages nested too deeply");
            }
            // The real parser keeps an undecodable part as opaque text.
            if end == usize::MAX {
                continue;
            }
            check_structure(&decoded, depth + 1, encoded_layers + 1)?;
        }
    }
    Ok(())
}

/// Case-insensitive count of `message/rfc822` and `message/global` (with
/// any whitespace after the slash) in `bytes`.
fn embedded_message_markers(bytes: &[u8]) -> usize {
    let lower = bytes.to_ascii_lowercase();
    let mut count = 0;
    let mut rest = &lower[..];
    while let Some(at) = rest.windows(8).position(|w| w == b"message/") {
        let after = &rest[at + 8..];
        let trimmed = after
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .map_or(&after[after.len()..], |n| &after[n..]);
        if trimmed.starts_with(b"rfc822") || trimmed.starts_with(b"global") {
            count += 1;
        }
        rest = after;
    }
    count
}

/// A forgiving decode of an encoded part: the text up to the first
/// `--boundary` anywhere, base64 with every non-alphabet byte dropped, or
/// quoted-printable with soft breaks and `=XX` escapes undone. It need not
/// match the real decoder; it only bounds what could be hidden from it.
fn lenient_decode(encoding: &TransferEncoding, body: &[u8], boundary: Option<&str>) -> Vec<u8> {
    let body = match boundary {
        Some(b) if !b.is_empty() => {
            let delimiter = [b"--".as_slice(), b.as_bytes()].concat();
            body.windows(delimiter.len())
                .position(|w| w == delimiter.as_slice())
                .map_or(body, |end| &body[..end])
        }
        _ => body,
    };
    match encoding {
        TransferEncoding::Base64 => {
            let sextet = |c: u8| match c {
                b'A'..=b'Z' => Some(c - b'A'),
                b'a'..=b'z' => Some(c - b'a' + 26),
                b'0'..=b'9' => Some(c - b'0' + 52),
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            };
            let values: Vec<u8> = body.iter().filter_map(|&c| sextet(c)).collect();
            let mut out = Vec::with_capacity(values.len() / 4 * 3 + 3);
            for chunk in values.chunks(4) {
                let mut acc = 0u32;
                for (k, v) in chunk.iter().enumerate() {
                    acc |= u32::from(*v) << (18 - 6 * k);
                }
                let bytes = acc.to_be_bytes();
                out.extend_from_slice(&bytes[1..chunk.len().saturating_sub(1).max(1) + 1]);
            }
            out
        }
        TransferEncoding::QuotedPrintable => {
            let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
            let mut out = Vec::with_capacity(body.len());
            let mut i = 0;
            while i < body.len() {
                if body[i] == b'=' {
                    if let (Some(h), Some(l)) = (
                        body.get(i + 1).copied().and_then(hex),
                        body.get(i + 2).copied().and_then(hex),
                    ) {
                        out.push(h << 4 | l);
                        i += 3;
                        continue;
                    }
                    let soft = body[i + 1..]
                        .iter()
                        .position(|&b| b == b'\n')
                        .filter(|&n| body[i + 1..i + 1 + n].iter().all(u8::is_ascii_whitespace));
                    if let Some(n) = soft {
                        i += n + 2;
                        continue;
                    }
                }
                out.push(body[i]);
                i += 1;
            }
            out
        }
        TransferEncoding::Identity | TransferEncoding::Other => body.to_vec(),
    }
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
    // Authenticate before taking a parse permit: an unauthenticated flood
    // must not hold the permits real senders need.
    if let Some(resp) = crate::auth::require_write_token(&state.db, &state.auth, &headers) {
        return resp;
    }
    // Saturated: tell the sender to retry rather than queue parses behind
    // each other. The permit lives until this blocking task finishes, so a
    // client that disconnects mid-parse does not free it early.
    let Ok(_permit) = state.email_parses.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(axum::http::header::RETRY_AFTER, "5")],
            Json(serde_json::json!({"error": "too many emails being parsed; retry"})),
        )
            .into_response();
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn b64_lines(raw: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD
            .encode(raw)
            .as_bytes()
            .chunks(20)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join("\r\n")
    }

    /// The probe must read an encoded part exactly as the real parser does.
    /// A stray `-` is skipped by mail-parser's MIME decoder (the strict
    /// decoder the probe used before rejected it, so the probe skipped a
    /// part the real parser then decoded and re-parsed).
    #[test]
    fn probe_decodes_like_the_real_parser_despite_a_stray_dash() {
        let inner = b"Subject: inner\r\n\r\nhello\r\n";
        let mut enc = b64_lines(inner);
        enc.insert(7, '-');
        let (end, decoded) = MessageStream::new(enc.as_bytes()).decode_base64_mime(b"");
        assert_ne!(end, usize::MAX);
        assert_eq!(&decoded[..], inner);
    }

    /// The MIME decoder stops at `--boundary` anywhere in a line, not only
    /// at the start of one; the old probe cut only at `\n--boundary`.
    #[test]
    fn probe_stops_at_a_mid_line_boundary() {
        let inner = b"Subject: inner\r\n\r\nhello\r\n";
        let enc = format!("{}--B\r\ntrailing", b64_lines(inner));
        let (end, decoded) = MessageStream::new(enc.as_bytes()).decode_base64_mime(b"B");
        assert_ne!(end, usize::MAX);
        assert_eq!(&decoded[..], inner);
        assert_eq!(
            lenient_decode(&TransferEncoding::Base64, enc.as_bytes(), Some("B")),
            inner
        );
    }

    #[test]
    fn markers_are_counted_case_insensitively_with_whitespace() {
        let text = b"Content-Type: message/rfc822\r\nContent-Type: Message/ RFC822\r\n\
                     content-type: message/global\r\nContent-Type: message/delivery-status\r\n";
        assert_eq!(embedded_message_markers(text), 3);
    }

    /// The backstop sees through either encoding, whatever junk the real
    /// decoder might be made to disagree on.
    #[test]
    fn lenient_decode_sees_through_base64_junk_and_quoted_printable() {
        let inner = b"Content-Type: message/rfc822\r\n\r\nContent-Type: message/rfc822\r\n";
        let junked = b64_lines(inner).replace("\r\n", "-*\r\n");
        let b64 = lenient_decode(&TransferEncoding::Base64, junked.as_bytes(), None);
        assert_eq!(embedded_message_markers(&b64), 2);
        let qp = b"Content-Type: m=65ssage/rfc8=\r\n22\r\n";
        let qp = lenient_decode(&TransferEncoding::QuotedPrintable, qp, None);
        assert_eq!(embedded_message_markers(&qp), 1);
    }

    /// An ordinary encoded forward still passes the probe.
    #[test]
    fn a_shallow_encoded_forward_is_accepted() {
        let inner = b"Subject: forwarded\r\n\r\nplease renew the domain\r\n";
        let raw = format!(
            "Subject: Fwd\r\nContent-Type: message/rfc822\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            b64_lines(inner)
        );
        assert_eq!(check_structure(raw.as_bytes(), 0, 0), Ok(()));
    }
}
