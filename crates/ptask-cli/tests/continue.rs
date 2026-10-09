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
