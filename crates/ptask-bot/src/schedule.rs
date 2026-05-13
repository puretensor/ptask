//! Morning digest (07:00) + evening recap (18:00) — Europe/London.
//! Concrete content lands in v0.4.4 / v0.4.5; this is the cron-style wiring.

use crate::config::BotConfig;
use anyhow::Result;
use ptask_core::Db;
use teloxide::prelude::*;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::{info, warn};

/// Spawn the cron scheduler. Returns once it's running (or never, if it
/// fails to start — the caller logs and continues).
pub async fn run_scheduler(bot: Bot, db: Db, cfg: BotConfig) -> Result<()> {
    let sched = JobScheduler::new().await?;

    // 07:00 Europe/London — morning digest. tokio_cron_scheduler uses UTC,
    // so we let the wider digest/recap modules (v0.4.4/5) handle tz-aware
    // selection. For now this fires once at 06:00 UTC, which is 07:00 BST
    // and 06:00 GMT — close enough for the placeholder.
    let bot_a = bot.clone();
    let db_a = db.clone();
    let cfg_a = cfg.clone();
    sched
        .add(Job::new_async("0 0 6 * * *", move |_uuid, _l| {
            let bot = bot_a.clone();
            let db = db_a.clone();
            let cfg = cfg_a.clone();
            Box::pin(async move {
                if let Err(e) = morning_digest(&bot, &db, &cfg).await {
                    warn!(target: "ptask::bot", error = %e, "morning digest failed");
                }
            })
        })?)
        .await?;

    // 18:00 Europe/London evening recap.
    let bot_b = bot.clone();
    let db_b = db.clone();
    let cfg_b = cfg.clone();
    sched
        .add(Job::new_async("0 0 17 * * *", move |_uuid, _l| {
            let bot = bot_b.clone();
            let db = db_b.clone();
            let cfg = cfg_b.clone();
            Box::pin(async move {
                if let Err(e) = evening_recap(&bot, &db, &cfg).await {
                    warn!(target: "ptask::bot", error = %e, "evening recap failed");
                }
            })
        })?)
        .await?;

    sched.start().await?;
    info!(target: "ptask::bot", "scheduler started (digest 06:00 UTC, recap 17:00 UTC)");
    // Park forever — JobScheduler runs on its own task; the outer spawn
    // owns this future and aborts it on shutdown.
    std::future::pending::<()>().await;
    Ok(())
}

/// Placeholder — concrete content arrives in v0.4.4.
async fn morning_digest(_bot: &Bot, _db: &Db, _cfg: &BotConfig) -> Result<()> {
    info!(target: "ptask::bot", "morning digest tick (placeholder)");
    Ok(())
}

/// Placeholder — concrete content arrives in v0.4.5.
async fn evening_recap(_bot: &Bot, _db: &Db, _cfg: &BotConfig) -> Result<()> {
    info!(target: "ptask::bot", "evening recap tick (placeholder)");
    Ok(())
}
