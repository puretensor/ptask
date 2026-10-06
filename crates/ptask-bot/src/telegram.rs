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

    /// Send `text`, split into as many messages as Telegram's 4096-unit
    /// limit needs. An oversized body is a 400 for the whole message, which
    /// used to drop the morning digest on exactly the days it was longest.
    pub async fn send_message(&self, chat_id: ChatId, text: impl Into<String>) -> Result<()> {
        let chunks = split_for_telegram(&text.into(), TELEGRAM_MAX_UTF16);
        send_all(chunks, |chunk| self.send_one(chat_id, chunk)).await
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

/// Send every chunk even when one fails — the first 400 used to drop all
/// the chunks after it — then report how many were lost.
async fn send_all<F, Fut>(chunks: Vec<String>, mut send: F) -> Result<()>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let total = chunks.len();
    let mut failed = 0usize;
    let mut first_error = None;
    for chunk in chunks {
        if let Err(e) = send(chunk).await {
            failed += 1;
            first_error.get_or_insert(e);
        }
    }
    match first_error {
        None => Ok(()),
        Some(e) => Err(e.context(format!(
            "telegram: {failed} of {total} message chunk(s) not sent"
        ))),
    }
}

/// Telegram's sendMessage limit: 4096 characters counted as UTF-16 code
/// units, so an astral emoji (🔥, 🚨, 📅) counts twice.
const TELEGRAM_MAX_UTF16: usize = 4096;

/// Split on line boundaries into chunks of at most `max` UTF-16 code units;
/// a single line longer than `max` is hard-split on char boundaries.
fn split_for_telegram(text: &str, max: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for line in text.split_inclusive('\n') {
        let len = line.encode_utf16().count();
        if cur_len + len > max && !cur.is_empty() {
            chunks.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        if len > max {
            let mut piece = String::new();
            let mut piece_len = 0usize;
            for c in line.chars() {
                if piece_len + c.len_utf16() > max && !piece.is_empty() {
                    chunks.push(std::mem::take(&mut piece));
                    piece_len = 0;
                }
                piece.push(c);
                piece_len += c.len_utf16();
            }
            chunks.push(piece);
            continue;
        }
        cur.push_str(line);
        cur_len += len;
    }
    if !cur.is_empty() || chunks.is_empty() {
        chunks.push(cur);
    }
    // Telegram rejects a whitespace-only message as empty (a hard-split line
    // of exactly `max` units leaves its "\n" alone), and send_all would then
    // report a failure for a message that fully arrived. Such a chunk joins
    // its predecessor only while that stays within `max`; otherwise it is
    // dropped (Telegram strips surrounding whitespace anyway). An
    // all-whitespace message keeps its single chunk.
    let mut merged: Vec<String> = Vec::with_capacity(chunks.len());
    let only = chunks.len() == 1;
    for chunk in chunks {
        if !only && chunk.trim().is_empty() {
            if let Some(prev) = merged.last_mut()
                && prev.encode_utf16().count() + chunk.encode_utf16().count() <= max
            {
                prev.push_str(&chunk);
            }
            continue;
        }
        merged.push(chunk);
    }
    if merged.is_empty() {
        merged.push(String::new());
    }
    merged
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
    use super::{send_all, split_for_telegram};

    fn utf16(s: &str) -> usize {
        s.encode_utf16().count()
    }

    #[test]
    fn chunks_respect_telegrams_utf16_limit() {
        // The morning digest shape: 80 overdue rows, each with an astral 🔥
        // (two UTF-16 units), under the bot's own ☕/🚨 header.
        let mut digest = String::from("☕ pTask digest 2026-10-06\n\n🚨 OVERDUE (80)\n");
        for i in 0..80 {
            digest.push_str(&format!(
                "  PT-{} [critical] 🔥 node{i:02} disk 95% /var/lib/ceph 2026-10-01\n",
                1000 + i
            ));
        }
        let chunks = split_for_telegram(&digest, 4096);
        let sizes: Vec<usize> = chunks.iter().map(|c| utf16(c)).collect();
        assert!(sizes.iter().all(|&n| n <= 4096), "{sizes:?}");
        assert!(chunks.iter().all(|c| c.ends_with('\n')));
        assert_eq!(chunks.concat(), digest);
    }

    #[test]
    fn no_chunk_is_whitespace_only() {
        // A hard-split line of exactly `max` units plus its newline left a
        // chunk of just "\n", which Telegram rejects as an empty message.
        for text in [
            format!("{}\nnext line\n", "a".repeat(4096)),
            format!("{}\n", "🔥".repeat(2048)),
            format!("\n\n{}\n", "b".repeat(5000)),
            format!("head\n{}\n\n\n", "c".repeat(8192)),
        ] {
            let chunks = split_for_telegram(&text, 4096);
            assert!(
                chunks.iter().all(|c| !c.trim().is_empty()),
                "{:?}",
                chunks.iter().map(|c| utf16(c)).collect::<Vec<_>>()
            );
            // Within the limit as sent, before any trimming: the exact-4096
            // line must not become 4097 by gaining its newline.
            let sizes: Vec<usize> = chunks.iter().map(|c| utf16(c)).collect();
            assert!(sizes.iter().all(|&n| n <= 4096), "{sizes:?}");
            // Only whitespace-only chunks are dropped; all other text arrives.
            let ink = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
            assert_eq!(ink(&chunks.concat()), ink(&text));
        }
        // An all-whitespace message is still sent as its one chunk.
        assert_eq!(split_for_telegram("\n\n", 4096), vec!["\n\n".to_string()]);
    }

    #[test]
    fn an_overlong_astral_line_is_hard_split_by_utf16_units() {
        let line = "🔥".repeat(3000); // 6000 UTF-16 units, no newline
        let chunks = split_for_telegram(&format!("head\n{line}"), 4096);
        assert!(chunks.iter().all(|c| utf16(c) <= 4096));
        assert_eq!(chunks.concat(), format!("head\n{line}"));
    }

    #[tokio::test]
    async fn a_failed_chunk_does_not_stop_the_rest() {
        let sent = std::sync::Mutex::new(Vec::new());
        let chunks = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        let result = send_all(chunks, |chunk| {
            let fail = chunk == "two";
            sent.lock().unwrap().push(chunk);
            async move {
                if fail {
                    anyhow::bail!("telegram sendMessage 400 Bad Request: message is too long")
                }
                Ok(())
            }
        })
        .await;
        assert_eq!(*sent.lock().unwrap(), ["one", "two", "three"]);
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("1 of 3") && err.contains("too long"), "{err}");
    }

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
