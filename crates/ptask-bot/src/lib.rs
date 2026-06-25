//! pTask Telegram bot.
//!
//! `pt bot` launches a Telegram Bot API long-poll loop. Access is gated by an
//! allowlist of Telegram `chat_id`s — messages from any other chat are
//! ignored. Commands:
//!
//!   /add <quick-add text>       — runs ptask_core::quickadd::parse → create
//!   /list [filter DSL]          — task list, optional Todoist-style filter
//!   /done <PT-N | substring>    — completes / advances recurring
//!   /next [N]                   — DAG-ready tasks
//!
//! The morning digest + evening recap fire on a tokio_cron_scheduler against
//! Europe/London. Config (env-driven):
//!
//!   PTASK_TELEGRAM_BOT_TOKEN       Telegram bot token (required)
//!   PTASK_TELEGRAM_ALLOWED_CHATS   comma-list of int64 chat_ids (required;
//!                                  empty = bot answers no-one)
//!   PTASK_TELEGRAM_DIGEST_CHATS    comma-list of chat_ids that receive the
//!                                  morning digest + evening recap (default
//!                                  = the allowlist's first entry)

mod commands;
mod config;
mod digest;
mod schedule;
mod telegram;

use crate::telegram::Bot;
use anyhow::{Context, Result};
use ptask_core::Db;
use tracing::{info, warn};

pub use commands::PtCommand;
pub use config::BotConfig;

/// Run the bot until cancelled (Ctrl-C). Blocks the current async task.
pub async fn run(db: Db) -> Result<()> {
    let cfg = BotConfig::from_env().context("loading bot config from env")?;
    let bot = Bot::new(&cfg.token);
    info!(
        target: "ptask::bot",
        allowed = cfg.allowed.len(),
        digest = cfg.digest_chats.len(),
        "pt bot starting"
    );

    // Print commands once on startup so journalctl reflects the surface.
    info!(target: "ptask::bot", commands = %PtCommand::descriptions(), "command surface");

    let bot_clone = bot.clone();
    let cfg_clone = cfg.clone();
    let db_clone = db.clone();
    let scheduler_handle = tokio::spawn(async move {
        if let Err(e) = schedule::run_scheduler(bot_clone, db_clone, cfg_clone).await {
            warn!(target: "ptask::bot", error = %e, "scheduler exited");
        }
    });

    let mut offset = None;
    loop {
        tokio::select! {
            _ = shutdown_signal() => break,
            result = bot.get_updates(offset, 30) => {
                match result {
                    Ok(updates) => {
                        for update in updates {
                            offset = Some(update.update_id + 1);
                            let Some(msg) = update.message else {
                                continue;
                            };
                            if !in_allowlist(&cfg, &msg) {
                                continue;
                            }
                            if let Err(e) = commands::dispatch_message(bot.clone(), msg, db.clone()).await {
                                warn!(target: "ptask::bot", error = %e, "command dispatch failed");
                            }
                        }
                    }
                    Err(e) => {
                        warn!(target: "ptask::bot", error = %e, "telegram polling failed; retrying");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        }
    }

    scheduler_handle.abort();
    let _ = scheduler_handle.await;
    Ok(())
}

fn in_allowlist(cfg: &BotConfig, msg: &telegram::Message) -> bool {
    let id = msg.chat.id.0;
    let allowed = cfg.allowed.contains(&id);
    if !allowed {
        // Useful breadcrumb if you forget to add a chat to the allowlist —
        // copy the id from the log line into PTASK_TELEGRAM_ALLOWED_CHATS.
        warn!(target: "ptask::bot", chat_id = id, "ignored message from non-allowlisted chat");
    }
    allowed
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        let mut sig = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            Ok(s) => s,
            Err(_) => return,
        };
        sig.recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    info!(target: "ptask::bot", "shutdown signal received");
}
