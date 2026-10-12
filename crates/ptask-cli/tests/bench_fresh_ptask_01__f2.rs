//! Undoing a merge must not erase a goal the operator linked to the canonical
//! task after the merge (the merge only copied the duplicate's goal, and undo
//! should take back only what the merge itself set).

mod common;
use common::Pt;
use serde_json::Value;

fn goal_ids(pt: &Pt, id: &str) -> Vec<String> {
    let shown: Value = pt.json(&["show", id]);
    shown["goal_chain"]
        .as_array()
        .map(|c| {
            c.iter()
                .map(|g| g["id"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn undo_merge_keeps_a_goal_linked_to_the_canonical_task_after_the_merge() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Check the nightly export job"]); // PT-1 (canon)
    pt.ok(&["add", "--raw", "Nightly export job check"]); // PT-2 (dup)
    pt.ok(&["goal", "add", "Keep exports verifiable"]); // G-1
    pt.ok(&["goal", "add", "Cut export cost"]); // G-2
    pt.ok(&["goal", "link", "PT-2", "G-1"]);
    pt.ok(&["merge", "PT-2", "--into", "PT-1"]);
    assert_eq!(goal_ids(&pt, "PT-1"), ["G-1"], "merge should copy G-1");

    // The operator re-files the canonical task under a different goal.
    pt.ok(&["goal", "link", "PT-1", "G-2"]);
    assert_eq!(goal_ids(&pt, "PT-1"), ["G-2"]);

    pt.ok(&["undo", "--yes"]);

    assert_eq!(
        pt.json(&["show", "PT-2"])["status"],
        "todo",
        "undo should have reopened the duplicate (the merge was undone)"
    );
    assert_eq!(
        goal_ids(&pt, "PT-1"),
        ["G-2"],
        "undoing the merge wiped the goal G-2 linked to PT-1 after the merge"
    );
}
