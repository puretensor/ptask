//! Black-box tests for close-and-continue: `pt done` names what the close
//! unblocked, and `--claim-next` claims the next ready task.

mod common;
use common::Pt;

#[test]
fn done_names_the_tasks_it_unblocked() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Provision the VLAN"]); // PT-1
    pt.ok(&["add", "--raw", "Move the cameras onto it"]); // PT-2 waits on PT-1
    pt.ok(&["add", "--raw", "Unrelated"]); // PT-3
    pt.ok(&["depend", "PT-2", "--on", "PT-1"]);
    let human = pt.ok(&["--no-color", "done", "PT-1"]);
    assert!(
        human.contains("unblocked") && human.contains("PT-2"),
        "{human}"
    );
    assert!(!human.contains("PT-3"), "{human}");
    let r = pt.json(&["done", "PT-3"]);
    assert!(r[0]["unblocked"].as_array().unwrap().is_empty());
}

#[test]
fn claim_next_closes_and_continues_in_one_command() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "-p", "low", "Low priority chore"]); // PT-1
    pt.ok(&["add", "--raw", "Provision the VLAN"]); // PT-2
    pt.ok(&["add", "--raw", "-p", "urgent", "Move the cameras onto it"]); // PT-3
    pt.ok(&["depend", "PT-3", "--on", "PT-2"]);

    let r = pt.json(&["done", "PT-2", "--claim-next"]);
    assert_eq!(r["results"][0]["unblocked"][0]["pt_id"], "PT-3");
    assert_eq!(
        r["claimed_next"]["pt_id"], "PT-3",
        "the urgent task the close freed"
    );
    assert_eq!(pt.json(&["show", "PT-3"])["status"], "in_progress");
    let log = pt.ok(&["--no-color", "log", "PT-3"]);
    assert!(log.contains("task.claimed"), "{log}");

    // A keyed close-and-continue: the claim journals under a derived key.
    pt.ok(&["add", "--raw", "Another chore"]); // PT-4
    let human = pt.ok_as(
        "hal",
        &["--idempotency-key", "cc-1", "done", "PT-3", "--claim-next"],
    );
    assert!(human.contains("claimed"), "{human}");
    let claimed: Vec<String> = ["PT-1", "PT-4"]
        .iter()
        .filter(|id| pt.json(&["show", id])["status"] == "in_progress")
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        claimed.len(),
        1,
        "exactly one next task claimed: {claimed:?}"
    );

    // A failed close claims nothing.
    let out = pt.run(&["done", "PT-99", "--claim-next"]);
    assert!(!out.status.success());
    let open_todo = pt
        .json(&["list"])
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["status"] == "todo")
        .count();
    assert_eq!(open_todo, 1, "the remaining chore stays unclaimed");
}

#[test]
fn nothing_ready_says_so() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Only task"]);
    let human = pt.ok(&["--no-color", "done", "PT-1", "--claim-next"]);
    assert!(human.contains("nothing ready to claim next"), "{human}");
}

fn in_progress(pt: &Pt) -> Vec<String> {
    pt.json(&["list"])
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["status"] == "in_progress")
        .map(|t| t["pt_id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_keyed_retry_reports_the_task_it_claimed_and_claims_no_other() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Provision the VLAN"]); // PT-1
    pt.ok(&["add", "--raw", "Move the cameras"]); // PT-2
    pt.ok(&["add", "--raw", "Label the patch panel"]); // PT-3
    let args = ["--idempotency-key", "cc-r", "done", "PT-1", "--claim-next"];
    let first = pt.json(&args);
    let claimed = first["claimed_next"]["pt_id"].as_str().unwrap().to_string();

    // The response was lost; the agent retries the same keyed command.
    let retry = pt.json(&args);
    assert_eq!(retry["results"][0]["outcome"], "replayed");
    assert_eq!(
        retry["claimed_next"]["pt_id"], claimed,
        "the retry must name the task the first run claimed"
    );
    assert_eq!(
        in_progress(&pt),
        vec![claimed],
        "the retry claimed a second task"
    );

    let human = pt.ok_as("test", &args);
    assert!(
        human.contains("replayed") && human.contains("claimed"),
        "{human}"
    );
}

#[test]
fn a_keyed_multi_task_close_and_continue_retries_cleanly() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Patch fox-n0"]); // PT-1
    pt.ok(&["add", "--raw", "Patch fox-n1"]); // PT-2
    pt.ok(&["add", "--raw", "Reboot the pair"]); // PT-3
    pt.ok(&["add", "--raw", "Write it up"]); // PT-4
    let args = [
        "--idempotency-key",
        "cc-m",
        "done",
        "PT-1",
        "PT-2",
        "--claim-next",
    ];
    let first = pt.json(&args);
    let claimed = first["claimed_next"]["pt_id"].as_str().unwrap().to_string();

    let retry = pt.run(&[
        "--json",
        "--idempotency-key",
        "cc-m",
        "done",
        "PT-1",
        "PT-2",
        "--claim-next",
    ]);
    assert!(
        retry.status.success(),
        "a retry of a keyed multi-task close-and-continue failed: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    let retry: serde_json::Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(retry["claimed_next"]["pt_id"], claimed);
    assert_eq!(in_progress(&pt), vec![claimed]);
}

// A keyed `pt done` journaled before 3.47.0 (no `claim_next` field) must
// still replay: the flag at its default is left out of the fingerprint.

const DONE_BEFORE_CLAIM_NEXT: &str = r#"Done(DoneArgs { queries: ["PT-1"] })"#;

fn open_db(pt: &Pt) -> ptask_core::Db {
    ptask_core::Db::open(pt.dir.path().join("tasks.db")).unwrap()
}

#[test]
fn the_done_fingerprint_is_unchanged_without_claim_next() {
    use ptask_core::event_log::{self, CommandFingerprint};
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Rotate the signing key"]); // PT-1
    pt.ok(&["--idempotency-key", "k-fp", "done", "PT-1"]);
    let journaled = event_log::get_by_uuid(&open_db(&pt), "k-fp")
        .unwrap()
        .expect("the keyed close is journaled under its key")
        .command;
    assert_eq!(
        journaled,
        Some(CommandFingerprint::new("Done", DONE_BEFORE_CLAIM_NEXT)),
        "`pt done` without --claim-next must fingerprint as it did before the flag existed"
    );
}

#[test]
fn a_keyed_done_journaled_before_claim_next_replays() {
    use ptask_core::event_log::{CommandFingerprint, EventCtx};
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Rotate the signing key"]); // PT-1
    {
        let db = open_db(&pt);
        let task = ptask_core::tasks::resolve(&db, "PT-1").unwrap();
        let ctx = EventCtx::local("test")
            .with_uuid("k-legacy")
            .with_command(CommandFingerprint::new("Done", DONE_BEFORE_CLAIM_NEXT));
        ptask_core::tasks::mark_done(&db, &task, &ctx).unwrap();
    }
    let out = pt.run(&["--idempotency-key", "k-legacy", "done", "PT-1"]);
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("replayed"),
        "a retry of the same keyed close across the upgrade must replay: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_noted_done_fingerprints_as_it_did_before_claim_next() {
    use ptask_core::event_log::{self, CommandFingerprint};
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Rotate the signing key"]); // PT-1
    pt.ok(&[
        "--idempotency-key",
        "k-note",
        "done",
        "PT-1",
        "-m",
        "verified",
    ]);
    let journaled = event_log::get_by_uuid(&open_db(&pt), "k-note")
        .unwrap()
        .expect("the keyed close is journaled under its key")
        .command;
    assert_eq!(
        journaled,
        Some(CommandFingerprint::new(
            "Done",
            r#"Done(DoneArgs { queries: ["PT-1"], note: Some("verified") })"#
        )),
        "a keyed `pt done -m` must fingerprint as 3.43 did, with claim_next left out"
    );
}
