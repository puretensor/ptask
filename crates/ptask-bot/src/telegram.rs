//! Minimal Telegram Bot API client.
//!
//! The bot only needs long polling and plain-text sends. Keeping this local
//! avoids teloxide's mandatory `aquamarine` proc-macro dependency, which pulls
//! in `proc-macro-error2` (RUSTSEC-2026-0173).

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

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
            .timeout(Duration::from_secs(timeout_secs.saturating_add(10).max(15)))
            .send()
            .await
            .map_err(|e| request_error("telegram GET getUpdates", &e))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| request_error("telegram read getUpdates response", &e))?;
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

    /// Send `text`, split into as many messages as Telegram's 4096-char
    /// limit needs. An oversized body is a 400 for the whole message, which
    /// used to drop the morning digest on exactly the days it was longest.
    pub async fn send_message(&self, chat_id: ChatId, text: impl Into<String>) -> Result<()> {
        for chunk in split_for_telegram(&text.into(), TELEGRAM_MAX_CHARS) {
            self.send_one(chat_id, chunk).await?;
        }
        Ok(())
    }

    async fn send_one(&self, chat_id: ChatId, text: String) -> Result<()> {
        let url = self.api_url("sendMessage");
        let resp = self
            .client
            .post(&url)
            .json(&json!({
                "chat_id": chat_id.0,
                "text": text,
                "disable_web_page_preview": true,
            }))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| request_error("telegram POST sendMessage", &e))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| request_error("telegram read sendMessage response", &e))?;
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

/// Telegram's sendMessage limit, in characters.
const TELEGRAM_MAX_CHARS: usize = 4096;

/// Split on line boundaries into chunks of at most `max` chars; a single
/// line longer than `max` is hard-split.
fn split_for_telegram(text: &str, max: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for line in text.split_inclusive('\n') {
        let len = line.chars().count();
        if cur_len + len > max && !cur.is_empty() {
            chunks.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        if len > max {
            let chars: Vec<char> = line.chars().collect();
            for piece in chars.chunks(max) {
                chunks.push(piece.iter().collect());
            }
            continue;
        }
        cur.push_str(line);
        cur_len += len;
    }
    if !cur.is_empty() || chunks.is_empty() {
        chunks.push(cur);
    }
    chunks
}

/// reqwest errors can include the request URL, and Telegram embeds the bot
/// token in that URL. Preserve the useful failure class without exposing the
/// credential through logs or an error response.
fn request_error(operation: &str, error: &reqwest::Error) -> anyhow::Error {
    let kind = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "connection failed"
    } else if error.is_decode() {
        "response decode failed"
    } else {
        "request failed"
    };
    anyhow::anyhow!("{operation}: {kind}")
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

#[cfg(test)]
mod tests {
    use super::split_for_telegram;

    #[test]
    fn short_text_is_one_message() {
        assert_eq!(split_for_telegram("a\nb", 4096), vec!["a\nb".to_string()]);
    }

    #[test]
    fn long_digest_splits_on_line_boundaries_under_the_limit() {
        let row = format!("{}\n", "x".repeat(99));
        let digest = row.repeat(100); // 10,000 chars
        let chunks = split_for_telegram(&digest, 4096);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= 4096));
        assert!(chunks.iter().all(|c| c.ends_with('\n')));
        assert_eq!(chunks.concat(), digest);
    }

    #[test]
    fn a_single_oversized_line_is_hard_split_on_char_boundaries() {
        let line = "é".repeat(9000);
        let chunks = split_for_telegram(&format!("head\n{line}"), 4096);
        assert!(chunks.iter().all(|c| c.chars().count() <= 4096));
        assert_eq!(chunks.concat(), format!("head\n{line}"));
    }
}
