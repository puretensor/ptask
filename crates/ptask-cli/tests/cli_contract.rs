//! Black-box contract tests for review findings on the `pt` CLI: each drives
//! the built binary against a throwaway database.

mod common;
use common::Pt;

fn titles(rows: &serde_json::Value) -> Vec<String> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|r| r["title"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn saved_view_shows_open_tasks_by_default() {
    let pt = Pt::new();
    for i in 0..22 {
        pt.ok(&["add", "--raw", &format!("done task {i:02}")]);
    }
    pt.ok(&["add", "--raw", "dismissed task"]); // PT-23
    pt.ok(&["add", "--raw", "open task"]); // PT-24
    let done: Vec<String> = (1..=22).map(|i| format!("PT-{i}")).collect();
    let mut args = vec!["done"];
    args.extend(done.iter().map(String::as_str));
    pt.ok(&args);
    pt.ok(&["dismiss", "PT-23"]);
    pt.ok(&["view", "save", "everything", "search: task"]);

    let rows = pt.json(&["view", "show", "everything"]);
    assert_eq!(titles(&rows), ["open task"], "{rows:#}");
    // `-s all` lifts the default, exactly like `pt list`.
    let all = pt.json(&["view", "show", "everything", "-s", "all", "-n", "50"]);
    assert_eq!(titles(&all).len(), 24);
}

#[test]
fn remote_rm_confirms_and_matches_active_tasks_only() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Quarterly VAT return"]); // PT-1
    pt.ok(&["done", "PT-1"]);
    pt.ok(&["add", "--raw", "quarterly board pack"]); // PT-2 (open)
    pt.ok(&["add", "--raw", "Renew TLS cert"]); // PT-3 (open)
    let srv = pt.serve();
    let url = srv.url.as_str();

    // The reviewer's run: no TTY, no --yes. Nothing may be deleted.
    let out = pt.run(&["remote", "rm", "quarterly", "--url", url]);
    assert!(
        !out.status.success(),
        "must refuse without --yes on a non-TTY"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--yes"), "{stderr}");
    assert!(pt.exists("PT-1") && pt.exists("PT-2"));
    // --json is a machine caller: no prompt possible, so it refuses too.
    let out = pt.run(&["--json", "remote", "rm", "PT-3", "--url", url]);
    assert!(!out.status.success());
    assert!(pt.exists("PT-3"));

    // A substring only reaches active tasks: the done VAT return is out of
    // reach, the open board pack is the single match.
    let out = pt.run(&["remote", "rm", "vat", "--yes", "--url", url]);
    assert!(
        !out.status.success(),
        "a substring must not match a done task"
    );
    assert!(pt.exists("PT-1"));
    pt.ok(&["remote", "rm", "quarterly", "--yes", "--url", url]);
    assert!(!pt.exists("PT-2") && pt.exists("PT-1"));

    // An exact PT-N still addresses a terminal task.
    pt.ok(&["remote", "rm", "PT-1", "-y", "--url", url]);
    assert!(!pt.exists("PT-1"));
}
