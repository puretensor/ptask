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
