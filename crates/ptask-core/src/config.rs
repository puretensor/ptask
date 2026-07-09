//! Central configuration — the ONE module that reads the process
//! environment.
//!
//! Every binary entrypoint calls [`Config::from_env`] exactly once and
//! threads the pieces to where they're used (axum `AppState`, the
//! accountability dispatcher, and native distill). Library code never touches
//! `std::env` — the v1.x pattern of ~20 ambient reads scattered through
//! auth/storage/accountability/webhooks made behaviour depend on *when* a
//! value was read and made tests non-hermetic on any host that exports
//! `PTASK_API_TOKEN` (i.e. the canonical one).

use std::path::PathBuf;

/// Everything a `pt` process can be configured with.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// SQLite store. `$PTASK_DB`, else `~/puretensor-tasks/tasks.db`.
    pub db_path: PathBuf,
    /// Identity stamped on locally-initiated mutations (`$PTASK_ACTOR`,
    /// default "shell"). The dashboard sidecar sets PTASK_ACTOR=dashboard
    /// on its pt subprocesses; HAL sessions can set PTASK_ACTOR=hal.
    pub actor: String,
    pub auth: AuthConfig,
    pub notify: DispatchCfg,
    pub webhooks: WebhookConfig,
    pub distill: DistillConfig,
    pub dash: DashConfig,
}

/// Triage-cockpit surface served by `pt serve` (v2.3.0 — absorbed from the
/// Python sidecar). Basic auth, NOT bearer tokens: the consumer is a browser.
#[derive(Debug, Clone, Default)]
pub struct DashConfig {
    /// Basic-auth user (`$PTASK_DASH_USER`, default "ops").
    pub user: String,
    /// Basic-auth password (`$PTASK_DASH_PASS`). None = dashboard routes
    /// open (local/dev) — mirror of the sidecar's disabled-if-unset rule.
    pub pass: Option<String>,
    /// Static www dir (`$PTASK_DASH_WWW`, default ~/ptask/dashboard/www).
    pub www_dir: PathBuf,
    /// Voice shim passthrough (`$PTASK_VOICE_SHIM_URL`, default
    /// http://127.0.0.1:9510) — /api/voice proxies here (STT stays Python).
    pub voice_shim_url: String,
}

/// API-token material for `pt serve` (enforce-if-configured).
#[derive(Debug, Clone, Default)]
pub struct AuthConfig {
    /// Write token — gates `/sync`, `/capture`, `/email` (and reads, as a
    /// superset credential). `None` = unauthenticated back-compat mode.
    pub api_token: Option<String>,
    /// Read-only token accepted by `/metrics` and the read routes, so a
    /// Prometheus scraper doesn't hold the fleet-wide write token.
    pub metrics_token: Option<String>,
    /// Operator escape hatch for deliberately isolated deployments.
    pub allow_unauthenticated: bool,
}

/// Outbound + inbound webhook secrets/targets for `pt serve`.
#[derive(Debug, Clone, Default)]
pub struct WebhookConfig {
    /// POST targets for outbound event fan-out (comma-separated env).
    pub outbound_urls: Vec<String>,
    /// HMAC-SHA256 secret for outbound signatures. Empty = unsigned.
    pub outbound_secret: String,
    /// Shared secret verifying inbound Gitea `X-Gitea-Signature`.
    pub gitea_secret: String,
    /// Shared secret verifying inbound GitHub `X-Hub-Signature-256`.
    pub github_secret: String,
}

/// Native distillation configuration.
#[derive(Debug, Clone, Default)]
pub struct DistillConfig {
    /// Gemini API key for the native pipeline ($GOOGLE_API_KEY).
    pub gemini_api_key: Option<String>,
    /// Gemini model id ($GEMINI_CONSOLIDATE_MODEL, default gemini-3.5-flash).
    pub gemini_model: String,
}

/// Configuration the accountability dispatcher needs. Plain data — the
/// network implementations live in `ptask-notify`; core only decides *what*
/// to send and records the outcome.
#[derive(Debug, Clone, Default)]
pub struct DispatchCfg {
    pub telegram_token: Option<String>,
    pub telegram_chat_id: Option<i64>,
    pub smtp_host: Option<String>,
    pub smtp_port: u16,
    pub smtp_user: Option<String>,
    pub smtp_pass: Option<String>,
    pub notify_email: Option<String>,
    /// Always CC'd on every outbound email per CLAUDE.md.
    pub cc_email: Option<String>,
    pub hal_nudge_url: Option<String>,
    /// Telegram Bot API base override (tests point this at an unroutable
    /// address). `None` means the real `https://api.telegram.org`.
    pub telegram_api_base: Option<String>,
    /// Suppress side-effecting sends. Tests set this; production never does.
    pub dry_run: bool,
}

impl DispatchCfg {
    /// True when both pieces of Telegram config are present — a failed send
    /// under this condition is a real delivery failure, not missing config.
    pub fn telegram_configured(&self) -> bool {
        self.telegram_token.is_some() && self.telegram_chat_id.is_some()
    }

    /// True when the SMTP quadruple required by email dispatch is present.
    pub fn email_configured(&self) -> bool {
        self.smtp_host.is_some()
            && self.smtp_user.is_some()
            && self.smtp_pass.is_some()
            && self.notify_email.is_some()
    }
}

impl Config {
    /// Read the full configuration from the process environment. Call once
    /// per binary, at the entrypoint.
    pub fn from_env() -> Self {
        Config {
            db_path: env_db_path(),
            actor: env_nonempty("PTASK_ACTOR").unwrap_or_else(|| "shell".into()),
            auth: AuthConfig {
                api_token: env_nonempty("PTASK_API_TOKEN"),
                metrics_token: env_nonempty("PTASK_METRICS_TOKEN"),
                allow_unauthenticated: env_truthy("PTASK_ALLOW_UNAUTHENTICATED"),
            },
            notify: DispatchCfg {
                telegram_token: env_first(&["PTASK_TELEGRAM_BOT_TOKEN", "TELEGRAM_BOT_TOKEN"]),
                telegram_chat_id: env_first(&[
                    "PTASK_ACCOUNTABILITY_CHAT_ID",
                    "PTASK_TELEGRAM_DIGEST_CHATS",
                    "TELEGRAM_CHAT_ID",
                ])
                .and_then(|s| s.split(',').next()?.trim().parse().ok()),
                smtp_host: env_first(&["PTASK_SMTP_HOST", "SMTP_HOST"]),
                smtp_port: env_first(&["PTASK_SMTP_PORT", "SMTP_PORT"])
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(587),
                smtp_user: env_first(&["PTASK_SMTP_USER", "SMTP_USER"]),
                smtp_pass: env_first(&["PTASK_SMTP_PASS", "SMTP_PASS"]),
                notify_email: env_first(&["PTASK_NOTIFY_EMAIL", "NOTIFY_EMAIL"]),
                cc_email: env_first(&["PTASK_NOTIFY_CC", "PTASK_OPS_EMAIL"])
                    .or_else(|| Some("ops@puretensor.ai".to_string())),
                hal_nudge_url: env_nonempty("PTASK_HAL_NUDGE_URL"),
                telegram_api_base: env_nonempty("PTASK_TELEGRAM_API_BASE"),
                dry_run: env_truthy("PTASK_ACCOUNTABILITY_DRY_RUN"),
            },
            webhooks: WebhookConfig {
                outbound_urls: std::env::var("PTASK_WEBHOOK_URLS")
                    .ok()
                    .map(|s| {
                        s.split(',')
                            .map(|p| p.trim().to_string())
                            .filter(|p| !p.is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
                outbound_secret: std::env::var("PTASK_WEBHOOK_SECRET").unwrap_or_default(),
                gitea_secret: std::env::var("PTASK_GITEA_WEBHOOK_SECRET").unwrap_or_default(),
                github_secret: std::env::var("PTASK_GITHUB_WEBHOOK_SECRET").unwrap_or_default(),
            },
            distill: DistillConfig {
                gemini_api_key: env_nonempty("GOOGLE_API_KEY"),
                gemini_model: env_nonempty("GEMINI_CONSOLIDATE_MODEL")
                    .unwrap_or_else(|| "gemini-3.5-flash".into()),
            },
            dash: DashConfig {
                user: env_nonempty("PTASK_DASH_USER").unwrap_or_else(|| "ops".into()),
                pass: env_nonempty("PTASK_DASH_PASS"),
                www_dir: std::env::var("PTASK_DASH_WWW")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| home_dir().join("ptask").join("dashboard").join("www")),
                voice_shim_url: env_nonempty("PTASK_VOICE_SHIM_URL")
                    .unwrap_or_else(|| "http://127.0.0.1:9510".into()),
            },
        }
    }
}

/// Default DB path: `$PTASK_DB`, else `~/puretensor-tasks/tasks.db` (the
/// original Python store location).
pub fn env_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("PTASK_DB") {
        return PathBuf::from(p);
    }
    home_dir().join("puretensor-tasks").join("tasks.db")
}

pub fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into()))
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|s| matches!(s.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn env_first(names: &[&str]) -> Option<String> {
    for n in names {
        if let Ok(v) = std::env::var(n)
            && !v.trim().is_empty()
        {
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_cfg_configured_helpers() {
        let mut cfg = DispatchCfg {
            telegram_token: Some("t".into()),
            telegram_chat_id: Some(1),
            ..Default::default()
        };
        assert!(cfg.telegram_configured());
        assert!(!cfg.email_configured());
        cfg.smtp_host = Some("h".into());
        cfg.smtp_user = Some("u".into());
        cfg.smtp_pass = Some("p".into());
        cfg.notify_email = Some("e@x".into());
        assert!(cfg.email_configured());
    }

    #[test]
    fn default_config_is_inert() {
        let cfg = Config::default();
        assert!(cfg.actor.is_empty());
        assert!(cfg.auth.api_token.is_none());
        assert!(!cfg.notify.telegram_configured());
        assert!(cfg.webhooks.outbound_urls.is_empty());
    }
}
