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
