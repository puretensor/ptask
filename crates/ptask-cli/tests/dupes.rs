//! Black-box tests for duplicate detection and merge: what `pt add` reports
//! and refuses, `pt dupes`, and `pt merge`.

mod common;
use common::Pt;

#[test]
fn add_reports_likely_duplicates_and_unique_refuses_them() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Vendor outreach to north-region partners"]); // PT-1
    let out = pt.json(&["add", "--raw", "North-region partners vendor outreach"]); // PT-2
    let d = out["possible_duplicates"].as_array().unwrap();
    assert_eq!(d.len(), 1, "{out:#}");
    assert_eq!(d[0]["pt_id"], "PT-1");
    assert!(d[0]["score"].as_f64().unwrap() >= 0.6);

    let human = pt.ok(&[
        "--no-color",
        "add",
        "--raw",
        "Vendor outreach (north-region partners)",
    ]);
    assert!(human.contains("duplicate?"), "{human}");
    assert!(human.contains("pt merge PT-3 --into PT-1"), "{human}");

    // An unrelated task carries no field at all.
    let clean = pt.json(&["add", "--raw", "Renew the office lease"]);
    assert!(clean.get("possible_duplicates").is_none(), "{clean:#}");

    // --unique: nothing is created, the candidates are listed, exit 1.
    let refused = pt.run(&[
        "--json",
        "add",
        "--unique",
        "--raw",
        "Outreach to vendors: north-region partners",
    ]);
    assert!(!refused.status.success());
    let body: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(body["created"], false);
    assert!(!body["possible_duplicates"].as_array().unwrap().is_empty());
    assert!(!pt.exists("PT-5"), "--unique must not create");
    pt.ok(&[
        "add",
        "--unique",
        "--raw",
        "Alpha widget certification network prep",
    ]);
    assert!(pt.exists("PT-5"));
    // Related work is reported, not refused: --unique refuses only a
    // near-certain duplicate (0.75), reporting starts at 0.6.
    let related = pt.json(&[
        "add",
        "--unique",
        "--raw",
        "Submit alpha widget certification",
    ]);
    assert_eq!(related["pt_id"], "PT-6");
    assert_eq!(related["possible_duplicates"][0]["pt_id"], "PT-5");
    assert!(related["possible_duplicates"][0]["score"].as_f64().unwrap() < 0.75);
}

#[test]
fn recently_closed_work_counts_as_a_duplicate() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Rotate the service API key"]);
    pt.ok(&["done", "PT-1"]);
    let out = pt.json(&["add", "--raw", "Service API key rotation"]);
    assert_eq!(out["possible_duplicates"][0]["pt_id"], "PT-1");
    assert_eq!(out["possible_duplicates"][0]["status"], "done");
}

#[test]
fn dupes_lists_pairs_and_candidates_for_one_task() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Voice clone trio: narrator track"]); // PT-1
    pt.ok(&["add", "--raw", "Narrator track voice clone"]); // PT-2
    pt.ok(&["add", "--raw", "Unrelated: patch the lab BIOS"]); // PT-3
    let pairs = pt.json(&["dupes"]);
    let pairs = pairs.as_array().unwrap();
    assert_eq!(pairs.len(), 1, "{pairs:#?}");
    assert_eq!(pairs[0]["a"]["pt_id"], "PT-1");
    assert_eq!(pairs[0]["b"]["pt_id"], "PT-2");
    let human = pt.ok(&["--no-color", "dupes"]);
    assert!(human.contains("pt merge PT-2 --into PT-1"), "{human}");

    let one = pt.json(&["dupes", "PT-2"]);
    assert_eq!(one.as_array().unwrap().len(), 1);
    assert_eq!(one[0]["pt_id"], "PT-1");
    assert!(pt.json(&["dupes", "PT-3"]).as_array().unwrap().is_empty());
    assert!(!pt.run(&["dupes", "--threshold", "1.5"]).status.success());
}

#[test]
fn merge_keeps_dependents_blocked_and_carries_the_work_over() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Archive the old build logs"]); // PT-1 canonical
    pt.ok(&[
        "add",
        "--raw",
        "-p",
        "urgent",
        "Old build logs archive @domain:mgmt",
    ]); // PT-2 duplicate
    pt.ok(&["add", "--raw", "Publish the archive report"]); // PT-3 waits on the duplicate
    pt.ok(&["add", "--raw", "Order replacement tapes"]); // PT-4 blocks the duplicate
    pt.ok(&["edit", "PT-2", "--label", "domain:mgmt"]);
    pt.ok(&["depend", "PT-3", "--on", "PT-2"]);
    pt.ok(&["depend", "PT-2", "--on", "PT-4"]);

    let m = pt.json(&["merge", "PT-2", "--into", "PT-1", "-m", "same filing"]);
    assert_eq!(m["duplicate"], "PT-2");
    assert_eq!(m["into"], "PT-1");
    assert_eq!(m["dependents_moved"], serde_json::json!(["PT-3"]));
    assert_eq!(m["prerequisites_added"], serde_json::json!(["PT-4"]));
    assert_eq!(m["labels_added"], serde_json::json!(["domain:mgmt"]));
    assert_eq!(m["priority_raised"], serde_json::json!([2, 4]));

    let dup = pt.json(&["show", "PT-2"]);
    assert_eq!(dup["status"], "dismissed");
    assert_eq!(dup["duplicate_of"], "PT-1");
    let canon = pt.json(&["show", "PT-1"]);
    assert_eq!(canon["merged_in"], serde_json::json!(["PT-2"]));
    assert_eq!(canon["priority"], 4);
    let shown = pt.ok(&["--no-color", "show", "PT-2"]);
    assert!(shown.contains("of PT-1 (merged)"), "{shown}");

    // Dismissing a prerequisite satisfies it; the move is what keeps PT-3
    // waiting, now on PT-1.
    let blocked = pt.run(&["done", "PT-3"]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("PT-1"));
    // The duplicate is gone from `next`; the canonical task waits on PT-4.
    let next: Vec<String> = pt
        .json(&["next"])
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["pt_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(next, ["PT-4"]);

    // Merging a closed task, or into a dismissed one, is refused.
    assert!(
        !pt.run(&["merge", "PT-2", "--into", "PT-1"])
            .status
            .success()
    );
    assert!(
        !pt.run(&["merge", "PT-4", "--into", "PT-2"])
            .status
            .success()
    );
    // A merged-away task is not offered as a duplicate again.
    let again = pt.json(&["add", "--raw", "Old build logs archive, again"]);
    let d: Vec<&str> = again["possible_duplicates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["pt_id"].as_str().unwrap())
        .collect();
    assert_eq!(d, ["PT-1"]);
}

#[test]
fn a_keyed_merge_replays_and_undo_reopens_the_duplicate() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Nightly export job"]);
    pt.ok(&["add", "--raw", "Nightly export job, phase 2"]);
    pt.ok(&[
        "--idempotency-key",
        "m-1",
        "merge",
        "PT-2",
        "--into",
        "PT-1",
    ]);
    let replay = pt.ok(&[
        "--idempotency-key",
        "m-1",
        "merge",
        "PT-2",
        "--into",
        "PT-1",
    ]);
    assert!(replay.contains("replayed"), "{replay}");
    pt.ok(&["undo", "--yes"]);
    assert_eq!(pt.json(&["show", "PT-2"])["status"], "todo");
    assert!(pt.json(&["show", "PT-2"])["duplicate_of"].is_null());
}

fn criteria_texts(pt: &Pt, id: &str) -> Vec<String> {
    pt.json(&["show", id])["criteria"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["text"].as_str().unwrap().to_string())
        .collect()
}

// A merge must not drop part of the definition of done: the duplicate's
// unchecked criteria are still owed by the work, so they move to the
// target (text the target already has is not added twice; checked ones
// already held), and undo takes them back.
#[test]
fn a_merge_carries_unchecked_criteria_and_undo_takes_them_back() {
    let pt = Pt::new();
    pt.ok(&[
        "add",
        "--raw",
        "Nightly export job",
        "--ac",
        "export verified",
    ]);
    pt.ok(&[
        "add",
        "--raw",
        "Nightly export job, phase 2",
        "--ac",
        "Export verified",
        "--ac",
        "row counts match",
        "--ac",
        "alert wired",
    ]);
    pt.ok(&["criteria", "check", "PT-2", "3", "-m", "pager test"]);

    let m = pt.json(&["merge", "PT-2", "--into", "PT-1"]);
    assert_eq!(m["criteria_carried"], serde_json::json!([2]), "{m}");
    assert_eq!(
        criteria_texts(&pt, "PT-1"),
        ["export verified", "row counts match"]
    );
    pt.ok(&["criteria", "check", "PT-1", "1"]);
    let done = pt.run(&["done", "PT-1"]);
    assert!(
        !done.status.success(),
        "the carried criterion gates the close"
    );
    assert!(
        String::from_utf8_lossy(&done.stderr).contains("row counts match"),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );

    pt.ok(&["undo", "--yes"]);
    assert_eq!(pt.json(&["show", "PT-2"])["status"], "todo");
    assert_eq!(criteria_texts(&pt, "PT-1"), ["export verified"]);
    assert_eq!(
        criteria_texts(&pt, "PT-2"),
        ["Export verified", "row counts match", "alert wired"],
        "the duplicate kept its own criteria throughout"
    );
}

#[test]
fn a_merge_into_a_done_task_refuses_unchecked_criteria() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Nightly export job"]);
    pt.ok(&["done", "PT-1"]);
    pt.ok(&[
        "add",
        "--raw",
        "Nightly export job, phase 2",
        "--ac",
        "row counts match",
    ]);
    let r = pt.run(&["merge", "PT-2", "--into", "PT-1"]);
    assert!(!r.status.success());
    let err = String::from_utf8_lossy(&r.stderr);
    assert!(
        err.contains("unchecked acceptance criteria") && err.contains("nothing was merged"),
        "{err}"
    );
    assert_eq!(pt.json(&["show", "PT-2"])["status"], "todo");
    assert!(criteria_texts(&pt, "PT-1").is_empty());

    pt.ok(&["criteria", "check", "PT-2", "1"]);
    pt.ok(&["merge", "PT-2", "--into", "PT-1"]);
    assert_eq!(pt.json(&["show", "PT-2"])["status"], "dismissed");
}

// A keyed `pt add` journaled before 3.45.0 (no `unique` field) must still
// replay: the flag at its default is left out of the fingerprint.

fn open_db(pt: &Pt) -> ptask_core::Db {
    ptask_core::Db::open(pt.dir.path().join("tasks.db")).unwrap()
}

/// What pt 3.44 journaled as the fingerprint of a keyed `pt add --raw <title>`.
fn add_before_unique(title: &str) -> String {
    format!(
        "Add(AddArgs {{ title: {title:?}, priority: None, description: None, deadline: None, \
         reason: None, raw: true, kind: None, deliverable: None }})"
    )
}

#[test]
fn the_add_fingerprint_is_unchanged_without_unique() {
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
            &add_before_unique("Rotate the signing key")
        )),
        "`pt add` without --unique must fingerprint as it did before the flag existed"
    );

    // --unique is a different command: the key must not replay it.
    let other = pt.run(&[
        "--idempotency-key",
        "k-add",
        "add",
        "--raw",
        "--unique",
        "Rotate the signing key",
    ]);
    assert!(
        !other.status.success(),
        "--unique under a used key replayed"
    );
}

#[test]
fn a_keyed_add_journaled_before_unique_replays() {
    use ptask_core::event_log::{CommandFingerprint, EventCtx};
    let pt = Pt::new();
    {
        let db = open_db(&pt);
        let ctx =
            EventCtx::local("test")
                .with_uuid("k-legacy")
                .with_command(CommandFingerprint::new(
                    "Add",
                    &add_before_unique("Rotate the signing key"),
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
