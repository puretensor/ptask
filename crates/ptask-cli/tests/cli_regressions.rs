//! End-to-end regressions that need the real `pt` binary: TTY gating,
//! exit codes and per-process flags (`--idempotency-key`) are invisible to
//! in-process tests.

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn pt(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pt"))
        .arg("--db")
        .arg(db)
        .args(args)
        .env("PTASK_ACTOR", "shell")
        .env("NO_COLOR", "1")
        .env_remove("PTASK_DB")
        .stdin(Stdio::null())
        .output()
        .expect("run pt")
}

fn ok(db: &Path, args: &[&str]) -> serde_json::Value {
    let out = pt(db, args);
    assert!(
        out.status.success(),
        "pt {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null)
}

fn open_titles(db: &Path) -> Vec<String> {
    ok(db, &["--json", "list", "-s", "all", "-n", "100"])
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn undo_of_a_create_needs_a_tty_or_yes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    ok(&db, &["add", "--raw", "keep me"]);

    // No TTY and no --yes: the delete is refused, loudly, and nothing goes.
    let out = pt(&db, &["undo"]);
    assert!(!out.status.success(), "undo deleted without confirmation");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--yes"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(open_titles(&db), ["keep me"]);

    // --yes confirms; the output names the task, not only its uuid.
    let v = ok(&db, &["--json", "undo", "--yes"]);
    assert_eq!(v["action"], "deleted");
    assert_eq!(v["pt_id"], "PT-1");
    assert_eq!(v["title"], "keep me");
    assert!(open_titles(&db).is_empty());
}

#[test]
fn done_twice_journals_one_completion() {
    // Regression (CLI-19): the second `pt done PT-1` wrote another
    // task.completed and interaction, inflating "Completed today".
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    ok(&db, &["add", "--raw", "close once"]);
    ok(&db, &["done", "PT-1"]);
    let again = pt(&db, &["done", "PT-1"]);
    assert!(!again.status.success(), "a second done must be refused");
    assert!(
        String::from_utf8_lossy(&again.stderr).contains("already done"),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    let log = ok(&db, &["--json", "log", "PT-1"]);
    let completions = log
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event_type"] == "task.completed")
        .count();
    assert_eq!(completions, 1);
}

/// Events journaled under `key` (or a per-task `key:<uuid>` child key).
fn events_with_key(db: &Path, key: &str) -> i64 {
    ptask_core::Db::open(db)
        .unwrap()
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM pt_event_log WHERE uuid = ?1 OR uuid LIKE ?1 || ':%'",
                [key],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

#[test]
fn a_retried_add_with_the_same_key_returns_the_same_task() {
    // Regression (CORE-7): the retry hit "UNIQUE constraint failed:
    // pt_event_log.uuid" and exited 1, though the flag promises "a retried
    // command with the same key returns ok without re-applying".
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let first = ok(
        &db,
        &["--json", "--idempotency-key", "k-add", "add", "buy bread"],
    );
    let again = ok(
        &db,
        &["--json", "--idempotency-key", "k-add", "add", "buy bread"],
    );
    assert_eq!(first["pt_id"], "PT-1");
    assert_eq!(again["pt_id"], first["pt_id"]);
    assert_eq!(again["id"], first["id"]);
    assert_eq!(open_titles(&db), ["buy bread"]);
}

#[test]
fn a_key_used_for_one_command_is_not_a_replay_of_another() {
    // Regression (CORE-7): replay only checked that the key existed, so
    // `--idempotency-key K add X` then `--idempotency-key K done PT-1`
    // printed "replayed", exited 0 and left the task todo.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    ok(&db, &["--idempotency-key", "k-1", "add", "--raw", "x"]);
    let done = pt(&db, &["--idempotency-key", "k-1", "done", "PT-1"]);
    assert!(!done.status.success(), "reused key reported as a replay");
    assert_eq!(ok(&db, &["--json", "show", "PT-1"])["status"], "todo");

    // Another task under the same verb is a mismatch too.
    ok(&db, &["add", "--raw", "y"]);
    ok(&db, &["--idempotency-key", "k-2", "start", "PT-1"]);
    let other = pt(&db, &["--idempotency-key", "k-2", "start", "PT-2"]);
    assert!(!other.status.success(), "key reused for another task");
    assert_eq!(ok(&db, &["--json", "show", "PT-2"])["status"], "todo");
}

#[test]
fn every_keyed_task_verb_can_be_retried() {
    // Regression (CLI-6): add/edit/start/depend/snooze failed the retry
    // with a raw UNIQUE error; dismiss/reopen with a domain error.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    ok(&db, &["add", "--raw", "first"]);
    ok(&db, &["add", "--raw", "second"]);
    let verbs: [(&str, &[&str]); 10] = [
        ("k-edit", &["edit", "PT-1", "--title", "first (edited)"]),
        ("k-priority", &["priority", "PT-1", "urgent"]),
        ("k-start", &["start", "PT-1"]),
        ("k-depend", &["depend", "PT-1", "--on", "PT-2"]),
        ("k-snooze", &["snooze", "PT-2", "2099-01-01"]),
        ("k-kind", &["kind", "PT-2", "scout"]),
        ("k-promote", &["promote", "PT-2"]),
        ("k-dismiss", &["dismiss", "PT-2"]),
        ("k-reopen", &["reopen", "PT-2"]),
        ("k-done", &["done", "PT-2"]),
    ];
    for (key, args) in verbs {
        let mut argv = vec!["--idempotency-key", key];
        argv.extend_from_slice(args);
        ok(&db, &argv);
        let retry = pt(&db, &argv);
        assert!(
            retry.status.success(),
            "{args:?} retry failed: {}",
            String::from_utf8_lossy(&retry.stderr)
        );
        assert_eq!(events_with_key(&db, key), 1, "{args:?} re-applied");
    }
    let rm = ["--idempotency-key", "k-rm", "rm", "PT-2", "--yes"];
    ok(&db, &rm);
    assert!(pt(&db, &rm).status.success(), "rm retry failed");
}

#[test]
fn keyed_goal_verbs_can_be_retried() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    ok(&db, &["add", "--raw", "task"]);
    let add = [
        "--json",
        "--idempotency-key",
        "g-add",
        "goal",
        "add",
        "mission",
    ];
    let first = ok(&db, &add);
    let again = ok(&db, &add);
    assert_eq!(first["id"], "G-1");
    assert_eq!(again["id"], "G-1");
    let verbs: [(&str, &[&str]); 3] = [
        ("g-link", &["goal", "link", "PT-1", "G-1"]),
        ("g-unlink", &["goal", "unlink", "PT-1"]),
        ("g-done", &["goal", "done", "G-1"]),
    ];
    for (key, args) in verbs {
        let mut argv = vec!["--idempotency-key", key];
        argv.extend_from_slice(args);
        ok(&db, &argv);
        let retry = pt(&db, &argv);
        assert!(
            retry.status.success(),
            "{args:?} retry failed: {}",
            String::from_utf8_lossy(&retry.stderr)
        );
        assert_eq!(events_with_key(&db, key), 1, "{args:?} re-applied");
    }
}

#[test]
fn undo_of_a_completion_needs_no_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    ok(&db, &["add", "--raw", "finish me"]);
    ok(&db, &["done", "PT-1"]);
    let v = ok(&db, &["--json", "undo"]);
    assert_eq!(v["action"], "reopened");
    assert_eq!(v["was"], "completed");
    let shown = ok(&db, &["--json", "show", "PT-1"]);
    assert_eq!(shown["status"], "todo");
}
