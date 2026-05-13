//! Env-driven bot config.
//!
//! The `from_env()` entry point reads three variables:
//!   PTASK_TELEGRAM_BOT_TOKEN       (required)
//!   PTASK_TELEGRAM_ALLOWED_CHATS   (comma-list of int64; optional → empty)
//!   PTASK_TELEGRAM_DIGEST_CHATS    (comma-list; defaults to allowed[0])
//!
//! Tests target the pure `parse` helper rather than the env-reading shell —
//! mutating shared process env from multiple parallel test threads is a
//! flake source. `from_env` is exercised via the bot's end-to-end startup.

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct BotConfig {
    pub token: String,
    /// `chat_id`s allowed to invoke any command.
    pub allowed: Vec<i64>,
    /// `chat_id`s that receive the morning digest + evening recap.
    /// Defaults to the first allowlisted chat when unset.
    pub digest_chats: Vec<i64>,
}

impl BotConfig {
    pub fn from_env() -> Result<Self> {
        let token = std::env::var("PTASK_TELEGRAM_BOT_TOKEN")
            .context("PTASK_TELEGRAM_BOT_TOKEN required")?;
        let allowed_raw = std::env::var("PTASK_TELEGRAM_ALLOWED_CHATS").unwrap_or_default();
        let digest_raw = std::env::var("PTASK_TELEGRAM_DIGEST_CHATS").unwrap_or_default();
        Self::parse(&token, &allowed_raw, &digest_raw)
    }

    /// Build a config from already-extracted strings. Pure — no env access.
    /// `allowed_raw` / `digest_raw` are comma-separated int64 lists; empty
    /// strings yield empty vectors. Empty digest_chats falls back to the
    /// first entry of `allowed`.
    pub fn parse(token: &str, allowed_raw: &str, digest_raw: &str) -> Result<Self> {
        let allowed = parse_id_list("PTASK_TELEGRAM_ALLOWED_CHATS", allowed_raw)?;
        let mut digest_chats = parse_id_list("PTASK_TELEGRAM_DIGEST_CHATS", digest_raw)?;
        if digest_chats.is_empty()
            && let Some(&first) = allowed.first()
        {
            digest_chats.push(first);
        }
        Ok(Self {
            token: token.to_string(),
            allowed,
            digest_chats,
        })
    }
}

fn parse_id_list(var: &str, raw: &str) -> Result<Vec<i64>> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let s = part.trim();
        if s.is_empty() {
            continue;
        }
        let id: i64 = s
            .parse()
            .with_context(|| format!("{} entry {:?}", var, s))?;
        out.push(id);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_minimal_inputs() {
        let cfg = BotConfig::parse("test-token", "111, 222", "").unwrap();
        assert_eq!(cfg.token, "test-token");
        assert_eq!(cfg.allowed, vec![111, 222]);
        // Digest defaults to first allowed entry.
        assert_eq!(cfg.digest_chats, vec![111]);
    }

    #[test]
    fn parse_explicit_digest_overrides_allowed_default() {
        let cfg = BotConfig::parse("t", "1,2,3", "9").unwrap();
        assert_eq!(cfg.digest_chats, vec![9]);
    }

    #[test]
    fn parse_rejects_non_integer_entries() {
        assert!(BotConfig::parse("t", "abc", "").is_err());
        assert!(BotConfig::parse("t", "1,bogus", "").is_err());
    }

    #[test]
    fn parse_handles_empty_allowed() {
        let cfg = BotConfig::parse("t", "", "").unwrap();
        assert!(cfg.allowed.is_empty());
        assert!(cfg.digest_chats.is_empty());
    }
}
