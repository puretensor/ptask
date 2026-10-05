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

#[test]
fn json_flag_is_honoured_by_bulk_review_view_delegate() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "alpha chore"]); // PT-1
    pt.ok(&["add", "--raw", "beta chore"]); // PT-2

    let dry = pt.json(&[
        "bulk",
        "search: chore",
        "--set-priority",
        "high",
        "--dry-run",
    ]);
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["matched"].as_array().unwrap().len(), 2, "{dry:#}");
    let applied = pt.json(&["bulk", "search: alpha", "--dismiss"]);
    assert_eq!(applied["matched"][0]["pt_id"], "PT-1", "{applied:#}");
    assert!(applied["failures"].as_array().unwrap().is_empty());
    let none = pt.json(&["bulk", "search: nothing-matches", "--done"]);
    assert!(none["matched"].as_array().unwrap().is_empty());

    let stale = pt.json(&["review", "--stale-days", "0"]);
    assert_eq!(stale[0]["pt_id"], "PT-2", "{stale:#}");

    assert_eq!(
        pt.json(&["view", "save", "chores", "search: chore"])["name"],
        "chores"
    );
    assert_eq!(pt.json(&["view", "list"])[0]["name"], "chores");
    assert_eq!(pt.json(&["view", "rm", "chores"])["removed"], true);

    let delegate = pt.json(&["delegate", "PT-2"]);
    assert_eq!(delegate["pt_id"], "PT-2");
    assert!(
        delegate["command"]
            .as_str()
            .unwrap()
            .starts_with("claude -p '")
    );
}

#[test]
fn json_flag_is_honoured_by_remote_verbs() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Blocker task"]); // PT-1
    let srv = pt.serve();
    let url = srv.url.as_str();
    let remote = |args: &[&str]| {
        let mut full = vec!["remote"];
        full.extend_from_slice(args);
        full.extend_from_slice(&["--url", url]);
        pt.json(&full)
    };

    let added = remote(&["add", "Remote chore p4"]);
    assert_eq!(added["pt_id"], "PT-2", "{added:#}");
    assert_eq!(remote(&["list"]).as_array().unwrap().len(), 2);
    assert_eq!(remote(&["show", "PT-2"])["title"], added["title"]);
    assert_eq!(remote(&["priority", "PT-2", "critical"])["priority"], 5);
    let edited = remote(&["edit", "PT-2", "--deadline", "2031-01-01"]);
    assert_eq!(edited["deadline"], "2031-01-01", "{edited:#}");
    assert_eq!(remote(&["start", "PT-2"])["pt_id"], "PT-2");
    assert_eq!(remote(&["depend", "PT-2", "--on", "PT-1"])["on"], "PT-1");
    assert_eq!(
        remote(&["depend", "PT-2", "--on", "PT-1", "--clear"])["pt_id"],
        "PT-2"
    );
    assert_eq!(remote(&["next"]).as_array().unwrap().len(), 2);
    assert_eq!(remote(&["done", "PT-2"])["pt_id"], "PT-2");
    assert_eq!(remote(&["reopen", "PT-2"])["pt_id"], "PT-2");
    assert_eq!(remote(&["snooze", "PT-2", "2031-02-01"])["pt_id"], "PT-2");
    assert_eq!(remote(&["dismiss", "PT-2"])["status"], "dismissed");
    assert_eq!(remote(&["rm", "PT-2", "--yes"])["deleted"], true);
    assert_eq!(remote(&["version"])["in_sync"], true);
}
