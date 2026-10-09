//! Contract tests from the pre-merge review of PR #127 (acceptance criteria
//! gate the close). Each test fails on the PR head for the reason its finding
//! describes and passes once that finding is fixed. The cockpit half
//! (findings 2 and 7: the drawer's 60-event window and the unescaped criterion
//! number) is `dashboard/tests/review_pr127_drawer.test.mjs`.

mod common;
use common::Pt;
use ptask_core::event_log::{self, CommandFingerprint, EventCtx};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Output, Stdio};

fn status(pt: &Pt, id: &str) -> String {
    pt.json(&["show", id])["status"]
        .as_str()
        .unwrap()
        .to_string()
}

/// `(n, text, done)` for each criterion, in number order.
fn criteria(pt: &Pt, id: &str) -> Vec<(i64, String, bool)> {
    pt.json(&["criteria", "ls", id])["criteria"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["n"].as_i64().unwrap(),
                c["text"].as_str().unwrap().to_string(),
                c["done"].as_bool().unwrap(),
            )
        })
        .collect()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn open_db(pt: &Pt) -> ptask_core::Db {
    ptask_core::Db::open(pt.dir.path().join("tasks.db")).unwrap()
}

/// `pt mcp` over stdio: newline-delimited JSON-RPC, as an agent drives it.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn start(pt: &Pt) -> Mcp {
        let mut child = pt
            .command("hal", &["mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut mcp = Mcp {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        mcp.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "review", "version": "0"}
            }),
        )
        .expect("initialize");
        mcp.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        mcp
    }

    fn send(&mut self, msg: Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "pt mcp closed its stdout"
            );
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg["id"] == json!(id) {
                if let Some(e) = msg.get("error") {
                    return Err(e["message"].as_str().unwrap_or_default().to_string());
                }
                return Ok(msg["result"].clone());
            }
        }
    }

    /// A tool call: `Ok(parsed result)`, or `Err(message)` for a JSON-RPC
    /// error or a tool result flagged `isError`.
    fn call(&mut self, tool: &str, args: Value) -> Result<Value, String> {
        let result = self.request("tools/call", json!({"name": tool, "arguments": args}))?;
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if result["isError"] == json!(true) {
            return Err(text);
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---- Finding 1: MCP task_criteria applies a batch whole or not at all ----

#[test]
fn task_criteria_with_a_repeated_check_number_is_all_or_nothing() {
    let pt = Pt::new();
    pt.ok(&[
        "add",
        "--raw",
        "Upgrade the build image",
        "--ac",
        "image builds",
        "--ac",
        "smoke test passes",
    ]);
    let before = criteria(&pt, "PT-1");
    let mut mcp = Mcp::start(&pt);
    let reply = mcp.call(
        "task_criteria",
        json!({"id": "PT-1", "add": ["changelog updated"], "check": [1, 1]}),
    );
    drop(mcp);
    let after = criteria(&pt, "PT-1");
    match reply {
        Err(e) => assert_eq!(
            after, before,
            "task_criteria refused the batch ({e}) but part of it landed: a retry adds the \
             same criterion twice"
        ),
        // Treating the repeat as one check is fine, provided all of it landed.
        Ok(_) => assert!(
            after.iter().any(|c| c.1 == "changelog updated") && after[0].2,
            "task_criteria reported success but did not apply the whole batch: {after:?}"
        ),
    }
}

#[test]
fn task_criteria_with_over_long_evidence_changes_nothing() {
    let pt = Pt::new();
    pt.ok(&[
        "add",
        "--raw",
        "Upgrade the build image",
        "--ac",
        "image builds",
        "--ac",
        "smoke test passes",
    ]);
    let before = criteria(&pt, "PT-1");
    let mut mcp = Mcp::start(&pt);
    let reply = mcp.call(
        "task_criteria",
        json!({
            "id": "PT-1",
            "add": ["metrics exported"],
            "check": [2],
            "evidence": "e".repeat(16 * 1024 + 1),
        }),
    );
    drop(mcp);
    assert!(reply.is_err(), "evidence over 16 KiB must be refused");
    assert_eq!(
        criteria(&pt, "PT-1"),
        before,
        "the refused batch still added its criterion (the evidence limit is checked after \
         the add commits)"
    );
}

#[test]
fn task_criteria_with_a_repeated_uncheck_number_is_all_or_nothing() {
    let pt = Pt::new();
    pt.ok(&[
        "add",
        "--raw",
        "Upgrade the build image",
        "--ac",
        "image builds",
        "--ac",
        "smoke test passes",
    ]);
    pt.ok(&["criteria", "check", "PT-1", "1"]);
    let before = criteria(&pt, "PT-1");
    let mut mcp = Mcp::start(&pt);
    let reply = mcp.call(
        "task_criteria",
        json!({"id": "PT-1", "add": ["rollback tested"], "uncheck": [1, 1]}),
    );
    drop(mcp);
    let after = criteria(&pt, "PT-1");
    match reply {
        Err(e) => assert_eq!(
            after, before,
            "task_criteria refused the batch ({e}) but part of it landed"
        ),
        Ok(_) => assert!(
            after.iter().any(|c| c.1 == "rollback tested") && !after[0].2,
            "task_criteria reported success but did not apply the whole batch: {after:?}"
        ),
    }
}

// ---- Finding 3: criteria events and `pt undo` ----
// The same transparency rule as PR #122's task.noted: a criteria change is
// neither an undo target nor a reason for undo to skip the change it follows.

#[test]
fn a_criteria_change_after_your_close_does_not_make_undo_reach_back() {
    let pt = Pt::new();
    pt.ok_as("shell", &["add", "--raw", "Older task"]); // PT-1
    pt.ok_as("shell", &["add", "--raw", "Newer task"]); // PT-2
    pt.ok_as("shell", &["done", "PT-1"]);
    pt.ok_as("shell", &["done", "PT-2"]);
    pt.ok_as("shell", &["criteria", "add", "PT-2", "follow-up verified"]);

    let out = pt.run_as("shell", &["undo"]);
    assert_eq!(
        status(&pt, "PT-1"),
        "done",
        "undo reached back past the close the criteria change followed and reopened an \
         older task (stdout: {})",
        stdout(&out)
    );
    assert!(out.status.success(), "undo failed: {}", stderr(&out));
    assert_ne!(
        status(&pt, "PT-2"),
        "done",
        "undo must reverse the close the criteria change followed"
    );
}

#[test]
fn undo_removes_a_task_just_filed_with_criteria() {
    let pt = Pt::new();
    pt.ok_as(
        "shell",
        &["add", "--raw", "Filed by mistake", "--ac", "never needed"],
    ); // PT-1
    let out = pt.run_as("shell", &["undo", "--yes"]);
    assert!(
        out.status.success(),
        "undo of `pt add --ac` was refused: the criteria written in the create's own \
         transaction count as a later change to it: {}",
        stderr(&out)
    );
    assert!(!pt.exists("PT-1"), "undo of the create removes the task");
}

// ---- Finding 4: the optional acceptance field must not change `pt add`'s fingerprint ----

/// What pt 3.42.2 (before `--ac`) journaled as the fingerprint of a keyed
/// `pt add --raw "Keyed filing"`.
const ADD_BEFORE_CRITERIA: &str = r#"Add(AddArgs { title: "Keyed filing", priority: None, description: None, deadline: None, reason: None, raw: true, kind: None, deliverable: None })"#;

#[test]
fn the_add_fingerprint_is_the_same_with_or_without_the_acceptance_field() {
    let pt = Pt::new();
    pt.ok(&["--idempotency-key", "k-fp", "add", "--raw", "Keyed filing"]);
    let journaled = event_log::get_by_uuid(&open_db(&pt), "k-fp")
        .unwrap()
        .expect("the keyed create is journaled under its key")
        .command;
    assert_eq!(
        journaled,
        Some(CommandFingerprint::new("Add", ADD_BEFORE_CRITERIA)),
        "`pt add` fingerprints differently now that AddArgs has an (empty) acceptance list; \
         a None/default field must not change the fingerprint"
    );
}

#[test]
fn a_keyed_add_journaled_before_the_acceptance_field_replays() {
    let pt = Pt::new();
    {
        // What `pt --idempotency-key k-legacy add --raw "Keyed filing"` journaled on 3.42.2.
        let db = open_db(&pt);
        let ctx = EventCtx::local("test")
            .with_uuid("k-legacy")
            .with_command(CommandFingerprint::new("Add", ADD_BEFORE_CRITERIA));
        ptask_core::tasks::create(&db, ptask_core::NewTask::minimal("Keyed filing"), &ctx).unwrap();
    }
    let out = pt.run(&[
        "--idempotency-key",
        "k-legacy",
        "add",
        "--raw",
        "Keyed filing",
    ]);
    assert!(
        out.status.success() && stdout(&out).contains("replayed"),
        "a retry of the same keyed filing across the upgrade must replay, not fail as a \
         different command: {}",
        stderr(&out)
    );
    assert!(!pt.exists("PT-2"), "the replay filed nothing new");
}

// ---- Finding 5: reopening a task resets its criteria checks ----

#[test]
fn reopening_a_task_resets_its_criteria_checks() {
    let pt = Pt::new();
    pt.ok(&[
        "add",
        "--raw",
        "Publish the release notes",
        "--ac",
        "notes reviewed",
    ]);
    pt.ok(&[
        "criteria",
        "check",
        "PT-1",
        "1",
        "-m",
        "reviewed in the draft",
    ]);
    pt.ok(&["done", "PT-1"]);
    pt.ok(&["reopen", "PT-1"]);
    assert_eq!(
        pt.json(&["criteria", "ls", "PT-1"])["unchecked"],
        1,
        "a reopened task kept its checks, so done → reopen → done passes the gate without \
         re-verifying"
    );
    assert!(
        !pt.run(&["done", "PT-1"]).status.success(),
        "re-closing a reopened task needs its criteria checked again"
    );
}

#[test]
fn an_undone_close_also_resets_the_criteria_checks() {
    let pt = Pt::new();
    pt.ok_as(
        "shell",
        &[
            "add",
            "--raw",
            "Publish the release notes",
            "--ac",
            "notes reviewed",
        ],
    );
    pt.ok_as("shell", &["criteria", "check", "PT-1", "1"]);
    pt.ok_as("shell", &["done", "PT-1"]);
    pt.ok_as("shell", &["undo"]); // reopens PT-1
    assert_ne!(status(&pt, "PT-1"), "done");
    assert_eq!(
        pt.json(&["criteria", "ls", "PT-1"])["unchecked"],
        1,
        "the close was undone but its checks survived it"
    );
}

// ---- Finding 6: a git close refused by the gate is not silent ----

#[test]
fn a_git_close_refused_by_the_criteria_gate_is_logged_and_journaled() {
    const SECRET: &str = "review-webhook-secret";
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    let pt = Pt::new();
    pt.ok(&[
        "add",
        "--raw",
        "Fix the flaky integration test",
        "--ac",
        "green three runs in a row",
    ]); // PT-1

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut server = pt
        .command("test", &["serve", "--bind", &format!("127.0.0.1:{port}")])
        .env("PTASK_GITHUB_WEBHOOK_SECRET", SECRET)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let up = (0..200).any(|_| {
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            false
        }
    });
    assert!(up, "pt serve did not come up");

    let body = json!({
        "ref": "refs/heads/main",
        "repository": {"full_name": "example/project", "default_branch": "main"},
        "commits": [{"id": COMMIT, "message": "Fix the flaky integration test (closes PT-1)"}],
    })
    .to_string();
    let signature = ptask_server::webhooks::sign(body.as_bytes(), SECRET);
    let reply = reqwest::blocking::Client::new()
        .post(format!("http://127.0.0.1:{port}/webhook/github"))
        .header("Content-Type", "application/json")
        .header("X-GitHub-Event", "push")
        .header("X-Hub-Signature-256", format!("sha256={signature}"))
        .body(body)
        .send()
        .unwrap();
    let reply_status = reply.status();
    let reply_body = reply.text().unwrap_or_default();
    let _ = server.kill();
    let mut log = String::new();
    server
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut log)
        .unwrap();
    let _ = server.wait();

    assert!(reply_status.is_success(), "{reply_status}: {reply_body}");
    assert_ne!(status(&pt, "PT-1"), "done", "the gate holds (sanity)");
    assert!(
        log.lines()
            .any(|l| l.contains("WARN") && l.contains("PT-1")),
        "a git close refused by the criteria gate left no warn-level log line naming the task \
         (the refusal is only in the webhook's response body: {reply_body}); log:\n{log}"
    );
    let journal = pt.json(&["log", "PT-1"]).to_string();
    assert!(
        journal.contains(&COMMIT[..12]),
        "a git close refused by the criteria gate left nothing on the task's journal: {journal}"
    );
}
