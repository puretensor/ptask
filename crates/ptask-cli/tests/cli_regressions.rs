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
