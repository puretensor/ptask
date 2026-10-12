//! Undoing a merge must put back exactly what the merge changed. When the
//! canonical task waited on its own duplicate, merging drops that edge, and
//! undo must restore it so the reopened duplicate still blocks the canonical.

mod common;
use common::Pt;

#[test]
fn undo_merge_restores_canonical_dependency_on_duplicate() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Archive the old build logs"]); // PT-1 canonical
    pt.ok(&["add", "--raw", "Old build logs archive"]); // PT-2 duplicate
    pt.ok(&["depend", "PT-1", "--on", "PT-2"]);

    pt.ok(&["merge", "PT-2", "--into", "PT-1"]);
    pt.ok(&["undo", "--yes"]);

    // The duplicate is open again...
    assert_eq!(pt.json(&["show", "PT-2"])["status"], "todo");
    // ...and the canonical task must still be blocked by it.
    let blocked = pt.run(&["done", "PT-1"]);
    assert!(
        !blocked.status.success(),
        "PT-1 closed although its prerequisite PT-2 was reopened by undo"
    );
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("PT-2"),
        "{}",
        String::from_utf8_lossy(&blocked.stderr)
    );
}
