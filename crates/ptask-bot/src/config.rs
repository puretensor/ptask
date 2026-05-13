//! Env-driven bot config.

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
        let allowed = parse_id_list("PTASK_TELEGRAM_ALLOWED_CHATS")?;
        let mut digest_chats = parse_id_list("PTASK_TELEGRAM_DIGEST_CHATS")?;
        if digest_chats.is_empty()
            && let Some(&first) = allowed.first()
        {
            digest_chats.push(first);
        }
        Ok(Self {
            token,
            allowed,
            digest_chats,
        })
    }
}

fn parse_id_list(var: &str) -> Result<Vec<i64>> {
    let raw = std::env::var(var).unwrap_or_default();
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
    fn from_env_with_minimal_inputs() {
        // SAFETY: cargo test runs single-threaded by default for this module
        // anyway; these env writes are local.
        unsafe {
            std::env::set_var("PTASK_TELEGRAM_BOT_TOKEN", "test-token");
            std::env::set_var("PTASK_TELEGRAM_ALLOWED_CHATS", "111, 222");
            std::env::remove_var("PTASK_TELEGRAM_DIGEST_CHATS");
        }
        let cfg = BotConfig::from_env().unwrap();
        assert_eq!(cfg.token, "test-token");
        assert_eq!(cfg.allowed, vec![111, 222]);
        // Digest defaults to first allowed entry.
        assert_eq!(cfg.digest_chats, vec![111]);
    }

    #[test]
    fn missing_token_errors() {
        unsafe {
            std::env::remove_var("PTASK_TELEGRAM_BOT_TOKEN");
        }
        assert!(BotConfig::from_env().is_err());
    }
}
