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

use ptask_core::accountability::{Dispatch, NudgeRequest};
use ptask_core::config::DispatchCfg;
use ptask_core::{Error, Result};
use tracing::warn;

/// Production dispatcher: reqwest for Telegram + HAL, lettre for SMTP.
#[derive(Debug, Default, Clone, Copy)]
pub struct HttpDispatch;

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
        let (Some(token), Some(chat)) = (cfg.telegram_token.as_deref(), cfg.telegram_chat_id)
        else {
            return Ok(false);
        };
        let base = cfg
            .telegram_api_base
            .as_deref()
            .unwrap_or("https://api.telegram.org");
        let url = format!("{}/bot{}/sendMessage", base, token);
        let mut body = serde_json::json!({"chat_id": chat, "text": text, "parse_mode": "HTML"});
        if !buttons.is_empty() {
            let row: Vec<serde_json::Value> = buttons
                .iter()
                .map(|(label, data)| serde_json::json!({"text": label, "callback_data": data}))
                .collect();
            body["reply_markup"] = serde_json::json!({"inline_keyboard": [row]});
        }
        let client = reqwest::Client::new();
        match client.post(url).json(&body).send().await {
            Ok(r) if r.status().is_success() => Ok(true),
            Ok(r) => {
                warn!(target: "ptask::notify", status = %r.status(), "telegram send failed");
                Ok(false)
            }
            Err(e) => {
                warn!(target: "ptask::notify", error = %e, "telegram send error");
                Ok(false)
            }
        }
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

    #[tokio::test]
    async fn telegram_unconfigured_is_ok_false() {
        let cfg = DispatchCfg::default();
        assert!(!HttpDispatch.send_telegram(&cfg, "x", &[]).await.unwrap());
        assert!(!HttpDispatch.send_email(&cfg, "s", "b").await.unwrap());
    }
}
