//! ptask-notify — the network side of accountability dispatch.
//!
//! Implements `ptask_core::accountability::Dispatch` with real
//! Telegram Bot API, SMTP (lettre), and HAL-compose HTTP calls. Extracted
//! from ptask-core in v1.16.0 so the domain crate carries no
//! HTTP/TLS/executor dependencies and its tests never touch the network.
//!
//! Config-missing and dry-run short-circuits are handled by the caller
//! (`run_check_at`); these implementations assume real config and that a
//! live send is wanted.

use ptask_core::accountability::{Dispatch, NudgeRequest, html_escape};
use ptask_core::approvals::Approval;
use ptask_core::config::DispatchCfg;
use ptask_core::{Db, Error, Result};
use tracing::warn;

/// One Telegram inline-keyboard button. Accountability nudges use
/// [`InlineButton::Callback`]; approval messages may mix in URL buttons.
#[derive(Debug, Clone)]
pub enum InlineButton {
    Callback { text: String, data: String },
    Url { text: String, url: String },
}

/// Telegram's message limit, in UTF-16 code units after entity parsing.
const TELEGRAM_MAX_UNITS: usize = 4096;
/// UTF-16 units of payload preview a ping carries. The payload gets the
/// larger share: it is what the operator approves, the note is only the
/// requester's prose. With the title (200) and requester (100) budgets and
/// the fixed labels, header + preview always fits; only the note can push a
/// message over, and it is dropped first.
const PREVIEW_UNITS: usize = 2000;
const NOTE_UNITS: usize = 800;
const TITLE_UNITS: usize = 200;
const REQUESTER_UNITS: usize = 100;

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// At most `max` UTF-16 units of `s` (plus "…" when cut). Cuts on char
/// boundaries, so a surrogate pair is never split.
fn excerpt(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut units = 0;
    for ch in s.chars() {
        units += ch.len_utf16();
        if units > max {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// Characters that render invisibly or reorder the text around them:
/// bidi embeddings/overrides/isolates and the Unicode
/// Default_Ignorable_Code_Point set (zero-width spaces and joiners, word
/// joiner, BOM, soft hyphen, variation selectors, tag characters, ...).
/// A preview containing one can show the operator something other than
/// the bytes being approved.
/// True when the preview holds a character that renders invisibly or
/// misleadingly: default-ignorable / bidi characters, or a control
/// character other than a line break or tab. A lone CR or a backspace can
/// make "pay 400\r9000" read as something else; CRLF line endings (every
/// email) are ordinary.
fn has_deceptive(preview: &str) -> bool {
    let mut chars = preview.chars().peekable();
    while let Some(ch) = chars.next() {
        let benign_control =
            ch == '\n' || ch == '\t' || (ch == '\r' && chars.peek() == Some(&'\n'));
        if is_deceptive(ch) || (ch.is_control() && !benign_control) {
            return true;
        }
    }
    false
}

fn is_deceptive(ch: char) -> bool {
    matches!(ch,
        '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}'
        | '\u{17B4}' | '\u{17B5}' | '\u{180B}'..='\u{180F}'
        | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}'
        | '\u{FEFF}' | '\u{FFA0}' | '\u{FFF0}'..='\u{FFF8}'
        | '\u{1BCA0}'..='\u{1BCA3}' | '\u{1D173}'..='\u{1D17A}'
        | '\u{E0000}'..='\u{E0FFF}')
}

/// Why a ping must not offer tap-to-decide, if it must not.
fn tap_blocker(ap: &Approval, preview: &str) -> Option<&'static str> {
    let readable = ap
        .payload
        .as_deref()
        .is_some_and(|p| std::str::from_utf8(p).is_ok());
    if !readable {
        Some("unseen")
    } else if utf16_len(preview) > PREVIEW_UNITS {
        Some("truncated")
    } else if has_deceptive(preview) {
        Some("deceptive")
    } else {
        None
    }
}

/// True when a Telegram ping for `ap` shows the operator the whole payload,
/// faithfully: stored, UTF-8, within the preview budget, and free of
/// invisible or direction-changing characters. The message builder offers
/// Approve/Reject buttons only then, and `/tg/callback` re-checks it before
/// honouring an Approve tap (pings sent by older builds carried buttons
/// under looser rules).
pub fn tap_decidable(ap: &Approval) -> bool {
    tap_blocker(ap, &ap.preview()).is_none()
}

/// Body + keyboard for an approval Telegram ping.
///
/// Tap-to-decide buttons are offered only when [`tap_decidable`]: a preview
/// cut at [`PREVIEW_UNITS`] could hide a harmful tail behind padding, a
/// binary or digest-only payload shows nothing, and invisible or bidi
/// characters can make the shown text lie. Those pings say so and link to
/// the inbox instead.
pub fn approval_telegram_message(
    ap: &Approval,
    dash_url: Option<&str>,
    tap_buttons: bool,
) -> (String, Vec<Vec<InlineButton>>) {
    // Every free-text field is bounded in UTF-16 units, which is what
    // Telegram counts: a body over 4096 is rejected, and a rejected ping
    // stays unnotified and is retried on every sweep.
    let full_preview = ap.preview();
    let preview_units = utf16_len(&full_preview);
    let mut blocker = tap_blocker(ap, &full_preview);
    let preview = excerpt(&full_preview, PREVIEW_UNITS);
    let digest_prefix: String = ap.digest.chars().take(12).collect();
    let header = format!(
        "<b>{}</b> · {} · {}\nRequester: {}\nDigest: {}…\n\nPreview:\n",
        html_escape(&ap.ap_id()),
        html_escape(&ap.kind),
        html_escape(&excerpt(&ap.title, TITLE_UNITS)),
        html_escape(&excerpt(&ap.requester, REQUESTER_UNITS)),
        html_escape(&digest_prefix),
    );
    let warning = |blocker: Option<&str>| match blocker {
        Some("truncated") => format!(
            "\n\n<b>⚠ PREVIEW TRUNCATED</b>: showing {PREVIEW_UNITS} of {preview_units} \
             units. The rest is not shown here; review the full payload in the \
             inbox. Tap-to-decide is disabled for this request."
        ),
        Some("deceptive") => "\n\n<b>⚠ Payload contains invisible or direction-changing \
             characters</b>, so this preview may not show it faithfully. Review it in the \
             inbox. Tap-to-decide is disabled for this request."
            .into(),
        Some(_) => "\n\n<b>⚠ Payload not shown</b>: review it in the inbox. \
             Tap-to-decide is disabled for this request."
            .into(),
        None => String::new(),
    };
    let note = excerpt(ap.request_note.as_deref().unwrap_or("").trim(), NOTE_UNITS);
    let note_block = if note.is_empty() {
        String::new()
    } else {
        format!("\n\nRequester's note:\n{}", html_escape(&note))
    };
    let mut body = format!("{}{}", html_escape(&preview), warning(blocker));
    let mut text = format!("{header}{body}{note_block}");
    if rendered_units(&text) > TELEGRAM_MAX_UNITS {
        text = format!("{header}{body}");
    }
    if rendered_units(&text) > TELEGRAM_MAX_UNITS {
        // Unreachable with the budgets above; kept so a future budget
        // change cannot produce an undeliverable, decidable ping.
        blocker = Some("truncated");
        let room = TELEGRAM_MAX_UNITS
            .saturating_sub(rendered_units(&header) + rendered_units(&warning(blocker)) + 1);
        body = format!(
            "{}{}",
            html_escape(&excerpt(&full_preview, room)),
            warning(blocker)
        );
        text = format!("{header}{body}");
    }
    let mut keyboard: Vec<Vec<InlineButton>> = Vec::new();
    if let Some(base) = dash_url.map(str::trim).filter(|s| !s.is_empty()) {
        let url = format!("{base}/#approvals");
        keyboard.push(vec![InlineButton::Url {
            text: "Open inbox".into(),
            url,
        }]);
    }
    if tap_buttons && blocker.is_none() {
        keyboard.push(vec![
            InlineButton::Callback {
                text: "Approve".into(),
                data: format!("ptapprove:{}", ap.ap_id()),
            },
            InlineButton::Callback {
                text: "Reject".into(),
                data: format!("ptreject:{}", ap.ap_id()),
            },
        ]);
    }
    (text, keyboard)
}

/// UTF-16 units Telegram counts for an HTML body: entities unescaped, tags
/// (only `<b>` here) removed.
fn rendered_units(html: &str) -> usize {
    utf16_len(
        &html
            .replace("<b>", "")
            .replace("</b>", "")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

/// Send `text` with an arbitrary inline keyboard. `Ok(true)` on HTTP 2xx.
pub async fn send_telegram_markup(
    cfg: &DispatchCfg,
    text: &str,
    keyboard: &[Vec<InlineButton>],
) -> Result<bool> {
    let (Some(token), Some(chat)) = (cfg.telegram_token.as_deref(), cfg.telegram_chat_id) else {
        return Ok(false);
    };
    let base = cfg
        .telegram_api_base
        .as_deref()
        .unwrap_or("https://api.telegram.org");
    let url = format!("{}/bot{}/sendMessage", base, token);
    let mut body = serde_json::json!({"chat_id": chat, "text": text, "parse_mode": "HTML"});
    if !keyboard.is_empty() {
        let rows: Vec<Vec<serde_json::Value>> = keyboard
            .iter()
            .map(|row| {
                row.iter()
                    .map(|b| match b {
                        InlineButton::Callback { text, data } => {
                            serde_json::json!({"text": text, "callback_data": data})
                        }
                        InlineButton::Url { text, url } => {
                            serde_json::json!({"text": text, "url": url})
                        }
                    })
                    .collect()
            })
            .collect();
        body["reply_markup"] = serde_json::json!({"inline_keyboard": rows});
    }
    let client = reqwest::Client::new();
    match client
        .post(url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => Ok(true),
        Ok(r) => {
            warn!(target: "ptask::notify", status = %r.status(), "telegram send failed");
            Ok(false)
        }
        Err(e) => {
            log_telegram_send_error(&e);
            Ok(false)
        }
    }
}

/// Best-effort ping for one approval. Success stamps `notified_at`. Failure
/// never fails the request that triggered it.
pub async fn notify_approval(
    db: &Db,
    cfg: &DispatchCfg,
    dash_url: Option<&str>,
    tap_buttons: bool,
    ap: &Approval,
) -> Result<bool> {
    if cfg.dry_run || !cfg.telegram_configured() {
        return Ok(false);
    }
    let (text, keyboard) = approval_telegram_message(ap, dash_url, tap_buttons);
    let ok = send_telegram_markup(cfg, &text, &keyboard).await?;
    if ok {
        ptask_core::approvals::mark_notified(db, &ap.uuid)?;
    }
    Ok(ok)
}

/// Sweep pending rows with `notified_at` NULL. Used by `pt approval notify`
/// and `pt accountability run`.
pub async fn notify_pending(
    db: &Db,
    cfg: &DispatchCfg,
    dash_url: Option<&str>,
    tap_buttons: bool,
) -> Result<usize> {
    let pending = ptask_core::approvals::pending_unnotified(db)?;
    let mut n = 0usize;
    for ap in pending {
        if notify_approval(db, cfg, dash_url, tap_buttons, &ap).await? {
            n += 1;
        }
    }
    Ok(n)
}

/// Production dispatcher: reqwest for Telegram + HAL, lettre for SMTP.
#[derive(Debug, Default, Clone, Copy)]
pub struct HttpDispatch;

fn log_telegram_send_error(error: &reqwest::Error) {
    let error_kind = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_request() {
        "request"
    } else {
        "other"
    };
    // reqwest's Display can include the request URL. Telegram credentials
    // live in that URL, so only log a safe category.
    warn!(target: "ptask::notify", error_kind, "telegram send error");
}

impl Dispatch for HttpDispatch {
    /// Send `text` via the Telegram Bot API. `Ok(true)` on HTTP 2xx,
    /// `Ok(false)` on network failure / non-2xx (logged). Non-empty
    /// `buttons` render as one inline-keyboard row; taps are forwarded by
    /// nexus (the bot's single `getUpdates` owner) to `POST /tg/callback`.
    async fn send_telegram(
        &self,
        cfg: &DispatchCfg,
        text: &str,
        buttons: &[(String, String)],
    ) -> Result<bool> {
        let keyboard: Vec<Vec<InlineButton>> = if buttons.is_empty() {
            Vec::new()
        } else {
            vec![
                buttons
                    .iter()
                    .map(|(label, data)| InlineButton::Callback {
                        text: label.clone(),
                        data: data.clone(),
                    })
                    .collect(),
            ]
        };
        send_telegram_markup(cfg, text, &keyboard).await
    }

    /// Send a single email via SMTP. CC is mandatory (CLAUDE.md). Returns
    /// `Ok(true)` on send, `Ok(false)` on missing config / network failure.
    async fn send_email(&self, cfg: &DispatchCfg, subject: &str, body: &str) -> Result<bool> {
        let (Some(host), Some(user), Some(pass), Some(to)) = (
            cfg.smtp_host.as_deref(),
            cfg.smtp_user.as_deref(),
            cfg.smtp_pass.as_deref(),
            cfg.notify_email.as_deref(),
        ) else {
            return Ok(false);
        };
        use lettre::message::Mailbox;
        use lettre::transport::smtp::AsyncSmtpTransport;
        use lettre::transport::smtp::authentication::Credentials;
        use lettre::{AsyncTransport, Message, Tokio1Executor};

        let from: Mailbox = format!("HAL <{}>", user)
            .parse()
            .map_err(|e| Error::Other(format!("invalid SMTP_USER address {:?}: {}", user, e)))?;
        let to: Mailbox = to
            .parse()
            .map_err(|e| Error::Other(format!("invalid NOTIFY_EMAIL {:?}: {}", to, e)))?;
        let mut builder = Message::builder().from(from).to(to).subject(subject);
        if let Some(cc) = cfg.cc_email.as_deref() {
            let cc: Mailbox = cc
                .parse()
                .map_err(|e| Error::Other(format!("invalid CC {:?}: {}", cc, e)))?;
            builder = builder.cc(cc);
        }
        let email = builder
            .body(body.to_string())
            .map_err(|e| Error::Other(format!("build email: {}", e)))?;
        let creds = Credentials::new(user.to_string(), pass.to_string());
        let mailer = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
            .map_err(|e| Error::Other(format!("smtp transport: {}", e)))?
            .port(cfg.smtp_port)
            .credentials(creds)
            .build();
        match mailer.send(email).await {
            Ok(_) => Ok(true),
            Err(e) => {
                warn!(target: "ptask::notify", error = %e, "email send failed");
                Ok(false)
            }
        }
    }

    /// Ask HAL to compose the message body. `None` = unavailable/failed.
    async fn compose_via_hal(&self, cfg: &DispatchCfg, req: &NudgeRequest) -> Option<String> {
        let url = cfg.hal_nudge_url.as_deref()?;
        let body = serde_json::json!({
            "task_uuid": req.task_uuid,
            "title": req.title,
            "level": req.level,
            "age_days": req.age_days,
            "dismissal_count": req.dismissal_count,
        });
        let client = reqwest::Client::new();
        let resp = client
            .post(url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: serde_json::Value = resp.json().await.ok()?;
        v.get("message")
            .and_then(|m| m.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    struct LockedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for LockedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for SharedWriter {
        type Writer = LockedWriter;

        fn make_writer(&'a self) -> Self::Writer {
            LockedWriter(Arc::clone(&self.0))
        }
    }

    fn pending_with(payload: Option<Vec<u8>>) -> Approval {
        Approval {
            uuid: "u".into(),
            seq: 9,
            kind: "spend".into(),
            title: "Pay".into(),
            request_note: None,
            payload_kind: payload.as_ref().map(|_| "file".into()),
            payload_bytes: payload.as_ref().map(|p| p.len() as i64),
            payload,
            payload_name: None,
            payload_ref: None,
            digest: "cd".repeat(32),
            requester: "hal".into(),
            task_uuid: None,
            task_pt_id: None,
            status: "pending".into(),
            decided_by: None,
            decided_via: None,
            decision_note: None,
            created_at: "2026-09-25T00:00:00+00:00".into(),
            decided_at: None,
            expires_at: None,
            notified_at: None,
            consumed_at: None,
            consumed_by: None,
        }
    }

    fn has_decide_buttons(keyboard: &[Vec<InlineButton>]) -> bool {
        keyboard
            .iter()
            .flatten()
            .any(|b| matches!(b, InlineButton::Callback { .. }))
    }

    #[test]
    fn tap_to_decide_only_when_the_whole_payload_is_shown() {
        let short = pending_with(Some(b"pay 400 GBP to ACME".to_vec()));
        let (_, kb) = approval_telegram_message(&short, Some("https://d"), true);
        assert!(has_decide_buttons(&kb));

        // Padding pushes the harmful tail past the excerpt.
        let mut padded = "pay 400 GBP to ACME ".repeat(200).into_bytes();
        padded.extend_from_slice(b"AND 90000 GBP TO MALLORY");
        let (text, kb) =
            approval_telegram_message(&pending_with(Some(padded)), Some("https://d"), true);
        assert!(
            !has_decide_buttons(&kb),
            "a truncated preview must not be decidable"
        );
        assert!(text.contains("TRUNCATED"), "{text}");
        assert!(
            kb.iter()
                .flatten()
                .any(|b| matches!(b, InlineButton::Url { .. })),
            "the inbox link stays"
        );

        for unseen in [Some(vec![0xff, 0x00, 0xfe]), None] {
            let (text, kb) = approval_telegram_message(&pending_with(unseen), None, true);
            assert!(!has_decide_buttons(&kb), "{text}");
        }
    }

    #[test]
    fn approval_ping_stays_under_the_telegram_limit_and_escapes() {
        let ap = Approval {
            uuid: "u".into(),
            seq: 7,
            kind: "email".into(),
            title: "<b>".repeat(2_000),
            request_note: Some("&".repeat(5_000)),
            payload: Some(vec![b'x'; 5_000]),
            payload_kind: Some("file".into()),
            payload_name: None,
            payload_bytes: Some(5_000),
            payload_ref: None,
            digest: "ab".repeat(32),
            requester: "hal".repeat(500),
            task_uuid: None,
            task_pt_id: None,
            status: "pending".into(),
            decided_by: None,
            decided_via: None,
            decision_note: None,
            created_at: "2026-09-25T00:00:00+00:00".into(),
            decided_at: None,
            expires_at: None,
            notified_at: None,
            consumed_at: None,
            consumed_by: None,
        };
        let (text, _) = approval_telegram_message(&ap, None, false);
        assert!(telegram_units(&text) <= 4096, "{}", telegram_units(&text));
        assert!(!text.contains("<b><b>"), "title must be escaped");
    }

    /// What Telegram's 4096 limit counts: UTF-16 code units after entity
    /// parsing (tags kept here, so this over-counts slightly).
    fn telegram_units(html: &str) -> usize {
        html.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
            .encode_utf16()
            .count()
    }

    #[test]
    fn astral_text_is_budgeted_in_utf16_units() {
        let mut ap = pending_with(Some("😀".repeat(2_000).into_bytes()));
        ap.title = "😀".repeat(400);
        ap.requester = "😀".repeat(200);
        ap.request_note = Some("😀".repeat(2_000));
        let (text, kb) = approval_telegram_message(&ap, Some("https://d"), true);
        assert!(telegram_units(&text) <= 4096, "{}", telegram_units(&text));
        assert!(!has_decide_buttons(&kb), "4000 units of preview is cut");
        assert!(text.contains("TRUNCATED"));
        assert!(!tap_decidable(&ap));
        // No excerpt splits a surrogate pair (the String would not exist),
        // and the message as a whole stays valid UTF-16.
        assert!(String::from_utf16(&text.encode_utf16().collect::<Vec<_>>()).is_ok());

        // Exactly the budget fits; the note is dropped if it would overflow.
        let mut fits = pending_with(Some("😀".repeat(1_000).into_bytes()));
        fits.title = "😀".repeat(400);
        fits.request_note = Some("😀".repeat(2_000));
        let (text, kb) = approval_telegram_message(&fits, None, true);
        assert!(telegram_units(&text) <= 4096, "{}", telegram_units(&text));
        assert!(has_decide_buttons(&kb));
        assert!(tap_decidable(&fits));
    }

    #[test]
    fn invisible_or_bidi_characters_disable_tap_to_decide() {
        for sneaky in [
            "pay ACME \u{202E}0004\u{202C} GBP",
            "pay \u{2067}ACME\u{2069}",
            "pay\u{200B}ACME",
            "pay ACME\u{2060}",
            "\u{FEFF}pay ACME",
            "pay \u{E0041}ACME",
            // Control characters: a lone CR, backspaces, ESC and C1.
            "pay 400\r9000 GBP to ACME",
            "pay 9000 GBP\x08\x08\x08 to ACME",
            "pay \x1b[2Kto ACME",
            "pay \u{0085}ACME",
        ] {
            let ap = pending_with(Some(sneaky.as_bytes().to_vec()));
            let (text, kb) = approval_telegram_message(&ap, None, true);
            assert!(!has_decide_buttons(&kb), "{sneaky:?}");
            assert!(!tap_decidable(&ap), "{sneaky:?}");
            assert!(text.contains("invisible"), "{text}");
        }
    }

    /// Ordinary multi-line text keeps its buttons: CRLF line endings,
    /// newlines and tabs are not deceptive.
    #[test]
    fn line_breaks_and_tabs_keep_tap_to_decide() {
        let ap = pending_with(Some(
            b"Dear ACME,\r\n\tplease pay 400 GBP.\r\nThanks\n".to_vec(),
        ));
        assert!(tap_decidable(&ap));
    }

    /// Network-level failure surfaces as Ok(false), not Err — the run loop
    /// counts it as a send failure and circuit-breaks. Uses an unroutable
    /// port so no real network is touched.
    #[tokio::test]
    async fn telegram_connection_refused_is_ok_false() {
        let cfg = DispatchCfg {
            telegram_token: Some("test".into()),
            telegram_chat_id: Some(1),
            telegram_api_base: Some("http://127.0.0.1:1".into()),
            ..Default::default()
        };
        let sent = HttpDispatch.send_telegram(&cfg, "x", &[]).await.unwrap();
        assert!(!sent);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn telegram_transport_log_omits_token_and_url() {
        let token = "sentinel-secret-token";
        let error = reqwest::Client::new()
            .post(format!("http://127.0.0.1:1/bot{token}/sendMessage"))
            .send()
            .await
            .unwrap_err();
        let logs = SharedWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_max_level(tracing::Level::WARN)
            .with_writer(logs.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            log_telegram_send_error(&error);
        });
        let output = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        assert!(output.contains("error_kind"), "captured logs: {output:?}");
        assert!(!output.contains(token));
        assert!(!output.contains("/sendMessage"));
    }

    #[tokio::test]
    async fn telegram_unconfigured_is_ok_false() {
        let cfg = DispatchCfg::default();
        assert!(!HttpDispatch.send_telegram(&cfg, "x", &[]).await.unwrap());
        assert!(!HttpDispatch.send_email(&cfg, "s", "b").await.unwrap());
    }
}
