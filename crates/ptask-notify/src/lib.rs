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

fn excerpt(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// Body + keyboard for an approval Telegram ping.
pub fn approval_telegram_message(
    ap: &Approval,
    dash_url: Option<&str>,
    tap_buttons: bool,
) -> (String, Vec<Vec<InlineButton>>) {
    // Every free-text field is bounded: Telegram rejects bodies over 4096
    // characters, and a rejected ping stays unnotified on every sweep.
    let preview = excerpt(&ap.preview(), 800);
    let note = excerpt(ap.request_note.as_deref().unwrap_or("").trim(), 1500);
    let digest_prefix: String = ap.digest.chars().take(12).collect();
    let mut text = format!(
        "<b>{}</b> · {} · {}\nRequester: {}\nDigest: {}…\n\nPreview:\n{}",
        html_escape(&ap.ap_id()),
        html_escape(&ap.kind),
        html_escape(&excerpt(&ap.title, 200)),
        html_escape(&excerpt(&ap.requester, 100)),
        html_escape(&digest_prefix),
        html_escape(&preview),
    );
    if !note.is_empty() {
        text.push_str("\n\nRequester's note:\n");
        text.push_str(&html_escape(&note));
    }
    let mut keyboard: Vec<Vec<InlineButton>> = Vec::new();
    if let Some(base) = dash_url.map(str::trim).filter(|s| !s.is_empty()) {
        let url = format!("{base}/#approvals");
        keyboard.push(vec![InlineButton::Url {
            text: "Open inbox".into(),
            url,
        }]);
    }
    if tap_buttons {
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
        // Telegram counts characters after entity parsing.
        let rendered = text
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        assert!(rendered.chars().count() <= 4096, "{}", rendered.len());
        assert!(!text.contains("<b><b>"), "title must be escaped");
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
