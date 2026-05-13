//! Morning digest (07:00 Europe/London) + evening recap (18:00 Europe/London).
//!
//! No cron crate — we compute the next London-local fire instant via `jiff`,
//! sleep until it, fire, and loop. That keeps DST correct (BST in summer,
//! GMT in winter) without re-deriving the rule via UTC offsets.

use crate::config::BotConfig;
use crate::digest;
use anyhow::Result;
use jiff::Zoned;
use jiff::civil::Time;
use ptask_core::Db;
use ptask_core::dates;
use teloxide::prelude::*;
use tracing::{info, warn};

/// Spawn the digest + recap loops. Returns immediately; loops live on tokio.
pub async fn run_scheduler(bot: Bot, db: Db, cfg: BotConfig) -> Result<()> {
    let bot_a = bot.clone();
    let db_a = db.clone();
    let cfg_a = cfg.clone();
    tokio::spawn(async move {
        run_daily(
            7,
            0,
            bot_a,
            db_a,
            cfg_a,
            "morning digest",
            digest::send_morning,
        )
        .await;
    });

    let bot_b = bot;
    let db_b = db;
    let cfg_b = cfg;
    tokio::spawn(async move {
        run_daily(
            18,
            0,
            bot_b,
            db_b,
            cfg_b,
            "evening recap",
            digest::send_evening,
        )
        .await;
    });

    info!(
        target: "ptask::bot",
        "scheduler started (07:00 + 18:00 Europe/London)"
    );
    Ok(())
}

async fn run_daily<F, Fut>(
    hour: i8,
    minute: i8,
    bot: Bot,
    db: Db,
    cfg: BotConfig,
    label: &'static str,
    send: F,
) where
    F: Fn(Bot, Db, BotConfig) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send,
{
    loop {
        let next = match next_fire(hour, minute) {
            Ok(z) => z,
            Err(e) => {
                warn!(target: "ptask::bot", label, error = %e, "next_fire failed; sleeping 1h");
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                continue;
            }
        };
        let now = match dates::now_in_operator_tz() {
            Ok(z) => z,
            Err(_) => Zoned::now(),
        };
        let wait = (next.timestamp().as_second() - now.timestamp().as_second()).max(0) as u64;
        info!(
            target: "ptask::bot",
            label,
            next = %dates::format_iso(&next),
            "sleeping until next fire"
        );
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        if let Err(e) = send(bot.clone(), db.clone(), cfg.clone()).await {
            warn!(target: "ptask::bot", label, error = %e, "fire failed");
        }
    }
}

/// Next instant in operator tz with the given hour:minute. If today's slot
/// is in the past, returns tomorrow's slot.
fn next_fire(hour: i8, minute: i8) -> Result<Zoned> {
    let now = dates::now_in_operator_tz()?;
    let tz = now.time_zone().clone();
    let time = Time::new(hour, minute, 0, 0)
        .map_err(|e| anyhow::anyhow!("time {:02}:{:02} invalid: {}", hour, minute, e))?;
    let today = now.date().to_zoned(tz.clone())?.with().time(time).build()?;
    if today > now {
        return Ok(today);
    }
    let tomorrow = now
        .date()
        .checked_add(jiff::Span::new().days(1))?
        .to_zoned(tz)?
        .with()
        .time(time)
        .build()?;
    Ok(tomorrow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_fire_is_in_the_future() {
        let z = next_fire(7, 0).unwrap();
        let now = dates::now_in_operator_tz().unwrap();
        assert!(z >= now);
        assert_eq!(z.hour(), 7);
        assert_eq!(z.minute(), 0);
    }
}
