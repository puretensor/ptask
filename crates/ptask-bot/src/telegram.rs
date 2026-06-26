//! Minimal Telegram Bot API client.
//!
//! The bot only needs long polling and plain-text sends. Keeping this local
//! avoids teloxide's mandatory `aquamarine` proc-macro dependency, which pulls
//! in `proc-macro-error2` (RUSTSEC-2026-0173).

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Clone)]
pub struct Bot {
    token: Arc<str>,
    client: reqwest::Client,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chat {
    pub id: ChatId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub chat: Chat,
    text: Option<String>,
}

impl Message {
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
}

impl Bot {
    pub fn new(token: &str) -> Self {
        Self {
            token: Arc::from(token.to_string()),
            client: reqwest::Client::new(),
        }
    }

    pub async fn get_updates(&self, offset: Option<i64>, timeout_secs: u64) -> Result<Vec<Update>> {
        let url = self.api_url("getUpdates");
        let mut params = vec![
            ("timeout", timeout_secs.to_string()),
            ("allowed_updates", r#"["message"]"#.to_string()),
        ];
        if let Some(offset) = offset {
            params.push(("offset", offset.to_string()));
        }
        let resp = self
            .client
            .get(&url)
            .query(&params)
            .send()
            .await
            .context("telegram GET getUpdates")?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .context("telegram read getUpdates response")?;
        if !status.is_success() {
            bail!("telegram getUpdates {status}: {body}");
        }
        let parsed: ApiResponse<Vec<ApiUpdate>> =
            serde_json::from_str(&body).context("parse telegram getUpdates")?;
        if !parsed.ok {
            bail!(
                "telegram getUpdates failed: {}",
                parsed.description.unwrap_or_else(|| "unknown error".into())
            );
        }
        Ok(parsed
            .result
            .unwrap_or_default()
            .into_iter()
            .map(Update::from)
            .collect())
    }

    pub async fn send_message(&self, chat_id: ChatId, text: impl Into<String>) -> Result<()> {
        let url = self.api_url("sendMessage");
        let resp = self
            .client
            .post(&url)
            .json(&json!({
                "chat_id": chat_id.0,
                "text": text.into(),
                "disable_web_page_preview": true,
            }))
            .send()
            .await
            .context("telegram POST sendMessage")?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .context("telegram read sendMessage response")?;
        if !status.is_success() {
            bail!("telegram sendMessage {status}: {body}");
        }
        let parsed: ApiResponse<serde_json::Value> =
            serde_json::from_str(&body).context("parse telegram sendMessage")?;
        if !parsed.ok {
            bail!(
                "telegram sendMessage failed: {}",
                parsed.description.unwrap_or_else(|| "unknown error".into())
            );
        }
        Ok(())
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.token, method)
    }
}

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiUpdate {
    update_id: i64,
    message: Option<ApiMessage>,
}

#[derive(Debug, Deserialize)]
struct ApiMessage {
    chat: ApiChat,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiChat {
    id: i64,
}

impl From<ApiUpdate> for Update {
    fn from(value: ApiUpdate) -> Self {
        Self {
            update_id: value.update_id,
            message: value.message.map(Message::from),
        }
    }
}

impl From<ApiMessage> for Message {
    fn from(value: ApiMessage) -> Self {
        Self {
            chat: Chat {
                id: ChatId(value.chat.id),
            },
            text: value.text,
        }
    }
}
