mod common;
use common::Pt;

#[test]
fn undo_merge_preserves_later_priority_edits() {
    // With no later edit, undo still restores the original priority.
    // Both lowering and raising the priority after the merge must survive.
    for (edit, expected) in [(None, 2), (Some("low"), 1), (Some("critical"), 5)] {
        let pt = Pt::new();
        pt.ok(&["add", "--raw", "Check the nightly export"]); // PT-1
        pt.ok(&["add", "--raw", "-p", "urgent", "Nightly export check"]); // PT-2
        pt.ok(&["merge", "PT-2", "--into", "PT-1"]);
        assert_eq!(pt.json(&["show", "PT-1"])["priority"], 4);

        if let Some(priority) = edit {
            pt.ok(&["priority", "PT-1", priority]);
            assert_eq!(pt.json(&["show", "PT-1"])["priority"], expected);
        }
        pt.ok(&["undo", "--yes"]);
        assert_eq!(pt.json(&["show", "PT-2"])["status"], "todo");
        assert_eq!(
            pt.json(&["show", "PT-1"])["priority"],
            expected,
            "undo must preserve the later {edit:?} edit instead of restoring stale priority"
        );
    }
}
