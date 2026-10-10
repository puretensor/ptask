//! Contract tests from the review of puretensor/ptask#124 (duplicate check
//! at filing time, `pt dupes`, `pt merge`). Each test fails on the PR head
//! for the reason its name gives and passes once that finding is fixed;
//! where the review allowed two fixes (repair or refuse), either passes.
//! Black-box against the built `pt`, on throwaway databases only.

mod common;
use common::Pt;
use ptask_core::Db;
use serde_json::{Value, json};
use std::process::{Output, Stdio};

fn db(pt: &Pt) -> Db {
    Db::open(pt.dir.path().join("tasks.db")).unwrap()
}

fn show(pt: &Pt, id: &str) -> Value {
    pt.json(&["show", id])
}

fn status(pt: &Pt, id: &str) -> String {
    show(pt, id)["status"].as_str().unwrap().to_string()
}

fn uuid(pt: &Pt, id: &str) -> String {
    show(pt, id)["id"].as_str().unwrap().to_string()
}

fn is_open(status: &str) -> bool {
    !matches!(status, "done" | "dismissed")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `(from, to)` PT-ids of every `task_links` row of `kind`, sorted.
fn links(pt: &Pt, kind: &str) -> Vec<(String, String)> {
    db(pt)
        .with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT f.pt_id, t.pt_id FROM task_links l
                   JOIN tasks f ON f.id = l.from_uuid
                   JOIN tasks t ON t.id = l.to_uuid
                  WHERE l.kind = ?1 ORDER BY f.pt_id, t.pt_id",
            )?;
            let rows = stmt
                .query_map([kind], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<(String, String)>, _>>()?;
            Ok(rows)
        })
        .unwrap()
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

fn labels(pt: &Pt, id: &str) -> Vec<String> {
    let task = uuid(pt, id);
    db(pt)
        .with_conn(|c| {
            let mut stmt =
                c.prepare("SELECT label FROM task_labels WHERE task_uuid = ?1 ORDER BY label")?;
            let rows = stmt
                .query_map([&task], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            Ok(rows)
        })
        .unwrap()
}

/// Every `pt merge A --into B` the output suggests, as `(A, B)`.
fn merge_hints(text: &str) -> Vec<(String, String)> {
    let words: Vec<&str> = text.split_whitespace().collect();
    words
        .windows(5)
        .filter(|w| w[0] == "pt" && w[1] == "merge" && w[3] == "--into")
        .map(|w| pair(w[2], w[4]))
        .collect()
}

/// The refusal branch the review allows for an undo of a merge: nothing
/// changed, and the message names what the merge moved (the dependent
/// PT-3 and the prerequisite PT-4) so it can be moved back by hand.
fn assert_refused_undo_names_what_moved(pt: &Pt, undo: &Output) {
    let err = stderr(undo);
    assert_eq!(
        status(pt, "PT-2"),
        "dismissed",
        "a refused undo changed nothing"
    );
    assert!(
        err.contains("PT-3") && err.contains("PT-4"),
        "a refused undo of a merge must name what moved (PT-3, PT-4): {err}"
    );
}

/// PT-1 the target; PT-2 merged into it by mistake; PT-3 waits on PT-2;
/// PT-2 waits on PT-4.
fn setup_mistaken_merge(pt: &Pt) {
    pt.ok(&["add", "--raw", "Calibrate the alpha sensor array"]); // PT-1
    pt.ok(&[
        "add",
        "--raw",
        "Alpha sensor array calibration, second pass",
    ]); // PT-2
    pt.ok(&["add", "--raw", "Publish the calibration report"]); // PT-3
    pt.ok(&["add", "--raw", "Order replacement probes"]); // PT-4
    pt.ok(&["depend", "PT-3", "--on", "PT-2"]);
    pt.ok(&["depend", "PT-2", "--on", "PT-4"]);
}

// Finding 1: `pt undo` of a merge reopens the duplicate and leaves the rest
// of the merge in place, so its dependents wait on the wrong task.

#[test]
fn undoing_a_merge_never_lets_a_dependent_close_while_the_duplicate_is_open() {
    let pt = Pt::new();
    setup_mistaken_merge(&pt);
    pt.ok(&["merge", "PT-2", "--into", "PT-1"]);

    let undo = pt.run(&["undo", "--yes"]);
    if !undo.status.success() {
        assert_refused_undo_names_what_moved(&pt, &undo);
        return;
    }
    assert_eq!(status(&pt, "PT-2"), "todo", "the undo reopened PT-2");
    pt.ok(&["done", "PT-4"]);
    pt.ok(&["done", "PT-1"]);
    let early = pt.run(&["done", "PT-3"]);
    assert!(
        !early.status.success(),
        "PT-3 closed while its prerequisite PT-2 is open again: the undo left \
         PT-3 waiting on the merge target PT-1 instead of PT-2"
    );
}

#[test]
fn undoing_a_merge_restores_the_target_or_is_refused() {
    let pt = Pt::new();
    setup_mistaken_merge(&pt);
    pt.ok(&["priority", "PT-2", "urgent"]);
    pt.ok(&[
        "edit",
        "PT-2",
        "--label",
        "batch-b",
        "--deadline",
        "2031-03-01",
    ]);
    pt.ok(&["merge", "PT-2", "--into", "PT-1"]);

    let undo = pt.run(&["undo", "--yes"]);
    if !undo.status.success() {
        assert_refused_undo_names_what_moved(&pt, &undo);
        return;
    }
    let target = show(&pt, "PT-1");
    let mut left = Vec::new();
    if target["priority"] != 2 {
        left.push(format!("priority {} (was 2)", target["priority"]));
    }
    if !target["deadline"].is_null() {
        left.push(format!("deadline {} (had none)", target["deadline"]));
    }
    if labels(&pt, "PT-1").iter().any(|l| l == "batch-b") {
        left.push("label batch-b (had none)".into());
    }
    if target["merged_in"]
        .as_array()
        .is_some_and(|m| !m.is_empty())
    {
        left.push(format!("merged_in {}", target["merged_in"]));
    }
    let deps = links(&pt, "depends_on");
    if deps.contains(&pair("PT-1", "PT-4")) {
        left.push("a prerequisite PT-4 (had none)".into());
    }
    if !deps.contains(&pair("PT-3", "PT-2")) {
        left.push(format!("PT-3 no longer waits on PT-2 (edges {deps:?})"));
    }
    assert!(
        left.is_empty(),
        "the undo reopened PT-2 but left the merge on PT-1: {}",
        left.join("; ")
    );
}

// Finding 2: a merge into a closed target moves the duplicate's dependents
// onto work that is already closed, which unblocks them.

#[test]
fn merging_into_a_done_task_never_unblocks_the_duplicates_dependents() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Rotate the gamma service credential"]); // PT-1
    pt.ok(&["done", "PT-1"]);
    // With nothing waiting on it, a duplicate of done work still merges.
    pt.ok(&["add", "--raw", "Gamma service credential rotation"]); // PT-2
    pt.ok(&["merge", "PT-2", "--into", "PT-1"]);

    pt.ok(&["add", "--raw", "Gamma service credential rotation, again"]); // PT-3
    pt.ok(&["add", "--raw", "Roll the new credential out"]); // PT-4
    pt.ok(&["depend", "PT-4", "--on", "PT-3"]);
    let merged = pt.run(&["merge", "PT-3", "--into", "PT-1"]);
    let closed = pt.run(&["done", "PT-4"]);
    assert!(
        !merged.status.success() && !closed.status.success(),
        "merging PT-3 into done PT-1 was accepted ({}) and PT-4, which waited on PT-3, \
         could then close ({}): the merge unblocked it",
        merged.status.success(),
        closed.status.success()
    );
    assert_eq!(
        status(&pt, "PT-3"),
        "todo",
        "the refused merge changed nothing"
    );
}

#[test]
fn add_hint_never_suggests_merging_into_a_done_task() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Rotate the gamma service credential"]); // PT-1
    pt.ok(&["done", "PT-1"]);
    let human = pt.ok(&[
        "--no-color",
        "add",
        "--raw",
        "Gamma service credential rotation",
    ]);
    assert!(
        human.contains("duplicate?"),
        "precondition: PT-1 is reported: {human}"
    );
    for (dup, into) in merge_hints(&human) {
        let st = status(&pt, &into);
        assert!(
            is_open(&st),
            "the add hint suggests `pt merge {dup} --into {into}`, but {into} is {st}:\n{human}"
        );
    }
}

// Finding 3: `--unique` / skip_if_duplicate refuse distinct work whose titles
// differ only in an identifier, and a refusal over MCP replies "ok": true.

#[test]
fn unique_refuses_rewordings_but_not_titles_that_differ_only_in_identifiers() {
    // A reworded duplicate is still refused.
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Storage migration program: phase 2"]);
    let same = pt.run(&[
        "--json",
        "add",
        "--unique",
        "--raw",
        "Phase 2 of the storage migration program",
    ]);
    assert!(
        !same.status.success(),
        "precondition: a reworded duplicate is refused"
    );

    let distinct = [
        (
            "Storage migration program: phase 2",
            "Storage migration program: phase 3",
        ),
        (
            "Replace the failed disk in web-01 slot 4",
            "Replace the failed disk in web-02 slot 4",
        ),
        (
            "Confirm pending travel booking 2031-01-10 to 2031-01-15 in the ledger",
            "Confirm pending travel booking 2031-01-20 to 2031-01-25 in the ledger",
        ),
        (
            "Review flagged build artifact a1b2c3d4e5f6",
            "Review flagged build artifact 0f9e8d7c6b5a",
        ),
    ];
    let mut refused = Vec::new();
    for (existing, new) in distinct {
        let pt = Pt::new();
        pt.ok(&["add", "--raw", existing]);
        let out = pt.run(&["--json", "add", "--unique", "--raw", new]);
        if !out.status.success() {
            refused.push(format!("{new:?} (existing: {existing:?})"));
        }
    }
    assert!(
        refused.is_empty(),
        "--unique refused distinct work that differs only in an identifier:\n  {}",
        refused.join("\n  ")
    );
}

/// One `tools/call` against `pt mcp` over stdio: the JSON-RPC reply.
fn mcp_call(pt: &Pt, tool: &str, arguments: Value) -> Value {
    use std::io::{BufRead, BufReader, Write};
    let mut child = pt
        .command("agent-m", &["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let reply_to = |id: i64| -> Value {
        loop {
            let line = rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("pt mcp replied");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    };
    let mut send = |v: Value| {
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    };
    send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-03-26", "capabilities": {},
        "clientInfo": {"name": "review", "version": "0"}}}),
    );
    reply_to(1);
    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": tool, "arguments": arguments}}));
    let reply = reply_to(2);
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    reply
}

#[test]
fn a_refused_add_over_mcp_is_not_reported_as_ok() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Storage migration program: phase 2"]);
    let reply = mcp_call(
        &pt,
        "task_add",
        json!({"text": "Phase 2 of the storage migration program", "skip_if_duplicate": true}),
    );
    let tasks: i64 = db(&pt)
        .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(tasks, 1, "precondition: skip_if_duplicate created nothing");
    // A JSON-RPC error or a tool error is an explicit non-ok result.
    if reply.get("error").is_some() || reply["result"]["isError"] == true {
        return;
    }
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    let body: Value = serde_json::from_str(text).unwrap();
    assert_ne!(
        body["ok"], true,
        "a refused task_add replied \"ok\": true, so an agent that checks `ok` \
         believes it filed the task: {body:#}"
    );
    assert!(
        body["created"] == false || body["skipped"] == true,
        "the refusal says nothing was created: {body:#}"
    );
}

// Finding 4: merges are recorded only as journal events, not as the
// `duplicate_of` task link the schema provides (V012), so pre-existing links
// are invisible and the journal-derived markers go stale.

#[test]
fn a_merge_records_a_duplicate_of_link() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Archive the old build logs"]); // PT-1
    pt.ok(&["add", "--raw", "Old build logs archive"]); // PT-2
    pt.ok(&["merge", "PT-2", "--into", "PT-1"]);
    assert_eq!(
        links(&pt, "duplicate_of"),
        [pair("PT-2", "PT-1")],
        "the merge wrote no task_links duplicate_of row"
    );
}

#[test]
fn reopening_a_merged_duplicate_clears_every_merge_marker() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Archive the old build logs"]); // PT-1
    pt.ok(&["add", "--raw", "Old build logs archive"]); // PT-2
    pt.ok(&["merge", "PT-2", "--into", "PT-1"]);
    pt.ok(&["reopen", "PT-2"]);
    assert!(
        links(&pt, "duplicate_of").is_empty(),
        "the duplicate_of link outlived the reopen"
    );
    let target = show(&pt, "PT-1");
    assert!(
        target["merged_in"].as_array().is_none_or(|m| m.is_empty()),
        "PT-1 still lists reopened PT-2 as merged in: {}",
        target["merged_in"]
    );
    // A later plain dismissal is not a merge.
    pt.ok(&["dismiss", "PT-2"]);
    let dup = show(&pt, "PT-2");
    assert!(
        dup["duplicate_of"].is_null(),
        "a plain dismissal of PT-2 after it was reopened reads as a merge: duplicate_of {}",
        dup["duplicate_of"]
    );
}

#[test]
fn pre_existing_duplicate_of_links_show_as_merge_markers() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Archive the old build logs"]); // PT-1
    pt.ok(&["add", "--raw", "Old build logs archive"]); // PT-2
    pt.ok(&["dismiss", "PT-2"]);
    // Recorded the way merges were recorded before #124 (a V012 link).
    let (dup, canon) = (uuid(&pt, "PT-2"), uuid(&pt, "PT-1"));
    db(&pt)
        .with_conn(|c| {
            c.execute(
                "INSERT INTO task_links (from_uuid, to_uuid, kind, created_at)
                 VALUES (?1, ?2, 'duplicate_of', '2026-01-01T00:00:00+00:00')",
                [&dup, &canon],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        show(&pt, "PT-2")["duplicate_of"],
        "PT-1",
        "a recorded duplicate_of link does not show as a merge marker"
    );
    assert_eq!(show(&pt, "PT-1")["merged_in"], json!(["PT-2"]));
}

// Finding 5: a merge silently drops what hangs off the duplicate other than
// dependents, prerequisites and labels.

/// `pt merge PT-2 --into PT-1`: `Ok` when it merged, the refusal otherwise.
fn merge_2_into_1(pt: &Pt) -> Result<(), String> {
    let out = pt.run(&["merge", "PT-2", "--into", "PT-1"]);
    if out.status.success() {
        Ok(())
    } else {
        Err(stderr(&out))
    }
}

fn recurs(pt: &Pt, id: &str) -> bool {
    let task = uuid(pt, id);
    db(pt)
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM pt_recurrence WHERE task_uuid = ?1)",
                [&task],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

fn insert_link(pt: &Pt, from: &str, to: &str, kind: &str) {
    let (from, to) = (uuid(pt, from), uuid(pt, to));
    db(pt)
        .with_conn(|c| {
            c.execute(
                "INSERT INTO task_links (from_uuid, to_uuid, kind, created_at)
                 VALUES (?1, ?2, ?3, '2026-01-01T00:00:00+00:00')",
                [from.as_str(), to.as_str(), kind],
            )?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn merging_a_recurring_duplicate_carries_the_rule_or_is_refused() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Check the nightly export job"]); // PT-1
    pt.ok(&["add", "Check the nightly export job every monday"]); // PT-2
    assert!(recurs(&pt, "PT-2"), "precondition: PT-2 recurs");
    match merge_2_into_1(&pt) {
        Err(why) => {
            assert!(
                why.to_lowercase().contains("recur"),
                "the refusal does not name the recurrence it would lose: {why}"
            );
            assert_eq!(status(&pt, "PT-2"), "todo");
        }
        Ok(()) => assert!(
            recurs(&pt, "PT-1"),
            "the merge dismissed recurring PT-2 and PT-1 does not recur: the weekly series ended silently"
        ),
    }
}

#[test]
fn merging_a_goal_linked_duplicate_carries_the_goal_or_is_refused() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Check the nightly export job"]); // PT-1
    pt.ok(&["add", "--raw", "Nightly export job check"]); // PT-2
    pt.ok(&["goal", "add", "Keep exports verifiable"]); // G-1
    pt.ok(&["goal", "link", "PT-2", "G-1"]);
    match merge_2_into_1(&pt) {
        Err(why) => assert!(
            why.contains("G-1") || why.to_lowercase().contains("goal"),
            "the refusal does not name the goal link it would lose: {why}"
        ),
        Ok(()) => {
            let chain = show(&pt, "PT-1")["goal_chain"].clone();
            assert!(
                chain
                    .as_array()
                    .is_some_and(|c| c.iter().any(|g| g["id"] == "G-1")),
                "the merge dropped PT-2's goal G-1: PT-1's goal chain is {chain}"
            );
        }
    }
}

#[test]
fn merging_a_duplicate_carries_its_discovered_from_link_or_is_refused() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Check the nightly export job"]); // PT-1
    pt.ok(&["add", "--raw", "Nightly export job check"]); // PT-2
    pt.ok(&["add", "--raw", "Audit the export pipeline"]); // PT-3
    insert_link(&pt, "PT-2", "PT-3", "discovered_from");
    match merge_2_into_1(&pt) {
        Err(why) => assert!(
            why.to_lowercase().contains("discovered"),
            "the refusal does not name the provenance link it would lose: {why}"
        ),
        Ok(()) => assert!(
            links(&pt, "discovered_from").contains(&pair("PT-1", "PT-3")),
            "the merge dropped PT-2's discovered_from link to PT-3: {:?}",
            links(&pt, "discovered_from")
        ),
    }
}

#[test]
fn merging_a_duplicate_carries_its_subtasks_or_is_refused() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Check the nightly export job"]); // PT-1
    pt.ok(&["add", "--raw", "Nightly export job check"]); // PT-2
    pt.ok(&["add", "--raw", "Rotate the export job logs"]); // PT-3, a subtask of PT-2
    let (child, parent) = (uuid(&pt, "PT-3"), uuid(&pt, "PT-2"));
    db(&pt)
        .with_conn(|c| {
            c.execute(
                "UPDATE tasks SET parent_uuid = ?1 WHERE id = ?2",
                [&parent, &child],
            )?;
            Ok(())
        })
        .unwrap();
    insert_link(&pt, "PT-3", "PT-2", "subtask_of");
    match merge_2_into_1(&pt) {
        Err(why) => assert!(
            why.contains("PT-3") || why.to_lowercase().contains("subtask"),
            "the refusal does not name the subtask it would orphan: {why}"
        ),
        Ok(()) => {
            let parent_of: Option<String> = db(&pt)
                .with_conn(|c| {
                    Ok(c.query_row(
                        "SELECT p.pt_id FROM tasks c JOIN tasks p ON p.id = c.parent_uuid
                          WHERE c.id = ?1",
                        [&child],
                        |r| r.get(0),
                    )?)
                })
                .ok();
            assert_eq!(
                parent_of.as_deref(),
                Some("PT-1"),
                "after the merge PT-3 is still a subtask of dismissed PT-2"
            );
        }
    }
}

// Finding 6: the add hint suggests merging into a task the merge refuses.

#[test]
fn add_hint_never_suggests_merging_into_a_dismissed_task() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Tune the queue worker pool size"]); // PT-1
    pt.ok(&["dismiss", "PT-1"]);
    let human = pt.ok(&[
        "--no-color",
        "add",
        "--raw",
        "Queue worker pool size tuning",
    ]);
    assert!(
        human.contains("duplicate?"),
        "precondition: PT-1 is reported: {human}"
    );
    for (dup, into) in merge_hints(&human) {
        assert_ne!(
            status(&pt, &into),
            "dismissed",
            "the add hint suggests `pt merge {dup} --into {into}`, which the merge refuses \
             because {into} is dismissed:\n{human}"
        );
    }
}

// Finding 7: the PR's fixtures carry internal names into this public
// repository. The names are not written here: the test reads them from an
// untracked file named by $PTASK_PUBLIC_DENYLIST (one phrase per line, `#`
// comments) and skips without it. Failures name the file, line and entry
// number, never the phrase, because CI logs of a public repository are public.

/// Files puretensor/ptask#124 adds or changes, and this file.
const SCANNED: &[&str] = &[
    "README.md",
    "crates/ptask-cli/src/main.rs",
    "crates/ptask-cli/tests/dupes.rs",
    "crates/ptask-cli/tests/review_pr124.rs",
    "crates/ptask-core/src/dupes.rs",
    "crates/ptask-server/src/mcp.rs",
    "docs/agent-surface.md",
    "docs/cli-reference.md",
];

/// Lowercase words separated by single spaces: punctuation, case and line
/// layout do not hide a phrase.
fn words(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn changed_files_carry_no_denylisted_internal_names() {
    let Some(list) = std::env::var_os("PTASK_PUBLIC_DENYLIST") else {
        eprintln!("skipped: set PTASK_PUBLIC_DENYLIST to an untracked denylist file");
        return;
    };
    let list = std::fs::read_to_string(&list)
        .unwrap_or_else(|e| panic!("PTASK_PUBLIC_DENYLIST {list:?}: {e}"));
    let entries: Vec<String> = list
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(words)
        .filter(|w| !w.is_empty())
        .collect();
    assert!(!entries.is_empty(), "the denylist file has no entries");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut hits = Vec::new();
    for file in SCANNED {
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        for (n, entry) in entries.iter().enumerate() {
            let needle = format!(" {entry} ");
            let lines: Vec<usize> = text
                .lines()
                .enumerate()
                .filter(|(_, l)| format!(" {} ", words(l)).contains(&needle))
                .map(|(i, _)| i + 1)
                .collect();
            if !lines.is_empty() {
                hits.push(format!("{file}:{lines:?}: denylist entry #{}", n + 1));
            } else if format!(" {} ", words(&text)).contains(&needle) {
                hits.push(format!("{file}: denylist entry #{} (across lines)", n + 1));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "internal names from the denylist are in public files:\n  {}",
        hits.join("\n  ")
    );
}
