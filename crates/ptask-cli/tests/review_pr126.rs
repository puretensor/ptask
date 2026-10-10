//! Review contracts for PR #126 (close and continue: unblocked list,
//! claim-next). Each test pins one review finding: it fails on the PR head
//! for the reason the finding describes and passes once the fix lands.
//!
//! The interface these tests fix (the implementation matches these names):
//! - `pt done <task>... --claim-next [--lease D]` and MCP
//!   `task_done(claim_next, lease_minutes)`: claim-next never takes a task
//!   the same call closed or advanced, and claims with an owner and an
//!   optional lease exactly as `pt claim` / `task_claim` do (once #123's
//!   owned claims are underneath).
//! - `claimed_next` is the task as it is after the claim (in progress),
//!   carrying `claimed_by` and `claim_expires_at` (and `claim_token` once
//!   #123's claim tokens exist), or `null` when nothing is claimable, or
//!   `{"error": ...}` when the claim failed after the closes succeeded.
//! - With `--json --claim-next` the output is always
//!   `{"results": [...], "claimed_next": ...}`: on success, on a failed
//!   close (nothing claimed) and on a keyed replay (the replay reports the
//!   task the first run claimed and claims nothing).

mod common;
use common::Pt;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Output, Stdio};

fn db(pt: &Pt) -> ptask_core::Db {
    ptask_core::Db::open(pt.dir.path().join("tasks.db")).unwrap()
}

/// Run statements against this test's own scratch database (fault
/// injection only).
fn sql(pt: &Pt, statements: &str) {
    db(pt)
        .with_conn(|c| {
            c.execute_batch(statements)?;
            Ok(())
        })
        .unwrap();
}

/// A task's status read straight from the store (works while the goal
/// tables are deliberately unreadable, unlike `pt show`).
fn status_of(pt: &Pt, pt_id: &str) -> String {
    db(pt)
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT status_v2 FROM tasks WHERE pt_id = ?1",
                [pt_id],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

/// How many claims were journaled in this database.
fn claim_events(pt: &Pt) -> i64 {
    db(pt)
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM pt_event_log WHERE event_type = 'task.claimed'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `pt --json <args>` as `actor`, parsed.
fn json_as(pt: &Pt, actor: &str, args: &[&str]) -> Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    let out = pt.ok_as(actor, &full);
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("pt {full:?}: {e}\n{out}"))
}

/// The JSON a command printed on stdout, whatever its exit status.
fn printed(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "no JSON on stdout ({e}); stdout: {} stderr: {}",
            text(&out.stdout),
            text(&out.stderr)
        )
    })
}

/// #123's owned claims (`pt claim`) are underneath this build.
fn claims_landed(pt: &Pt) -> bool {
    pt.run(&["claim", "--help"]).status.success()
}

/// #123's claim tokens (`pt heartbeat --claim <token>`) are underneath.
fn claim_tokens_landed(pt: &Pt) -> bool {
    text(&pt.run(&["heartbeat", "--help"]).stdout).contains("--claim")
}

/// One `pt mcp` stdio session on this test's database, as `actor`.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    id: u64,
}

impl Mcp {
    fn start(pt: &Pt, actor: &str) -> Mcp {
        let mut child = pt
            .command(actor, &["mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp {
            child,
            stdin,
            stdout,
            id: 0,
        };
        let init = m.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "review-tests", "version": "0"},
            }),
        );
        assert!(init.get("result").is_some(), "initialize: {init}");
        m.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        m
    }

    fn send(&mut self, msg: &Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "pt mcp exited before answering {method}"
            );
            if let Ok(msg) = serde_json::from_str::<Value>(&line)
                && msg["id"] == json!(id)
            {
                return msg;
            }
        }
    }

    /// `tools/call`: the tool's JSON reply, or the error it returned.
    fn call(&mut self, tool: &str, args: Value) -> Result<Value, String> {
        let r = self.request("tools/call", json!({"name": tool, "arguments": args}));
        if let Some(e) = r.get("error") {
            return Err(e["message"].as_str().unwrap_or_default().to_string());
        }
        let body = r["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        if r["result"]["isError"] == json!(true) {
            return Err(body.to_string());
        }
        Ok(serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.to_string())))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// --- 1. claim-next never takes back what the same call closed --------------

#[test]
fn close_and_continue_does_not_reclaim_the_recurring_task_it_just_advanced() {
    let pt = Pt::new();
    pt.ok(&["add", "Rotate the logs every day", "-p", "urgent"]); // PT-1, recurring
    pt.ok(&["add", "--raw", "Tidy the shared notes"]); // PT-2
    let v = json_as(&pt, "agent", &["done", "PT-1", "--claim-next"]);
    assert_eq!(v["results"][0]["outcome"], "advanced", "{v}");
    assert_ne!(
        v["claimed_next"]["pt_id"], "PT-1",
        "claim-next claimed the next occurrence of the task this close just advanced: {v}"
    );
    assert_eq!(v["claimed_next"]["pt_id"], "PT-2", "{v}");
    assert_eq!(status_of(&pt, "PT-1"), "todo");
}

/// An agent sweeps with close-and-continue until nothing is left: close
/// what it holds, take what comes back. The sweep has to end.
#[test]
fn an_agent_looping_close_and_continue_over_mcp_terminates() {
    let pt = Pt::new();
    pt.ok(&["add", "Rotate the logs every day", "-p", "urgent"]); // PT-1, recurring
    pt.ok(&["add", "--raw", "Tidy the shared notes"]); // PT-2
    let mut m = Mcp::start(&pt, "agent");
    let mut current = "PT-1".to_string();
    for call in 1.. {
        assert!(
            call <= 5,
            "close-and-continue still claiming after {} calls",
            call - 1
        );
        let r = m
            .call("task_done", json!({"id": current, "claim_next": true}))
            .unwrap();
        let next = &r["claimed_next"];
        if next.is_null() {
            break;
        }
        let id = next["pt_id"].as_str().unwrap().to_string();
        assert_ne!(
            id, current,
            "task_done({current}, claim_next) claimed the task it just closed: {r}"
        );
        current = id;
    }
}

// --- 2. a keyed retry replays, it does not fail or claim again --------------

#[test]
fn a_keyed_retry_of_a_multi_task_close_and_continue_replays_cleanly() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Close the first ticket"]); // PT-1
    pt.ok(&["add", "--raw", "Close the second ticket"]); // PT-2
    pt.ok(&["add", "--raw", "-p", "urgent", "Pick this up next"]); // PT-3
    pt.ok(&["add", "--raw", "-p", "low", "Leave this for later"]); // PT-4
    let args = [
        "--idempotency-key",
        "sweep-1",
        "--json",
        "done",
        "PT-1",
        "PT-2",
        "--claim-next",
    ];
    let first = pt.run_as("agent", &args);
    assert!(first.status.success(), "{}", text(&first.stderr));
    assert_eq!(printed(&first)["claimed_next"]["pt_id"], "PT-3");

    // The reply was lost; the agent retries the same keyed command.
    let retry = pt.run_as("agent", &args);
    assert!(
        retry.status.success() && !text(&retry.stderr).contains("UNIQUE"),
        "a keyed retry of an applied close-and-continue failed: {}",
        text(&retry.stderr)
    );
    let v = printed(&retry);
    let results = v["results"]
        .as_array()
        .unwrap_or_else(|| panic!("no results on the retry: {v}"));
    assert_eq!(results.len(), 2, "{v}");
    for r in results {
        assert_eq!(r["outcome"], "replayed", "{v}");
    }
    assert_eq!(
        v["claimed_next"]["pt_id"], "PT-3",
        "the retry must report the task the first run claimed: {v}"
    );
    assert_eq!(
        status_of(&pt, "PT-4"),
        "todo",
        "the retry claimed a second task"
    );
    assert_eq!(claim_events(&pt), 1);
}

#[test]
fn a_keyed_retry_of_a_single_close_and_continue_reports_the_task_it_claimed() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Close the ticket"]); // PT-1
    pt.ok(&["add", "--raw", "Pick this up next"]); // PT-2
    let args = [
        "--idempotency-key",
        "sweep-2",
        "done",
        "PT-1",
        "--claim-next",
    ];
    let first = json_as(&pt, "agent", &args);
    assert_eq!(first["claimed_next"]["pt_id"], "PT-2", "{first}");
    let retry = json_as(&pt, "agent", &args);
    assert_eq!(
        retry["claimed_next"]["pt_id"], "PT-2",
        "a caller that lost the first reply never learns it now holds PT-2: {retry}"
    );
    assert_eq!(claim_events(&pt), 1, "the retry claimed again");
}

// --- 3. claim-next claims the way task_claim does (#123 underneath) ---------

#[test]
fn with_pr123_claim_next_takes_an_owner_and_a_lease_like_pt_claim() {
    let pt = Pt::new();
    if !claims_landed(&pt) {
        eprintln!(
            "SKIPPED review #126-3 (CLI): needs #123's owned claims underneath; \
             `pt claim` does not exist on this build"
        );
        return;
    }
    pt.ok(&["add", "--raw", "Close the ticket"]); // PT-1
    pt.ok(&["add", "--raw", "-p", "urgent", "Pick this up next"]); // PT-2
    pt.ok(&["add", "--raw", "-p", "low", "Close another ticket"]); // PT-3
    pt.ok(&["add", "--raw", "-p", "high", "Then this one"]); // PT-4

    let v = json_as(
        &pt,
        "agent",
        &["done", "PT-1", "--claim-next", "--lease", "30m"],
    );
    let next = &v["claimed_next"];
    assert_eq!(next["pt_id"], "PT-2", "{v}");
    assert_eq!(
        next["claimed_by"], "agent",
        "claimed_next must name its holder like task_claim: {v}"
    );
    assert!(
        next["claim_expires_at"].is_string(),
        "claim-next took no lease, so a dead sweeper's task never comes back: {v}"
    );
    let c = pt.json(&["show", "PT-2"])["claim"].clone();
    assert_eq!(c["by"], "agent", "{c}");
    assert!(c["expires_at"].is_string(), "{c}");
    if claim_tokens_landed(&pt) {
        let t = next["claim_token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| {
                panic!("claimed_next carries no claim_token to heartbeat with: {v}")
            });
        pt.ok_as("agent", &["heartbeat", "PT-2", "--claim", t]);
    }

    // Without --lease: owned, no lease (as `pt claim` without one).
    let v = json_as(&pt, "agent", &["done", "PT-3", "--claim-next"]);
    assert_eq!(v["claimed_next"]["pt_id"], "PT-4", "{v}");
    assert_eq!(v["claimed_next"]["claimed_by"], "agent", "{v}");
    assert!(v["claimed_next"]["claim_expires_at"].is_null(), "{v}");
}

#[test]
fn with_pr123_mcp_claim_next_takes_an_owner_and_a_lease_like_task_claim() {
    let pt = Pt::new();
    if !claims_landed(&pt) {
        eprintln!(
            "SKIPPED review #126-3 (MCP): needs #123's owned claims underneath; \
             `pt claim` does not exist on this build"
        );
        return;
    }
    pt.ok(&["add", "--raw", "Close the ticket"]); // PT-1
    pt.ok(&["add", "--raw", "Pick this up next"]); // PT-2
    let mut m = Mcp::start(&pt, "agent");
    let r = m
        .call(
            "task_done",
            json!({"id": "PT-1", "claim_next": true, "lease_minutes": 30}),
        )
        .unwrap();
    let next = &r["claimed_next"];
    assert_eq!(next["pt_id"], "PT-2", "{r}");
    assert_eq!(
        next["claimed_by"], "agent",
        "claimed_next must name its holder like task_claim: {r}"
    );
    assert!(
        next["claim_expires_at"].is_string(),
        "claim-next took no lease, so a dead sweeper's task never comes back: {r}"
    );
    let c = pt.json(&["show", "PT-2"])["claim"].clone();
    assert!(c["expires_at"].is_string(), "{c}");
    if claim_tokens_landed(&pt) {
        let t = next["claim_token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| panic!("claimed_next carries no claim_token: {r}"));
        m.call("task_heartbeat", json!({"id": "PT-2", "claim_token": t}))
            .unwrap();
    }
}

// --- 4. a failure after the close never fails the close ---------------------

#[test]
fn a_failed_claim_after_a_successful_close_does_not_fail_the_close() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Close the ticket"]); // PT-1
    pt.ok(&["add", "--raw", "Pick this up next"]); // PT-2
    // From here every claim fails; closes are untouched.
    sql(
        &pt,
        "CREATE TRIGGER review_claims_fail BEFORE UPDATE OF status_v2 ON tasks
           WHEN NEW.status_v2 = 'in_progress'
         BEGIN SELECT RAISE(ABORT, 'injected claim failure'); END;",
    );
    let out = pt.run_as("agent", &["--json", "done", "PT-1", "--claim-next"]);
    assert_eq!(status_of(&pt, "PT-1"), "done", "setup: the close commits");
    assert!(
        out.status.success(),
        "the close committed, yet the command failed because the claim after it did: {}",
        text(&out.stderr)
    );
    let v = printed(&out);
    assert_eq!(v["results"][0]["outcome"], "completed", "{v}");
    let err = v["claimed_next"]["error"]
        .as_str()
        .unwrap_or_else(|| panic!("claimed_next must carry the claim's error: {v}"));
    assert!(err.contains("injected claim failure"), "{v}");
    assert_eq!(status_of(&pt, "PT-2"), "todo");
}

#[test]
fn over_mcp_a_failed_goal_lookup_after_close_and_claim_returns_the_task_without_goals() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Close the ticket"]); // PT-1
    pt.ok(&["add", "--raw", "Pick this up next"]); // PT-2
    pt.ok(&["goal", "add", "Keep the service healthy"]); // G-1
    pt.ok(&["goal", "link", "PT-2", "G-1"]);
    // The goal chain becomes unreadable; the close and the claim never
    // read it, only the goal decoration of the reply does.
    sql(&pt, "ALTER TABLE goals RENAME TO goals_unreadable;");
    let mut m = Mcp::start(&pt, "agent");
    let r = m.call("task_done", json!({"id": "PT-1", "claim_next": true}));
    let r = r.unwrap_or_else(|e| {
        panic!(
            "task_done reported failure ({e}) although the close and the claim \
             committed: PT-1 is {}, PT-2 is {}",
            status_of(&pt, "PT-1"),
            status_of(&pt, "PT-2")
        )
    });
    assert_eq!(r["status"], "done", "{r}");
    assert_eq!(r["claimed_next"]["pt_id"], "PT-2", "{r}");
}

// --- 5. claimed_next is the task as claimed ---------------------------------

#[test]
fn claimed_next_shows_the_task_in_progress() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Close the ticket"]); // PT-1
    pt.ok(&["add", "--raw", "Pick this up next"]); // PT-2
    let v = json_as(&pt, "agent", &["done", "PT-1", "--claim-next"]);
    assert_eq!(status_of(&pt, "PT-2"), "in_progress");
    assert_eq!(
        v["claimed_next"]["status"], "in_progress",
        "claimed_next is the snapshot from before the claim: {v}"
    );
}

#[test]
fn mcp_claimed_next_shows_the_task_in_progress() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Close the ticket"]); // PT-1
    pt.ok(&["add", "--raw", "Pick this up next"]); // PT-2
    let mut m = Mcp::start(&pt, "agent");
    let r = m
        .call("task_done", json!({"id": "PT-1", "claim_next": true}))
        .unwrap();
    assert_eq!(status_of(&pt, "PT-2"), "in_progress");
    assert_eq!(
        r["claimed_next"]["status"], "in_progress",
        "claimed_next is the snapshot from before the claim: {r}"
    );
}

// --- 6. one JSON shape for --json --claim-next -------------------------------

#[test]
fn close_and_continue_json_is_always_the_documented_object() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Close the ticket"]); // PT-1
    pt.ok(&["add", "--raw", "Pick this up next"]); // PT-2

    // One of the closes fails: still the object, and nothing is claimed.
    let out = pt.run_as(
        "agent",
        &["--json", "done", "PT-1", "PT-999", "--claim-next"],
    );
    assert!(
        !out.status.success(),
        "a failed close still fails the command"
    );
    let v = printed(&out);
    assert!(
        v.is_object() && v["results"].is_array(),
        "expected {{\"results\": [...], \"claimed_next\": ...}}, got: {v}"
    );
    assert_eq!(v["results"].as_array().unwrap().len(), 2, "{v}");
    assert!(
        v.get("claimed_next").is_some_and(Value::is_null),
        "nothing is claimed after a failed close: {v}"
    );
    assert_eq!(status_of(&pt, "PT-2"), "todo");

    // A keyed run and its replay: the same object both times.
    pt.ok(&["add", "--raw", "Close another ticket"]); // PT-3
    let args = [
        "--idempotency-key",
        "shape-1",
        "done",
        "PT-3",
        "--claim-next",
    ];
    for (run, v) in [
        ("first", json_as(&pt, "agent", &args)),
        ("replay", json_as(&pt, "agent", &args)),
    ] {
        assert!(
            v["results"].is_array() && v.get("claimed_next").is_some(),
            "{run} run: expected {{\"results\": [...], \"claimed_next\": ...}}, got: {v}"
        );
    }
}

// --- follow-up (HAL, at the merge onto #122's fingerprints) ------------------

/// #122 renders a keyed `done` without its note field so 3.42.2 keys still
/// replay. `--claim-next` changes what the command does (it claims a task),
/// so under one key `done PT-1` and `done PT-1 --claim-next` are different
/// commands: the second must be refused, not replayed as the first.
#[test]
fn a_key_used_by_a_plain_done_is_not_replayed_by_done_claim_next() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Close the ticket"]); // PT-1
    pt.ok(&["add", "--raw", "Pick this up next"]); // PT-2
    pt.ok(&["--idempotency-key", "sweep-9", "done", "PT-1"]);
    let out = pt.run(&[
        "--idempotency-key",
        "sweep-9",
        "done",
        "PT-1",
        "--claim-next",
    ]);
    assert!(
        !out.status.success(),
        "a keyed done --claim-next replayed a plain keyed done under the same key: {}",
        text(&out.stdout)
    );
    assert_eq!(status_of(&pt, "PT-2"), "todo", "nothing may be claimed");
}

/// 3.43.0 to 3.46.0 (main before this PR) journaled a keyed `done -m` as the
/// derived Debug of DoneArgs { queries, note }. Adding `--claim-next` must
/// not change that fingerprint when the flag is off, or a retry of the same
/// keyed close across the upgrade fails as a different command.
#[test]
fn a_keyed_noted_done_fingerprints_as_it_did_before_claim_next() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Rotate the signing key"]); // PT-1
    pt.ok(&[
        "--idempotency-key",
        "k-noted",
        "done",
        "PT-1",
        "-m",
        "verified",
    ]);
    let db = ptask_core::Db::open(pt.dir.path().join("tasks.db")).unwrap();
    let journaled = ptask_core::event_log::get_by_uuid(&db, "k-noted")
        .unwrap()
        .expect("the keyed close is journaled under its key")
        .command;
    assert_eq!(
        journaled,
        Some(ptask_core::event_log::CommandFingerprint::new(
            "Done",
            r#"Done(DoneArgs { queries: ["PT-1"], note: Some("verified") })"#
        )),
        "a keyed `done -m` without --claim-next must fingerprint exactly as 3.46.0 did"
    );
}
