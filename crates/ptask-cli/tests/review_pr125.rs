//! Contract tests from the review of puretensor/ptask#125 (flux by actor).
//! Each test fails on the PR head for the reason its name gives and passes
//! once that finding is fixed. Black-box against the built `pt`, on
//! throwaway databases only; journal rows that no current writer produces
//! (but the live journal holds) are written directly.

mod common;
use common::Pt;
use ptask_core::Db;
use ptask_core::event_log::{self, EventCtx};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};

fn db(pt: &Pt) -> Db {
    Db::open(pt.dir.path().join("tasks.db")).unwrap()
}

/// `pt --json <args>` as `actor`, parsed.
fn json_as(pt: &Pt, actor: &str, args: &[&str]) -> Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    let out = pt.ok_as(actor, &full);
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("pt {full:?}: {e}\n{out}"))
}

fn uuid(pt: &Pt, id: &str) -> String {
    pt.json(&["show", id])["id"].as_str().unwrap().to_string()
}

fn open_count(pt: &Pt) -> i64 {
    db(pt)
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM tasks WHERE status_v2 NOT IN ('done', 'dismissed')",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

fn flux(pt: &Pt, since: &str) -> Value {
    pt.json(&["flux", "--since", since])
}

/// An actor's row; an actor with nothing to count has no row, read as zeros.
fn row(report: &Value, actor: &str) -> Value {
    report["actors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["actor"] == actor)
        .cloned()
        .unwrap_or_else(
            || json!({"created": 0, "done": 0, "dismissed": 0, "reopened": 0, "net": 0}),
        )
}

/// Journal one event as `actor`, the way an external writer would.
fn journal(db: &Db, actor: &str, task: &str, event_type: &str, payload: Value) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let key = format!("review-125:{}", N.fetch_add(1, Ordering::Relaxed));
    event_log::record(
        db,
        &key,
        Some(task),
        event_type,
        &payload,
        &EventCtx::local(actor),
    )
    .unwrap();
}

// Finding 1: deletes are invisible to flux, so a task created and then
// deleted counts as backlog growth and deleting open work counts as nothing.

#[test]
fn deleting_open_tasks_counts_as_closing_them() {
    let pt = Pt::new();
    for t in [
        "First sweep finding",
        "Second sweep finding",
        "Third sweep finding",
    ] {
        pt.ok_as("agent-a", &["add", "--raw", t]); // PT-1..PT-3
    }
    // Undo of a create deletes PT-3; `pt rm` deletes open PT-1.
    let undone = pt.ok_as("agent-a", &["--no-color", "undo", "--yes"]);
    assert!(
        undone.contains("PT-3"),
        "precondition: the undo deleted PT-3: {undone}"
    );
    pt.ok_as("agent-a", &["rm", "PT-1", "--yes"]);
    // Deleting a task that is already done changes nothing that is open.
    let late = json_as(&pt, "agent-a", &["add", "--raw", "Fourth sweep finding"]);
    let late = late["pt_id"].as_str().unwrap().to_string();
    pt.ok_as("agent-a", &["done", &late]);
    pt.ok_as("agent-a", &["rm", &late, "--yes"]);
    assert_eq!(open_count(&pt), 1, "precondition: only PT-2 is open");

    let r = flux(&pt, "1h");
    let a = row(&r, "agent-a");
    assert_eq!(
        a["net"], 1,
        "agent-a opened 4, deleted 2 open tasks and closed 1, so the open count \
         went from 0 to 1, yet flux reports net {}: {r:#}",
        a["net"]
    );
    assert_eq!(
        r["total"]["net"], 1,
        "total net is the open-count change: {r:#}"
    );
}

#[test]
fn sync_deletes_of_open_tasks_count_as_closing_them() {
    let pt = Pt::new();
    let srv = pt.serve();
    pt.ok(&["remote", "add", "Remote sweep finding", "--url", &srv.url]);
    pt.ok(&["remote", "rm", "PT-1", "--yes", "--url", &srv.url]);
    assert_eq!(open_count(&pt), 0, "precondition: nothing is open");
    let r = flux(&pt, "1h");
    assert_eq!(
        r["total"]["net"], 0,
        "a task created and deleted over /sync reads as backlog growth: {r:#}"
    );
}

// Finding 2: flux counts payload keys, not transitions: any status "pending"
// is a reopen and every completion a close, even when the task was already
// in that state.

#[test]
fn only_real_open_close_transitions_count() {
    let pt = Pt::new();
    pt.ok_as("agent-a", &["add", "--raw", "Blocked sweep finding"]); // PT-1
    pt.ok_as("agent-a", &["add", "--raw", "Completed sweep finding"]); // PT-2
    pt.ok_as("agent-a", &["add", "--raw", "Older sweep finding"]); // PT-3
    pt.ok_as("agent-a", &["done", "PT-2"]);
    pt.ok_as("agent-a", &["done", "PT-3"]);
    let (one, two, three) = (uuid(&pt, "PT-1"), uuid(&pt, "PT-2"), uuid(&pt, "PT-3"));
    let db = db(&pt);
    // PT-3 was created and closed before the window.
    db.with_conn(|c| {
        c.execute(
            "UPDATE pt_event_log SET ts = strftime('%Y-%m-%dT%H:%M:%f', 'now', '-3 hours') || '+00:00'
              WHERE task_uuid = ?1",
            [&three],
        )?;
        Ok(())
    })
    .unwrap();
    // agent-b blocks and un-blocks PT-1, which never leaves the open set (as
    // an accountability clean-up journaled it on the live system).
    journal(
        &db,
        "agent-b",
        &one,
        "task.updated",
        json!({"task_uuid": one, "status": "blocked"}),
    );
    journal(
        &db,
        "agent-b",
        &one,
        "task.updated",
        json!({"task_uuid": one, "status": "pending", "reason": "lift an automatic block"}),
    );
    // agent-c completes PT-2 again although it is already done (as the /sync
    // path journaled it before completions of done tasks were refused).
    journal(
        &db,
        "agent-c",
        &two,
        "task.completed",
        json!({"task_uuid": two, "pt_id": "PT-2"}),
    );
    // Real reopens count, including one of a task closed before the window.
    pt.ok_as("agent-d", &["reopen", "PT-2"]);
    pt.ok_as("agent-d", &["reopen", "PT-3"]);

    let r = flux(&pt, "1h");
    assert_eq!(
        row(&r, "agent-b")["reopened"],
        0,
        "an un-block of PT-1, which was open throughout, counted as a reopen: {r:#}"
    );
    assert_eq!(
        row(&r, "agent-c")["done"],
        0,
        "a second completion of PT-2, already done, counted as a close: {r:#}"
    );
    assert_eq!(
        row(&r, "agent-d")["reopened"],
        2,
        "both real reopens count, including PT-3 closed before the window: {r:#}"
    );
    // In the window: PT-1 and PT-2 opened, PT-2 closed, PT-2 and PT-3
    // reopened, so the open count went from 0 to 3.
    assert_eq!(open_count(&pt), 3, "precondition");
    assert_eq!(r["total"]["net"], 3, "{r:#}");
}

// Finding 3: the digest counts from UTC midnight N days ago, flux_by_actor
// from exactly N days ago, so the two halves of one digest disagree.

#[test]
fn digest_flux_by_actor_covers_the_digests_own_window() {
    let pt = Pt::new();
    pt.ok_as("agent-a", &["add", "--raw", "Late sweep finding"]); // PT-1
    let task = uuid(&pt, "PT-1");
    // Created between the digest's own start for --days 1 (UTC midnight
    // yesterday) and the rolling start (now - 24h).
    db(&pt)
        .with_conn(|c| {
            let ts: String = c.query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%f',
                        (julianday(date('now', '-1 day')) + julianday('now', '-1 day')) / 2)
                        || '+00:00'",
                [],
                |r| r.get(0),
            )?;
            c.execute(
                "UPDATE tasks SET created_at = ?1 WHERE id = ?2",
                [&ts, &task],
            )?;
            c.execute(
                "UPDATE pt_event_log SET ts = ?1 WHERE task_uuid = ?2",
                [&ts, &task],
            )?;
            Ok(())
        })
        .unwrap();

    let d = pt.json(&["digest", "--days", "1"]);
    let by_actor: i64 = d["flux_by_actor"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["created"].as_i64().unwrap())
        .sum();
    assert_eq!(
        by_actor,
        d["created_count"].as_i64().unwrap(),
        "the digest counts {} task(s) created in its window but flux_by_actor counts {by_actor}: \
         the two windows start at different times: {d:#}",
        d["created_count"]
    );
}
