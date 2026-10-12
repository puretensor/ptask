//! Undoing a merge must put back exactly what the merge changed. A dependent
//! that already waited on the canonical task keeps that prerequisite.

mod common;
use common::Pt;

#[test]
fn undo_merge_keeps_preexisting_dependency_on_canonical() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Archive the old build logs"]); // PT-1 canonical
    pt.ok(&["add", "--raw", "Old build logs archive"]); // PT-2 duplicate
    pt.ok(&["add", "--raw", "Publish the archive report"]); // PT-3 waits on both
    pt.ok(&["depend", "PT-3", "--on", "PT-1"]);
    pt.ok(&["depend", "PT-3", "--on", "PT-2"]);

    pt.ok(&["merge", "PT-2", "--into", "PT-1"]);
    pt.ok(&["undo", "--yes"]);

    // The duplicate is back, and PT-3 waits on it again...
    assert_eq!(pt.json(&["show", "PT-2"])["status"], "todo");
    // ...but the PT-3 -> PT-1 edge predates the merge and must survive.
    // Close the duplicate so only the canonical prerequisite can block PT-3.
    pt.ok(&["done", "PT-2"]);
    let blocked = pt.run(&["done", "PT-3"]);
    assert!(
        !blocked.status.success(),
        "PT-3 closed although its pre-existing prerequisite PT-1 is still open"
    );
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("PT-1"),
        "{}",
        String::from_utf8_lossy(&blocked.stderr)
    );
}
