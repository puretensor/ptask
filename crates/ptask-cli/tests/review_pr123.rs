//! Review contracts for PR #123 (claim ownership, heartbeat leases, release
//! and reclaim). Each test pins one review finding: it fails on the PR head
//! for the reason the finding describes and passes once the fix lands.
//!
//! The claim-token interface these tests fix (the implementation matches
//! these names):
//! - `pt --json claim <task> [--lease D]` returns a top-level `claim_token`:
//!   opaque, non-empty, new for every claim. A reclaim followed by a claim
//!   from the same actor is a new claim instance with a new token.
//! - `pt heartbeat <task> --claim <token> [--lease D]` renews only the claim
//!   that token names. A stale token fails with `claim lost`; a heartbeat
//!   without a token is refused.
//! - `pt release <task> --claim <token> [-m REASON]` hands back only the
//!   claim that token names. Without a token a release needs `--force`
//!   (the operator's override), and so does releasing an unowned task.
//! - MCP: `task_claim` returns `claim_token`; `task_heartbeat` and
//!   `task_release` take a `claim_token` argument (no force over MCP).

mod common;
use common::Pt;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

fn db(pt: &Pt) -> ptask_core::Db {
    ptask_core::Db::open(pt.dir.path().join("tasks.db")).unwrap()
}

/// Run statements against this test's own scratch database (fault
/// injection, and states no command produces any more).
fn sql(pt: &Pt, statements: &str) {
    db(pt)
        .with_conn(|c| {
            c.execute_batch(statements)?;
            Ok(())
        })
        .unwrap();
}

/// Make PT-N's lease end five minutes ago.
fn expire_lease(pt: &Pt, pt_id: &str) {
    db(pt)
        .with_conn(|c| {
            c.execute(
                "UPDATE tasks SET claim_expires_at =
                     strftime('%Y-%m-%dT%H:%M:%S', 'now', '-5 minutes') || '+00:00'
                  WHERE pt_id = ?1",
                [pt_id],
            )?;
            Ok(())
        })
        .unwrap();
}

/// An in-progress task nobody holds: what V021 leaves for work started
/// before 3.44, and what a pre-3.44 process still writes while a deploy
/// rolls out.
fn make_unowned_in_progress(pt: &Pt, pt_id: &str) {
    db(pt)
        .with_conn(|c| {
            c.execute(
                "UPDATE tasks SET status_v2 = 'in_progress', status = 'pending',
                                  claimed_by = NULL, claimed_at = NULL,
                                  claim_expires_at = NULL
                  WHERE pt_id = ?1",
                [pt_id],
            )?;
            Ok(())
        })
        .unwrap();
}

fn show(pt: &Pt, id: &str) -> Value {
    pt.json(&["show", id])
}

/// `pt --json <args>` as `actor`, parsed.
fn json_as(pt: &Pt, actor: &str, args: &[&str]) -> Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    let out = pt.ok_as(actor, &full);
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("pt {full:?}: {e}\n{out}"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn token(v: &Value, what: &str) -> String {
    v["claim_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            panic!(
                "{what} returned no claim_token, so two claims by the same actor \
                 cannot be told apart: {v}"
            )
        })
}

/// `GET path` against a running `pt serve`: (status code, raw response).
fn get(url: &str, path: &str) -> (u16, String) {
    let host = url.trim_start_matches("http://");
    let mut s = std::net::TcpStream::connect(host).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, raw)
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

    fn tool_names(&mut self) -> Vec<String> {
        let mut names = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let r = self.request("tools/list", params);
            let tools = r["result"]["tools"]
                .as_array()
                .unwrap_or_else(|| panic!("tools/list: {r}"));
            names.extend(
                tools
                    .iter()
                    .map(|t| t["name"].as_str().unwrap().to_string()),
            );
            match r["result"]["nextCursor"].as_str() {
                Some(c) => cursor = Some(c.to_string()),
                None => return names,
            }
        }
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// --- 1. ownership follows the claim instance, not the actor name ----------

/// Parallel agent sessions share one actor name (every session of an agent
/// runs under the same PTASK_ACTOR). Session A's lease runs out, the task is
/// reclaimed, and session B (same actor) claims it. A, still alive, must be
/// told to stop, not keep renewing or releasing B's claim.
#[test]
fn a_stale_session_cannot_renew_or_release_a_claim_retaken_under_the_same_actor() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Rebuild the search index"]);
    pt.ok_as("agent", &["claim", "PT-1", "--lease", "10m"]); // session A
    expire_lease(&pt, "PT-1");
    pt.ok_as("operator", &["reclaim", "--apply"]);
    pt.ok_as("agent", &["claim", "PT-1", "--lease", "10m"]); // session B

    // Session A heartbeats and releases the way it always could: by task
    // and actor name, presenting nothing that identifies its own claim.
    let renew = pt.run_as("agent", &["heartbeat", "PT-1"]);
    assert!(
        !renew.status.success(),
        "session A renewed session B's claim on the actor name alone: {}",
        text(&renew.stdout)
    );
    let release = pt.run_as("agent", &["release", "PT-1"]);
    assert!(
        !release.status.success(),
        "session A released session B's claim without --force: {}",
        text(&release.stdout)
    );
    let s = show(&pt, "PT-1");
    assert_eq!(s["status"], "in_progress", "{s}");
    assert_eq!(s["claim"]["by"], "agent", "{s}");
}

#[test]
fn heartbeat_and_release_present_the_token_of_the_current_claim() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Archive the old reports"]);
    let a = json_as(&pt, "agent", &["claim", "PT-1", "--lease", "10m"]);
    let token_a = token(&a, "pt claim");
    expire_lease(&pt, "PT-1");
    pt.ok_as("operator", &["reclaim", "--apply"]);
    let b = json_as(&pt, "agent", &["claim", "PT-1", "--lease", "10m"]);
    let token_b = token(&b, "pt claim");
    assert_ne!(token_a, token_b, "a new claim is a new instance");

    let stale = pt.run_as("agent", &["heartbeat", "PT-1", "--claim", &token_a]);
    assert!(
        !stale.status.success() && text(&stale.stderr).contains("claim lost"),
        "a heartbeat with the reclaimed claim's token must fail with `claim lost`: \
         status {}, stderr {}",
        stale.status,
        text(&stale.stderr)
    );
    pt.ok_as(
        "agent",
        &["heartbeat", "PT-1", "--claim", &token_b, "--lease", "2h"],
    );

    let stale_release = pt.run_as("agent", &["release", "PT-1", "--claim", &token_a]);
    assert!(
        !stale_release.status.success(),
        "a release with the reclaimed claim's token handed back the live claim"
    );
    assert_eq!(show(&pt, "PT-1")["status"], "in_progress");
    pt.ok_as(
        "agent",
        &["release", "PT-1", "--claim", &token_b, "-m", "handing back"],
    );
    assert_eq!(show(&pt, "PT-1")["status"], "todo");
}

#[test]
fn over_mcp_a_stale_session_of_the_same_actor_is_told_the_claim_is_lost() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Compact the event archive"]);
    let mut a = Mcp::start(&pt, "agent");
    let mut b = Mcp::start(&pt, "agent");
    let claimed_a = a
        .call("task_claim", json!({"id": "PT-1", "lease_minutes": 10}))
        .unwrap();
    expire_lease(&pt, "PT-1");
    pt.ok_as("operator", &["reclaim", "--apply"]);
    let claimed_b = b
        .call("task_claim", json!({"id": "PT-1", "lease_minutes": 10}))
        .unwrap();

    // Session A as it is written today: task id only.
    let renewed = a.call("task_heartbeat", json!({"id": "PT-1"}));
    assert!(
        renewed.is_err(),
        "session A renewed session B's claim on the actor name alone: {renewed:?}"
    );
    let released = a.call("task_release", json!({"id": "PT-1"}));
    assert!(
        released.is_err(),
        "session A released session B's claim: {released:?}"
    );

    let (token_a, token_b) = (
        token(&claimed_a, "task_claim"),
        token(&claimed_b, "task_claim"),
    );
    match a.call(
        "task_heartbeat",
        json!({"id": "PT-1", "claim_token": token_a}),
    ) {
        Err(e) => assert!(e.contains("claim lost"), "{e}"),
        Ok(v) => panic!("the reclaimed claim's token renewed the live claim: {v}"),
    }
    b.call(
        "task_heartbeat",
        json!({"id": "PT-1", "claim_token": token_b, "lease_minutes": 60}),
    )
    .unwrap();
    assert!(
        a.call(
            "task_release",
            json!({"id": "PT-1", "claim_token": token_a})
        )
        .is_err(),
        "the reclaimed claim's token released the live claim"
    );
    b.call(
        "task_release",
        json!({"id": "PT-1", "claim_token": token_b, "reason": "handing back"}),
    )
    .unwrap();
    assert_eq!(show(&pt, "PT-1")["status"], "todo");
}

// --- 2. a lease that ran out does not outlive the work ----------------------

#[test]
fn starting_a_task_whose_lease_ran_out_takes_the_claim_over() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Refresh the dependency report"]);
    pt.ok_as("agent", &["claim", "PT-1", "--lease", "10m"]);
    expire_lease(&pt, "PT-1");
    pt.ok_as("other", &["start", "PT-1"]);
    let c = show(&pt, "PT-1")["claim"].clone();
    assert_eq!(
        c["by"], "other",
        "start left the run-out claim with its old holder: {c}"
    );
    assert!(
        c["expires_at"].is_null(),
        "start kept the dead lease, so a reclaim would undo the start: {c}"
    );
    let r = pt.json(&["reclaim", "--apply"]);
    assert_eq!(
        r["reclaimed"].as_array().map(Vec::len),
        Some(0),
        "reclaim returned work that was just started: {r}"
    );
    assert_eq!(show(&pt, "PT-1")["status"], "in_progress");
}

/// A claim on a task whose lease ran out either takes it over or is refused
/// with an error that says why (the lease expired, so a reclaim frees it);
/// "already claimed by <holder>" hides that the holder is gone.
#[test]
fn claiming_a_task_whose_lease_ran_out_takes_it_over_or_says_the_lease_expired() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Re-run the nightly checks"]);
    pt.ok_as("agent", &["claim", "PT-1", "--lease", "10m"]);
    expire_lease(&pt, "PT-1");
    let out = pt.run_as("other", &["--json", "claim", "PT-1", "--lease", "30m"]);
    if out.status.success() {
        let c = show(&pt, "PT-1")["claim"].clone();
        assert_eq!(c["by"], "other", "{c}");
        assert_eq!(c["expired"], false, "the takeover kept the dead lease: {c}");
    } else {
        let err = text(&out.stderr);
        assert!(
            err.to_lowercase().contains("lease expired"),
            "a claim refused over a run-out lease must say the lease expired: {err}"
        );
    }
}

// --- 4. releasing an unowned in-progress task is the operator's call -------

#[test]
fn releasing_an_in_progress_task_nobody_holds_needs_force() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Review the backup report"]);
    make_unowned_in_progress(&pt, "PT-1");
    let out = pt.run_as("other", &["release", "PT-1"]);
    assert!(
        !out.status.success(),
        "an in-progress task nobody holds went back to todo without --force: {}",
        text(&out.stdout)
    );
    assert!(
        text(&out.stderr).contains("--force"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(show(&pt, "PT-1")["status"], "in_progress");
    pt.ok_as(
        "operator",
        &["release", "PT-1", "--force", "-m", "nobody on it"],
    );
    assert_eq!(show(&pt, "PT-1")["status"], "todo");
}

#[test]
fn mcp_task_release_refuses_an_in_progress_task_nobody_holds() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Check the certificate expiry dates"]);
    make_unowned_in_progress(&pt, "PT-1");
    let mut m = Mcp::start(&pt, "other");
    for args in [
        json!({"id": "PT-1"}),
        json!({"id": "PT-1", "claim_token": "not-a-real-token"}),
    ] {
        let r = m.call("task_release", args.clone());
        assert!(
            r.is_err(),
            "task_release({args}) handed back a task nobody holds (MCP has no force): {r:?}"
        );
        assert_eq!(show(&pt, "PT-1")["status"], "in_progress");
    }
}

// --- 5. the expired-claims gauge cannot go silently green ------------------

#[test]
fn the_expired_claims_gauge_surfaces_a_failed_query_instead_of_reporting_zero() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Sweep the temporary files"]);
    pt.ok_as("agent", &["claim", "PT-1", "--lease", "10m"]);
    expire_lease(&pt, "PT-1"); // one claim really has expired
    // Break only the expired-claims query: the active-claims query does not
    // read the lease column, so it still succeeds.
    sql(
        &pt,
        "ALTER TABLE tasks RENAME COLUMN claim_expires_at TO claim_expires_at_unreadable",
    );
    let srv = pt.serve();
    let (status, body) = get(&srv.url, "/metrics");
    assert!(
        !body.contains("pt_claims_expired 0"),
        "a failed expired-claims query was reported as zero expired claims \
         (status {status}):\n{body}"
    );
    assert_eq!(
        status, 500,
        "a failed claims query must fail the scrape like the active-claims query does:\n{body}"
    );
    assert!(body.contains("pt_metrics_render_error"), "{body}");
}

// --- 6. the documented tool count is the served one -------------------------

#[test]
fn the_documented_mcp_tool_count_matches_the_server() {
    let pt = Pt::new();
    let names = Mcp::start(&pt, "agent").tool_names();
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for doc in ["README.md", "docs/agent-surface.md"] {
        let text = std::fs::read_to_string(repo.join(doc)).unwrap();
        // Every "<N> tools" in the doc.
        let counts: Vec<usize> = text
            .match_indices(" tools")
            .filter_map(|(i, _)| {
                let digits: String = text[..i]
                    .chars()
                    .rev()
                    .take_while(char::is_ascii_digit)
                    .collect();
                digits.chars().rev().collect::<String>().parse().ok()
            })
            .collect();
        assert!(!counts.is_empty(), "{doc} states no MCP tool count");
        for n in counts {
            assert_eq!(
                n,
                names.len(),
                "{doc} says {n} MCP tools; tools/list serves {}: {names:?}",
                names.len()
            );
        }
    }
    let surface = std::fs::read_to_string(repo.join("docs/agent-surface.md")).unwrap();
    for name in &names {
        assert!(
            surface.contains(name.as_str()),
            "docs/agent-surface.md does not mention the served tool {name}"
        );
    }
}
