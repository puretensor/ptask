//! Claim ownership, leases and recovery.
//!
//! A claim is the atomic todo/backlog/triage → in_progress flip
//! ([`claim`]) plus who holds it (`tasks.claimed_by`) and, when the claimer
//! asks for one, a lease (`tasks.claim_expires_at`). Each take also mints an
//! opaque instance token (`tasks.claim_token`): [`heartbeat`] and unforced
//! [`release`] compare-and-set on that token, so two sessions that share an
//! actor name cannot renew or hand back each other's claim. A heartbeat on a
//! token that is no longer current fails, which is the signal to stop
//! working. [`release`] hands a task back to todo without closing it.
//! [`reclaim_expired`] returns tasks whose lease ran out to todo: `pt reclaim`
//! runs it on demand (a dry run unless `--apply`), and the hourly scoring run
//! only when the operator turns `PTASK_CLAIM_RECLAIM` on.
//!
//! An expired lease is free: [`claim`] takes it over, and `pt start` takes
//! the holder and clears the lease. Leaving in_progress by any other path
//! (done, dismiss, snooze, a recurring advance) drops the claim in a trigger
//! (V021), so a claim never outlives its work. A claim without a lease never
//! expires on its own.

use crate::error::{Error, Result};
use crate::event_log::EventCtx;
use crate::storage::Db;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Longest lease a claim or heartbeat may ask for: a day. Longer work
/// renews; a lease is a liveness promise, not a reservation.
pub const MAX_LEASE_MINUTES: i64 = 24 * 60;

/// Who holds a task and until when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub by: String,
    pub at: Option<String>,
    /// Lease end (UTC); `None` = no lease, the claim never expires.
    pub expires_at: Option<String>,
    /// True when the lease has run out (the holder stopped heartbeating).
    #[serde(default)]
    pub expired: bool,
    /// Opaque instance token minted when this claim was taken. Present on
    /// [`claim`] (and on `pt start` when it takes the holder); never loaded
    /// by [`get`], never journaled, never shown.
    #[serde(skip)]
    pub token: String,
}

/// What [`release`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Released {
    pub task_uuid: String,
    pub pt_id: Option<String>,
    /// The claim that was dropped, if the task had one.
    pub holder: Option<String>,
    /// True when the caller released someone else's claim (`--force`).
    pub forced: bool,
}

/// One claim whose lease has run out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpiredClaim {
    pub task_uuid: String,
    pub pt_id: Option<String>,
    pub title: String,
    pub holder: String,
    pub claimed_at: Option<String>,
    pub expired_at: String,
}

/// A reclaim pass: what was (or, dry-run, would be) returned to todo.
#[derive(Debug, Clone, Serialize)]
pub struct ReclaimReport {
    pub dry_run: bool,
    pub reclaimed: Vec<ExpiredClaim>,
    /// Expired at scan time but changed before the write (a late heartbeat,
    /// a close): left alone.
    pub skipped: usize,
}

fn validate_lease(minutes: i64) -> Result<()> {
    if !(1..=MAX_LEASE_MINUTES).contains(&minutes) {
        return Err(Error::Other(format!(
            "lease must be 1..={MAX_LEASE_MINUTES} minutes, got {minutes}"
        )));
    }
    Ok(())
}

fn utc_now() -> jiff::Zoned {
    jiff::Zoned::now().with_time_zone(jiff::tz::TimeZone::UTC)
}

fn lease_end(now: &jiff::Zoned, minutes: i64) -> Result<String> {
    validate_lease(minutes)?;
    let end = now
        .checked_add(jiff::Span::new().minutes(minutes))
        .map_err(|e| Error::Other(format!("lease math: {e}")))?;
    Ok(crate::dates::format_iso(&end))
}

fn is_expired(expires_at: Option<&str>, now: &jiff::Zoned) -> bool {
    expires_at
        .and_then(crate::dates::parse_iso_to_utc)
        .is_some_and(|end| end.timestamp() <= now.timestamp())
}

/// True when `expires_at` is a lease that has already run out.
pub(crate) fn lease_has_expired(expires_at: Option<&str>) -> bool {
    is_expired(expires_at, &utc_now())
}

/// Mint an opaque claim-instance token. New on every take.
pub(crate) fn new_claim_token() -> String {
    format!("ct_{}", uuid::Uuid::new_v4().simple())
}

fn presented_token(token: Option<&str>) -> Option<&str> {
    token.map(str::trim).filter(|t| !t.is_empty())
}

/// Parse a lease like `30m`, `2h`, `90` (minutes) or `1d`.
pub fn parse_lease(input: &str) -> Result<i64> {
    let s = input.trim().to_ascii_lowercase();
    let (num, mult) = match s.chars().last() {
        Some('m') => (&s[..s.len() - 1], 1),
        Some('h') => (&s[..s.len() - 1], 60),
        Some('d') => (&s[..s.len() - 1], 24 * 60),
        _ => (s.as_str(), 1),
    };
    let n: i64 = num
        .trim()
        .parse()
        .map_err(|_| Error::Other(format!("lease {input:?}: expected e.g. 30m, 2h, 1d")))?;
    let minutes = n
        .checked_mul(mult)
        .ok_or_else(|| Error::Other(format!("lease {input:?} is out of range")))?;
    validate_lease(minutes)?;
    Ok(minutes)
}

/// The instance token of the live claim on `task_uuid`, if any. [`get`]
/// does not load it and `pt show` does not show it; claim-next returns it
/// so the claimer can heartbeat without a second round trip.
pub fn instance_token(db: &Db, task_uuid: &str) -> Result<Option<String>> {
    let conn = db.get()?;
    let token: Option<String> = conn
        .query_row(
            "SELECT claim_token FROM tasks
              WHERE id=?1 AND status_v2='in_progress' AND claim_token IS NOT NULL",
            [task_uuid],
            |r| r.get(0),
        )
        .optional()?;
    Ok(token.filter(|t| !t.is_empty()))
}

/// The claim on a task, if any.
pub fn get(db: &Db, task_uuid: &str) -> Result<Option<Claim>> {
    let conn = db.get()?;
    get_in_conn(&conn, task_uuid)
}

/// [`get`] on an existing connection.
pub fn get_in_conn(conn: &rusqlite::Connection, task_uuid: &str) -> Result<Option<Claim>> {
    let row: Option<(Option<String>, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT claimed_by, claimed_at, claim_expires_at FROM tasks
              WHERE id = ?1 AND status_v2 = 'in_progress'",
            [task_uuid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let now = utc_now();
    Ok(row.and_then(|(by, at, expires_at)| {
        by.map(|by| Claim {
            expired: is_expired(expires_at.as_deref(), &now),
            by,
            at,
            expires_at,
            token: String::new(),
        })
    }))
}

/// Atomically claim a task for the caller (todo/backlog/triage →
/// in_progress), optionally with a lease of `lease_minutes`. An in-progress
/// task whose lease has run out is free: this takes the holder and installs
/// a new instance token. The guarded flip, the owner and the `task.claimed`
/// event are one transaction, so two claimers cannot both win and a claim is
/// never without its record.
pub fn claim(
    db: &Db,
    task_uuid: &str,
    lease_minutes: Option<i64>,
    ctx: &EventCtx,
) -> Result<Claim> {
    let now = utc_now();
    let at = crate::dates::format_iso(&now);
    let expires_at = lease_minutes.map(|m| lease_end(&now, m)).transpose()?;
    let token = new_claim_token();
    let mut conn = db.get()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE tasks SET status_v2='in_progress', status='pending', updated_at=?1,
                          claimed_by=?2, claimed_at=?1, claim_expires_at=?3, claim_token=?4
         WHERE id=?5 AND status_v2 IN ('triage','backlog','todo')",
        params![
            crate::tasks::iso_now(),
            ctx.actor,
            expires_at,
            token,
            task_uuid
        ],
    )?;
    if changed == 0 {
        let row: Option<(String, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT status_v2, claimed_by, claim_expires_at FROM tasks WHERE id=?1",
                [task_uuid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((status, holder, old_expires)) = row else {
            return Err(Error::Other("task not found or not claimable".into()));
        };
        let expired = is_expired(old_expires.as_deref(), &now);
        if status == "in_progress" && expired {
            let took = tx.execute(
                "UPDATE tasks SET status_v2='in_progress', status='pending', updated_at=?1,
                                  claimed_by=?2, claimed_at=?1, claim_expires_at=?3, claim_token=?4
                 WHERE id=?5 AND status_v2='in_progress' AND claim_expires_at IS ?6",
                params![
                    crate::tasks::iso_now(),
                    ctx.actor,
                    expires_at,
                    token,
                    task_uuid,
                    old_expires
                ],
            )?;
            if took == 0 {
                return Err(Error::Other(match holder {
                    Some(h) => {
                        format!("task not claimable: already claimed by {h} (lease expired)")
                    }
                    None => "task not claimable: lease expired".into(),
                }));
            }
        } else {
            return Err(Error::Other(match (status.as_str(), holder) {
                ("in_progress", Some(h)) => {
                    format!("task not claimable: already claimed by {h}")
                }
                _ => "task not found or not claimable".into(),
            }));
        }
    }
    let mut payload = serde_json::json!({ "task_uuid": task_uuid, "by": ctx.actor });
    if let (Some(m), Some(e)) = (lease_minutes, &expires_at) {
        payload["lease_minutes"] = serde_json::json!(m);
        payload["expires_at"] = serde_json::json!(e);
    }
    crate::tasks::record_event_tx(&tx, ctx, task_uuid, "task.claimed", &payload)?;
    tx.commit()?;
    Ok(Claim {
        by: ctx.actor.clone(),
        at: Some(at),
        expires_at,
        expired: false,
        token,
    })
}

/// Keep a claim alive: push its lease to now + `lease_minutes`. Only the
/// holder of *this* claim instance may (`claim_token` from [`claim`] or
/// `pt start`), and only while the task is still in progress under that
/// token. Anything else (released, reclaimed, closed, taken by another,
/// a stale session of the same actor) is an error that tells the caller to
/// stop working. A lease that has run out but was not reclaimed yet is
/// still that instance's to renew. A heartbeat with no token is refused.
///
/// Not journaled: a heartbeat every few minutes would flood the journal and
/// every sync client's delta, and it changes no task state.
pub fn heartbeat(
    db: &Db,
    task_uuid: &str,
    lease_minutes: i64,
    claim_token: &str,
    ctx: &EventCtx,
) -> Result<Claim> {
    let Some(claim_token) = presented_token(Some(claim_token)) else {
        return Err(Error::Other(
            "claim lost: claim token required; stop work on it (re-claim if it is still yours to do)"
                .into(),
        ));
    };
    let now = utc_now();
    let expires_at = lease_end(&now, lease_minutes)?;
    let mut conn = db.get()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let current = tx
        .query_row(
            "SELECT status_v2, claimed_by, claimed_at, claim_token FROM tasks WHERE id=?1",
            [task_uuid],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((status, holder, at, stored_token)) = current else {
        return Err(Error::Other("claim lost: task not found; stop work".into()));
    };
    let token_ok = stored_token.as_deref() == Some(claim_token);
    let actor_ok = holder
        .as_deref()
        .is_some_and(|h| h.eq_ignore_ascii_case(&ctx.actor));
    let mine = status == "in_progress" && actor_ok && token_ok;
    if !mine {
        let why = match (status.as_str(), holder.as_deref(), token_ok) {
            ("in_progress", Some(h), true) => format!("now claimed by {h}"),
            ("in_progress", Some(_), false) => "this claim is no longer current".to_string(),
            ("in_progress", None, _) => "the claim was released".to_string(),
            (s, _, _) => format!("the task is {s}"),
        };
        return Err(Error::Other(format!(
            "claim lost: {why}; stop work on it (re-claim if it is still yours to do)"
        )));
    }
    tx.execute(
        "UPDATE tasks SET claim_expires_at=?1 WHERE id=?2 AND claim_token=?3",
        params![expires_at, task_uuid, claim_token],
    )?;
    tx.commit()?;
    Ok(Claim {
        by: holder.unwrap_or_default(),
        at,
        expires_at: Some(expires_at),
        expired: false,
        token: claim_token.to_string(),
    })
}

/// Hand a task back: in_progress → todo, claim dropped, nothing closed.
/// The holder of *this* claim instance may release it by presenting
/// `claim_token`. An unowned in-progress task, a stale token, a missing
/// token, or someone else's claim, only with `force` (the operator's
/// override, journaled as forced). There is no force over MCP.
/// `reason` is journaled with the release.
pub fn release(
    db: &Db,
    task_uuid: &str,
    force: bool,
    reason: Option<&str>,
    claim_token: Option<&str>,
    ctx: &EventCtx,
) -> Result<Released> {
    let reason = reason.map(str::trim).filter(|r| !r.is_empty());
    if let Some(r) = reason
        && r.chars().count() > crate::approvals::MAX_NOTE_CHARS
    {
        return Err(Error::Other(format!(
            "reason exceeds {} characters",
            crate::approvals::MAX_NOTE_CHARS
        )));
    }
    let mut conn = db.get()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let current = tx
        .query_row(
            "SELECT status_v2, claimed_by, pt_id, claim_token FROM tasks WHERE id=?1",
            [task_uuid],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((status, holder, pt_id, stored_token)) = current else {
        return Err(Error::Other("task not found".into()));
    };
    if status != "in_progress" {
        return Err(Error::Other(format!(
            "task is {status}, not in progress: nothing to release"
        )));
    }
    let presented = presented_token(claim_token);
    let token_ok = matches!((presented, stored_token.as_deref()), (Some(p), Some(s)) if p == s);
    let actor_ok = holder
        .as_deref()
        .is_some_and(|h| h.eq_ignore_ascii_case(&ctx.actor));
    let authorized = token_ok && actor_ok;
    if !authorized && !force {
        let others = holder
            .as_deref()
            .is_some_and(|h| !h.eq_ignore_ascii_case(&ctx.actor));
        if others {
            return Err(Error::Other(format!(
                "claimed by {}, not you: only the holder releases it (the operator can with --force)",
                holder.as_deref().unwrap_or_default()
            )));
        }
        return Err(Error::Other(
            "only the holder of this claim can release it (the operator can with --force)".into(),
        ));
    }
    let forced = !authorized;
    // The trigger clears the claim columns as the status leaves in_progress.
    tx.execute(
        "UPDATE tasks SET status_v2='todo', status='pending', updated_at=?1
          WHERE id=?2 AND status_v2='in_progress'",
        params![crate::tasks::iso_now(), task_uuid],
    )?;
    let mut payload = serde_json::json!({
        "task_uuid": task_uuid, "pt_id": pt_id, "status": "todo",
        "holder": holder, "forced": forced,
    });
    if let Some(r) = reason {
        payload["reason"] = serde_json::json!(r);
    }
    crate::tasks::record_event_tx(&tx, ctx, task_uuid, "task.released", &payload)?;
    tx.commit()?;
    Ok(Released {
        task_uuid: task_uuid.to_string(),
        pt_id,
        holder,
        forced,
    })
}

/// In-progress tasks whose lease has run out, oldest expiry first.
pub fn expired(db: &Db) -> Result<Vec<ExpiredClaim>> {
    let conn = db.get()?;
    expired_in_conn(&conn)
}

fn expired_in_conn(conn: &rusqlite::Connection) -> Result<Vec<ExpiredClaim>> {
    let now = utc_now();
    let mut stmt = conn.prepare(
        "SELECT id, pt_id, title, claimed_by, claimed_at, claim_expires_at FROM tasks
          WHERE status_v2 = 'in_progress' AND claimed_by IS NOT NULL
            AND claim_expires_at IS NOT NULL",
    )?;
    let mut rows: Vec<(jiff::Timestamp, ExpiredClaim)> = stmt
        .query_map([], |r| {
            Ok(ExpiredClaim {
                task_uuid: r.get(0)?,
                pt_id: r.get(1)?,
                title: r.get(2)?,
                holder: r.get(3)?,
                claimed_at: r.get(4)?,
                expired_at: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        // An unparseable expiry is not "expired": recovery acts on state, so
        // it fails closed, like the reaper's timestamp guards.
        .filter_map(|c| {
            let end = crate::dates::parse_iso_to_utc(&c.expired_at)?.timestamp();
            (end <= now.timestamp()).then_some((end, c))
        })
        .collect();
    rows.sort_by_key(|(end, _)| *end);
    Ok(rows.into_iter().map(|(_, c)| c).collect())
}

/// Return expired claims to todo. Each is re-checked under the write lock
/// (same holder, same lease end, still in progress), so a heartbeat or a
/// close that lands after the scan wins. Journaled `task.reclaimed` with
/// the holder and the lease end. `dry_run` lists without writing.
pub fn reclaim_expired(db: &Db, dry_run: bool, ctx: &EventCtx) -> Result<ReclaimReport> {
    let candidates = expired(db)?;
    if dry_run {
        return Ok(ReclaimReport {
            dry_run,
            reclaimed: candidates,
            skipped: 0,
        });
    }
    let mut reclaimed = Vec::new();
    let mut skipped = 0;
    for c in candidates {
        let mut conn = db.get()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE tasks SET status_v2='todo', status='pending', updated_at=?1
              WHERE id=?2 AND status_v2='in_progress' AND claimed_by=?3
                AND claim_expires_at=?4",
            params![crate::tasks::iso_now(), c.task_uuid, c.holder, c.expired_at],
        )?;
        if changed == 0 {
            skipped += 1;
            continue;
        }
        let key = format!("reclaim:{}:{}", c.task_uuid, c.expired_at);
        crate::tasks::record_event_tx(
            &tx,
            &ctx.with_uuid(key),
            &c.task_uuid,
            "task.reclaimed",
            &serde_json::json!({
                "task_uuid": c.task_uuid, "pt_id": c.pt_id, "status": "todo",
                "holder": c.holder, "expired_at": c.expired_at,
            }),
        )?;
        tx.commit()?;
        reclaimed.push(c);
    }
    Ok(ReclaimReport {
        dry_run,
        reclaimed,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{self, NewTask};

    fn fresh() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("c.db")).unwrap();
        (dir, db)
    }

    fn status(db: &Db, id: &str) -> String {
        db.with_conn(|c| {
            Ok(
                c.query_row("SELECT status_v2 FROM tasks WHERE id=?1", [id], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap()
    }

    /// Pretend the lease ran out `mins` minutes ago.
    fn expire(db: &Db, id: &str, mins: i64) {
        let past = utc_now()
            .checked_sub(jiff::Span::new().minutes(mins))
            .unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET claim_expires_at=?1 WHERE id=?2",
                params![crate::dates::format_iso(&past), id],
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn a_claim_has_an_owner_and_a_second_claimer_learns_who() {
        let (_d, db) = fresh();
        let t = tasks::create(&db, NewTask::minimal("roll osds"), &EventCtx::test()).unwrap();
        let c = claim(&db, &t.id, Some(30), &EventCtx::local("hal")).unwrap();
        assert_eq!(c.by, "hal");
        assert!(c.expires_at.is_some() && !c.expired);
        let err = claim(&db, &t.id, None, &EventCtx::local("grok")).unwrap_err();
        assert!(err.to_string().contains("already claimed by hal"), "{err}");
        assert_eq!(get(&db, &t.id).unwrap().unwrap().by, "hal");
        // Out-of-range leases are refused before anything changes.
        let u = tasks::create(&db, NewTask::minimal("u"), &EventCtx::test()).unwrap();
        assert!(claim(&db, &u.id, Some(0), &EventCtx::local("hal")).is_err());
        assert!(
            claim(
                &db,
                &u.id,
                Some(MAX_LEASE_MINUTES + 1),
                &EventCtx::local("hal")
            )
            .is_err()
        );
        assert_eq!(status(&db, &u.id), "todo");
    }

    #[test]
    fn only_the_holder_heartbeats_and_a_lost_claim_says_stop() {
        let (_d, db) = fresh();
        let t = tasks::create(&db, NewTask::minimal("long job"), &EventCtx::test()).unwrap();
        let granted = claim(&db, &t.id, Some(5), &EventCtx::local("hal")).unwrap();
        expire(&db, &t.id, 1);
        // Expired but not reclaimed yet: still this instance's to renew.
        let renewed = heartbeat(&db, &t.id, 30, &granted.token, &EventCtx::local("HAL")).unwrap();
        assert!(!get(&db, &t.id).unwrap().unwrap().expired);
        assert!(renewed.expires_at.is_some());
        let other =
            heartbeat(&db, &t.id, 30, &granted.token, &EventCtx::local("grok")).unwrap_err();
        assert!(
            other
                .to_string()
                .starts_with("claim lost: now claimed by hal"),
            "{other}"
        );
        release(
            &db,
            &t.id,
            false,
            None,
            Some(granted.token.as_str()),
            &EventCtx::local("hal"),
        )
        .unwrap();
        let lost = heartbeat(&db, &t.id, 30, &granted.token, &EventCtx::local("hal")).unwrap_err();
        assert!(lost.to_string().contains("stop work"), "{lost}");
    }

    #[test]
    fn release_is_the_holders_unless_forced() {
        let (_d, db) = fresh();
        let t = tasks::create(&db, NewTask::minimal("t"), &EventCtx::test()).unwrap();
        claim(&db, &t.id, None, &EventCtx::local("hal")).unwrap();
        let refused = release(&db, &t.id, false, None, None, &EventCtx::local("grok")).unwrap_err();
        assert!(refused.to_string().contains("claimed by hal"), "{refused}");
        assert_eq!(status(&db, &t.id), "in_progress");
        let r = release(
            &db,
            &t.id,
            true,
            Some("hal is down"),
            None,
            &EventCtx::local("shell"),
        )
        .unwrap();
        assert!(r.forced);
        assert_eq!(r.holder.as_deref(), Some("hal"));
        assert_eq!(status(&db, &t.id), "todo");
        assert_eq!(get(&db, &t.id).unwrap(), None);
        // Claimable again; releasing a todo task is an error.
        assert!(release(&db, &t.id, false, None, None, &EventCtx::local("hal")).is_err());
        claim(&db, &t.id, None, &EventCtx::local("grok")).unwrap();
        let payload: String = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT payload FROM pt_event_log WHERE event_type='task.released'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(
            (v["holder"].as_str(), v["forced"].as_bool()),
            (Some("hal"), Some(true))
        );
        assert_eq!(v["reason"], "hal is down");
    }

    #[test]
    fn leaving_in_progress_any_way_drops_the_claim() {
        let (_d, db) = fresh();
        let ctx = EventCtx::local("hal");
        for close in ["done", "dismiss", "snooze"] {
            let t = tasks::create(&db, NewTask::minimal(close), &EventCtx::test()).unwrap();
            claim(&db, &t.id, Some(30), &ctx).unwrap();
            match close {
                "done" => {
                    tasks::mark_done(&db, &t, &ctx).unwrap();
                }
                "dismiss" => tasks::dismiss(&db, &t.id, &ctx).unwrap(),
                _ => tasks::snooze(&db, &t.id, "2099-01-01", &ctx).unwrap(),
            }
            let cols: (Option<String>, Option<String>, Option<String>) = db
                .with_conn(|c| {
                    Ok(c.query_row(
                        "SELECT claimed_by, claim_expires_at, claim_token FROM tasks WHERE id=?1",
                        [&t.id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )?)
                })
                .unwrap();
            assert_eq!(cols, (None, None, None), "{close} left the claim behind");
        }
    }

    #[test]
    fn reclaim_returns_only_expired_leases_and_respects_a_late_heartbeat() {
        let (_d, db) = fresh();
        let hal = EventCtx::local("hal");
        let dead = tasks::create(&db, NewTask::minimal("dead agent"), &EventCtx::test()).unwrap();
        let alive = tasks::create(&db, NewTask::minimal("alive"), &EventCtx::test()).unwrap();
        let unleased = tasks::create(&db, NewTask::minimal("no lease"), &EventCtx::test()).unwrap();
        let dead_claim = claim(&db, &dead.id, Some(10), &hal).unwrap();
        claim(&db, &alive.id, Some(10), &hal).unwrap();
        claim(&db, &unleased.id, None, &hal).unwrap();
        expire(&db, &dead.id, 5);

        let dry = reclaim_expired(&db, true, &EventCtx::system("reclaim")).unwrap();
        assert_eq!(dry.reclaimed.len(), 1);
        assert_eq!(dry.reclaimed[0].task_uuid, dead.id);
        assert_eq!(
            status(&db, &dead.id),
            "in_progress",
            "a dry run writes nothing"
        );

        let real = reclaim_expired(&db, false, &EventCtx::system("reclaim")).unwrap();
        assert_eq!(real.reclaimed.len(), 1);
        assert_eq!(status(&db, &dead.id), "todo");
        assert_eq!(status(&db, &alive.id), "in_progress");
        assert_eq!(
            status(&db, &unleased.id),
            "in_progress",
            "no lease never expires"
        );
        let lost = heartbeat(&db, &dead.id, 10, &dead_claim.token, &hal).unwrap_err();
        assert!(lost.to_string().contains("claim lost"), "{lost}");
        // Reclaimed work is claimable by the next worker.
        claim(&db, &dead.id, Some(10), &EventCtx::local("grok")).unwrap();
        assert!(
            reclaim_expired(&db, false, &EventCtx::system("reclaim"))
                .unwrap()
                .reclaimed
                .is_empty()
        );
    }

    #[test]
    fn a_new_claim_is_a_new_instance_and_the_old_token_is_dead() {
        let (_d, db) = fresh();
        let t = tasks::create(&db, NewTask::minimal("index"), &EventCtx::test()).unwrap();
        let a = claim(&db, &t.id, Some(10), &EventCtx::local("hal")).unwrap();
        expire(&db, &t.id, 5);
        reclaim_expired(&db, false, &EventCtx::system("reclaim")).unwrap();
        let b = claim(&db, &t.id, Some(10), &EventCtx::local("hal")).unwrap();
        assert_ne!(a.token, b.token);
        assert!(!b.token.is_empty());
        let stale = heartbeat(&db, &t.id, 30, &a.token, &EventCtx::local("hal")).unwrap_err();
        assert!(stale.to_string().contains("claim lost"), "{stale}");
        heartbeat(&db, &t.id, 30, &b.token, &EventCtx::local("hal")).unwrap();
        assert!(
            release(
                &db,
                &t.id,
                false,
                None,
                Some(a.token.as_str()),
                &EventCtx::local("hal")
            )
            .is_err()
        );
        assert_eq!(status(&db, &t.id), "in_progress");
        release(
            &db,
            &t.id,
            false,
            Some("handing back"),
            Some(b.token.as_str()),
            &EventCtx::local("hal"),
        )
        .unwrap();
        assert_eq!(status(&db, &t.id), "todo");
    }

    #[test]
    fn an_expired_lease_is_free_to_claim() {
        let (_d, db) = fresh();
        let t = tasks::create(&db, NewTask::minimal("nightly"), &EventCtx::test()).unwrap();
        claim(&db, &t.id, Some(10), &EventCtx::local("hal")).unwrap();
        expire(&db, &t.id, 5);
        let taken = claim(&db, &t.id, Some(30), &EventCtx::local("grok")).unwrap();
        assert_eq!(taken.by, "grok");
        let c = get(&db, &t.id).unwrap().unwrap();
        assert_eq!(c.by, "grok");
        assert!(!c.expired);
    }

    #[test]
    fn unowned_in_progress_needs_force_to_release() {
        let (_d, db) = fresh();
        let t = tasks::create(&db, NewTask::minimal("legacy"), &EventCtx::test()).unwrap();
        db.with_conn(|c| {
            c.execute(
                "UPDATE tasks SET status_v2='in_progress', status='pending',
                                  claimed_by=NULL, claimed_at=NULL,
                                  claim_expires_at=NULL, claim_token=NULL
                  WHERE id=?1",
                [&t.id],
            )?;
            Ok(())
        })
        .unwrap();
        let refused = release(&db, &t.id, false, None, None, &EventCtx::local("grok")).unwrap_err();
        assert!(refused.to_string().contains("--force"), "{refused}");
        assert_eq!(status(&db, &t.id), "in_progress");
        let r = release(
            &db,
            &t.id,
            true,
            Some("nobody on it"),
            None,
            &EventCtx::local("shell"),
        )
        .unwrap();
        assert!(r.forced);
        assert_eq!(status(&db, &t.id), "todo");
    }

    #[test]
    fn lease_parsing() {
        assert_eq!(parse_lease("30m").unwrap(), 30);
        assert_eq!(parse_lease("2h").unwrap(), 120);
        assert_eq!(parse_lease(" 90 ").unwrap(), 90);
        assert_eq!(parse_lease("1d").unwrap(), MAX_LEASE_MINUTES);
        for bad in ["", "0m", "2d", "abc", "-5", "99999999999999999h"] {
            assert!(parse_lease(bad).is_err(), "{bad:?}");
        }
    }
}
