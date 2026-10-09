//! Flux by actor: who opened and who closed work over a window.
//!
//! The cockpit's flux chip counts tasks added and completed over a window
//! (`+13 / −4`), which is what prompted the operator's rule that a closing
//! pass must not open more work than it closes. The count cannot say *who*
//! opened the thirteen. The journal can: every create, completion,
//! dismissal and reopen carries its actor, so this reads `pt_event_log`
//! and splits the window per actor. `net` is the actor's effect on the
//! open-task count (created + reopened − done − dismissed); a positive net
//! during a closing pass is the rule being broken.
//!
//! Read-only and deterministic: `pt flux`, MCP `task_flux` (an agent can
//! audit its own pass before it reports), the digest and the cockpit.

use crate::error::{Error, Result};
use crate::storage::Db;
use serde::Serialize;

/// Longest window: 90 days.
pub const MAX_WINDOW_MINUTES: i64 = 90 * 24 * 60;

/// One actor's counts over the window.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ActorFlux {
    pub actor: String,
    pub created: i64,
    pub done: i64,
    pub dismissed: i64,
    pub reopened: i64,
    /// created + reopened − done − dismissed: the actor's effect on the
    /// open-task count.
    pub net: i64,
}

/// Flux over a window, per actor (largest net first) and in total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FluxReport {
    /// Window start (UTC) and its length in minutes.
    pub since: String,
    pub window_minutes: i64,
    pub total: ActorFlux,
    pub actors: Vec<ActorFlux>,
}

/// Parse a window like `30m`, `6h`, `24h`, `7d`, `2w` into minutes.
pub fn parse_window(input: &str) -> Result<i64> {
    let s = input.trim().to_ascii_lowercase();
    let unit = s.chars().last().unwrap_or(' ');
    let (num, mult) = match unit {
        'm' => (&s[..s.len() - 1], 1),
        'h' => (&s[..s.len() - 1], 60),
        'd' => (&s[..s.len() - 1], 24 * 60),
        'w' => (&s[..s.len() - 1], 7 * 24 * 60),
        _ => {
            return Err(Error::Other(format!(
                "window {input:?}: expected e.g. 30m, 24h, 7d, 2w"
            )));
        }
    };
    let n: i64 = num
        .trim()
        .parse()
        .map_err(|_| Error::Other(format!("window {input:?}: expected e.g. 30m, 24h, 7d, 2w")))?;
    let minutes = n
        .checked_mul(mult)
        .filter(|m| (1..=MAX_WINDOW_MINUTES).contains(m))
        .ok_or_else(|| Error::Other(format!("window {input:?}: must be 1m..=90d")))?;
    Ok(minutes)
}

/// Flux over the last `window_minutes`.
pub fn by_actor(db: &Db, window_minutes: i64) -> Result<FluxReport> {
    if !(1..=MAX_WINDOW_MINUTES).contains(&window_minutes) {
        return Err(Error::Other(format!(
            "window must be 1..={MAX_WINDOW_MINUTES} minutes"
        )));
    }
    let now = jiff::Zoned::now().with_time_zone(jiff::tz::TimeZone::UTC);
    let since = now
        .checked_sub(jiff::Span::new().minutes(window_minutes))
        .map_err(|e| Error::Other(format!("window math: {e}")))?;
    let since_iso = crate::dates::format_iso(&since);
    let conn = db.get()?;
    // One pass over the window's journal. Event timestamps carry their
    // offset, so julianday() compares instants. Pre-attribution rows (NULL
    // actor) count as "unknown".
    let mut stmt = conn.prepare(
        "SELECT COALESCE(actor, 'unknown') AS who,
                SUM(event_type = 'task.created'),
                SUM(event_type = 'task.completed'),
                SUM(event_type = 'task.updated' AND json_valid(payload)
                    AND json_extract(payload, '$.status') = 'dismissed'),
                SUM(event_type = 'task.updated' AND json_valid(payload)
                    AND json_extract(payload, '$.status') = 'pending')
           FROM pt_event_log
          WHERE task_uuid IS NOT NULL
            AND julianday(ts) >= julianday(?1)
            AND event_type IN ('task.created', 'task.completed', 'task.updated')
          GROUP BY who",
    )?;
    let mut actors: Vec<ActorFlux> = stmt
        .query_map([&since_iso], |r| {
            let mut a = ActorFlux {
                actor: r.get(0)?,
                created: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                done: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                dismissed: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                reopened: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                net: 0,
            };
            a.net = a.created + a.reopened - a.done - a.dismissed;
            Ok(a)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    actors.retain(|a| a.created + a.done + a.dismissed + a.reopened > 0);
    actors.sort_by(|x, y| y.net.cmp(&x.net).then_with(|| x.actor.cmp(&y.actor)));
    let mut total = ActorFlux {
        actor: "total".into(),
        ..Default::default()
    };
    for a in &actors {
        total.created += a.created;
        total.done += a.done;
        total.dismissed += a.dismissed;
        total.reopened += a.reopened;
        total.net += a.net;
    }
    Ok(FluxReport {
        since: since_iso,
        window_minutes,
        total,
        actors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::EventCtx;
    use crate::tasks::{self, NewTask};

    #[test]
    fn flux_splits_the_window_by_actor() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("f.db")).unwrap();
        let hal = EventCtx::local("hal");
        let shell = EventCtx::local("shell");
        // hal: opens three, closes one — a closing pass that grew the backlog.
        let mut hal_tasks = Vec::new();
        for t in ["a", "b", "c"] {
            hal_tasks.push(tasks::create(&db, NewTask::minimal(t), &hal).unwrap());
        }
        tasks::mark_done(&db, &hal_tasks[0], &hal).unwrap();
        // shell: closes two of hal's, dismisses one, reopens one.
        tasks::mark_done(&db, &hal_tasks[1], &shell).unwrap();
        tasks::dismiss(&db, &hal_tasks[2].id, &shell).unwrap();
        tasks::reopen(&db, &hal_tasks[2].id, &shell).unwrap();
        // Out of window: an old create by grok.
        let old = tasks::create(&db, NewTask::minimal("old"), &EventCtx::local("grok")).unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE pt_event_log SET ts = '2020-01-01T00:00:00+00:00' WHERE task_uuid = ?1",
                [&old.id],
            )?;
            Ok(())
        })
        .unwrap();

        let r = by_actor(&db, 60).unwrap();
        let get = |who: &str| r.actors.iter().find(|a| a.actor == who).cloned().unwrap();
        let h = get("hal");
        assert_eq!(
            (h.created, h.done, h.dismissed, h.reopened, h.net),
            (3, 1, 0, 0, 2)
        );
        let s = get("shell");
        assert_eq!(
            (s.created, s.done, s.dismissed, s.reopened, s.net),
            (0, 1, 1, 1, -1)
        );
        assert!(r.actors.iter().all(|a| a.actor != "grok"), "out of window");
        assert_eq!(r.actors[0].actor, "hal", "largest net first");
        assert_eq!((r.total.created, r.total.net), (3, 1));
    }

    #[test]
    fn windows_parse_and_bound() {
        assert_eq!(parse_window("30m").unwrap(), 30);
        assert_eq!(parse_window("24h").unwrap(), 1440);
        assert_eq!(parse_window(" 7D ").unwrap(), 7 * 1440);
        assert_eq!(parse_window("2w").unwrap(), 14 * 1440);
        for bad in ["", "7", "0h", "91d", "-1h", "xh", "99999999999999w"] {
            assert!(parse_window(bad).is_err(), "{bad:?}");
        }
    }
}
