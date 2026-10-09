//! Black-box tests for acceptance criteria: `pt add --ac`, `pt criteria`,
//! and the close gate they put on `pt done` (and every other closer).

mod common;
use common::Pt;

#[test]
fn a_task_closes_only_when_every_criterion_is_checked() {
    let pt = Pt::new();
    pt.ok(&[
        "add",
        "--raw",
        "Ship the restore drill",
        "--ac",
        "restic check passes",
        "--ac",
        "drill runs on the weekly timer",
    ]);
    let refused = pt.run(&["done", "PT-1"]);
    assert!(!refused.status.success());
    let err = String::from_utf8_lossy(&refused.stderr);
    assert!(err.contains("has unchecked acceptance criteria"), "{err}");
    assert!(
        err.contains("1. restic check passes") && err.contains("2. drill runs"),
        "{err}"
    );
    assert_eq!(pt.json(&["show", "PT-1"])["status"], "todo");

    pt.ok_as(
        "hal",
        &[
            "criteria",
            "check",
            "PT-1",
            "1",
            "-m",
            "restic check: 0 errors",
        ],
    );
    let shown = pt.ok(&["--no-color", "show", "PT-1"]);
    assert!(shown.contains("1/2 checked"), "{shown}");
    assert!(
        shown.contains("[x] 1. restic check passes") && shown.contains("· hal"),
        "{shown}"
    );
    assert!(shown.contains("[ ] 2. drill runs"), "{shown}");
    let brief = pt.ok(&["context", "PT-1"]);
    assert!(brief.contains("## Acceptance criteria"), "{brief}");
    assert!(brief.contains("- [x] 1. restic check passes"), "{brief}");
    assert!(
        brief.contains("- [ ] 2. drill runs on the weekly timer"),
        "{brief}"
    );

    assert!(
        !pt.run(&["done", "PT-1"]).status.success(),
        "one still open"
    );
    pt.ok(&["criteria", "check", "PT-1", "2"]);
    pt.ok(&["done", "PT-1"]);
    let v = pt.json(&["show", "PT-1"]);
    assert_eq!(v["status"], "done");
    assert_eq!(v["criteria"][0]["evidence"], "restic check: 0 errors");
}

#[test]
fn criteria_can_be_added_unchecked_and_removed_with_a_trail() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Rotate the Bedrock key"]);
    pt.ok(&["done", "PT-1"]); // no criteria: closes as before
    pt.ok(&["add", "--raw", "Rotate the Tailscale key"]);
    pt.ok(&["criteria", "add", "PT-2", "old", "key", "revoked"]);
    pt.ok(&["criteria", "add", "PT-2", "services", "reconnected"]);
    pt.ok(&["criteria", "check", "PT-2", "1"]);
    pt.ok(&["criteria", "uncheck", "PT-2", "1"]);
    pt.ok(&["criteria", "rm", "PT-2", "2"]);
    let r = pt.json(&["criteria", "ls", "PT-2"]);
    assert_eq!(r["unchecked"], 1);
    assert_eq!(r["criteria"][0]["text"], "old key revoked");
    assert!(
        !pt.run(&["criteria", "check", "PT-2", "2"]).status.success(),
        "removed"
    );
    assert!(
        !pt.run(&["criteria", "uncheck", "PT-2", "1"])
            .status
            .success(),
        "not checked"
    );
    assert!(!pt.run(&["criteria", "add", "PT-2", " "]).status.success());
    let log = pt.json(&["log", "PT-2"]);
    let kinds: Vec<&str> = log
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event_type"].as_str().unwrap())
        .collect();
    for kind in [
        "task.criterion_added",
        "task.criterion_checked",
        "task.criterion_unchecked",
        "task.criterion_removed",
    ] {
        assert!(kinds.contains(&kind), "{kind}: {kinds:?}");
    }
}

#[test]
fn bulk_done_refuses_the_task_with_open_criteria_and_closes_the_rest() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Harden the drill", "--ac", "CI green"]);
    pt.ok(&["add", "--raw", "Unrelated"]);
    // Bulk, like every closer, reports the refusal per task and moves on.
    let out = pt.run(&["bulk", "search: drill | search: unrelated", "--done"]);
    assert!(!out.status.success());
    assert_eq!(pt.json(&["show", "PT-1"])["status"], "todo");
    assert_eq!(pt.json(&["show", "PT-2"])["status"], "done");
}

fn status(pt: &Pt, id: &str) -> String {
    pt.json(&["show", id])["status"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn undo_right_after_add_with_criteria_removes_that_task() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Older task"]); // PT-1
    pt.ok(&[
        "add",
        "--raw",
        "Ship the restore drill",
        "--ac",
        "drill passes",
    ]); // PT-2

    let out = pt.run(&["undo", "--yes"]);
    assert!(
        out.status.success(),
        "the criteria journaled with the create made undo refuse: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!pt.exists("PT-2"), "undo must remove the task just added");
    assert!(pt.exists("PT-1"), "undo reached past PT-2 to an older task");
}

#[test]
fn undo_after_editing_a_closed_tasks_criteria_reverses_that_close() {
    let pt = Pt::new();
    pt.ok_as("shell", &["add", "--raw", "Draft the quarterly summary"]); // PT-1, yours
    pt.ok_as("hal", &["add", "--raw", "Renew the staging certificate"]); // PT-2, another actor's
    pt.ok_as("shell", &["done", "PT-2"]);
    pt.ok_as(
        "shell",
        &["criteria", "add", "PT-2", "cert", "served", "on", "lhr"],
    );
    pt.ok_as(
        "hal",
        &["criteria", "check", "PT-2", "1", "-m", "curl -v ok"],
    );

    let out = pt.run_as("shell", &["undo", "--yes"]);
    assert!(
        out.status.success(),
        "a criteria edit (yours or another actor's) blocked undo of your close: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        pt.exists("PT-1"),
        "undo --yes reached past the close and deleted PT-1, which it was never about"
    );
    assert_eq!(status(&pt, "PT-1"), "todo");
    assert_ne!(
        status(&pt, "PT-2"),
        "done",
        "undo must reverse the close the criteria edit followed"
    );
}

// A keyed `pt add` journaled before 3.48.0 (no `acceptance` field) must
// still replay: the field at its default is left out of the fingerprint.

fn open_db(pt: &Pt) -> ptask_core::Db {
    ptask_core::Db::open(pt.dir.path().join("tasks.db")).unwrap()
}

/// What pt 3.47 journaled as the fingerprint of a keyed `pt add --raw <title>`.
fn add_before_criteria(title: &str) -> String {
    format!(
        "Add(AddArgs {{ title: {title:?}, priority: None, description: None, deadline: None, \
         reason: None, raw: true, kind: None, deliverable: None }})"
    )
}

#[test]
fn the_add_fingerprint_is_unchanged_without_criteria() {
    use ptask_core::event_log::{self, CommandFingerprint};
    let pt = Pt::new();
    pt.ok(&[
        "--idempotency-key",
        "k-add",
        "add",
        "--raw",
        "Rotate the signing key",
    ]);
    let journaled = event_log::get_by_uuid(&open_db(&pt), "k-add")
        .unwrap()
        .expect("the keyed add is journaled under its key")
        .command;
    assert_eq!(
        journaled,
        Some(CommandFingerprint::new(
            "Add",
            &add_before_criteria("Rotate the signing key")
        )),
        "an empty --ac list must not change the fingerprint of `pt add`"
    );

    // A different command under the same key is still refused.
    let other = pt.run(&[
        "--idempotency-key",
        "k-add",
        "add",
        "--raw",
        "Rotate the signing key",
        "--ac",
        "old key revoked",
    ]);
    assert!(
        !other.status.success(),
        "adding criteria changes the command; the key must not replay it"
    );
}

#[test]
fn a_keyed_add_journaled_before_criteria_replays() {
    use ptask_core::event_log::{CommandFingerprint, EventCtx};
    let pt = Pt::new();
    {
        let db = open_db(&pt);
        let ctx =
            EventCtx::local("test")
                .with_uuid("k-legacy")
                .with_command(CommandFingerprint::new(
                    "Add",
                    &add_before_criteria("Rotate the signing key"),
                ));
        ptask_core::tasks::create(
            &db,
            ptask_core::tasks::NewTask::minimal("Rotate the signing key"),
            &ctx,
        )
        .unwrap();
    }
    let out = pt.run(&[
        "--idempotency-key",
        "k-legacy",
        "add",
        "--raw",
        "Rotate the signing key",
    ]);
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("replayed"),
        "a retry of the same keyed add across the upgrade must replay: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!pt.exists("PT-2"), "the retry filed a second task");
}
