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

/// Two incident tasks idle past the reaper's TTL; dismissing the ones at
/// the `stuck` indices is made to fail inside SQLite.
fn seed_reap_failure(pt: &Pt, stuck: &[usize]) {
    let db = ptask_core::Db::open(pt.dir.path().join("tasks.db")).unwrap();
    let ctx = ptask_core::event_log::EventCtx::test();
    let mut ids = Vec::new();
    for title in ["stale incident ok", "stale incident stuck"] {
        let new = ptask_core::NewTask {
            title: title.into(),
            description: String::new(),
            priority: 3,
            deadline: None,
            source_type: "incident".into(),
            ai_confidence: 1.0,
            ai_reasoning: String::new(),
        };
        let t = ptask_core::tasks::create_with_extensions(
            &db,
            new,
            ptask_core::Extensions::default(),
            &ctx,
        )
        .unwrap();
        ids.push(t.id);
    }
    db.with_conn(|c| {
        c.execute(
            "UPDATE tasks SET updated_at = strftime('%Y-%m-%dT%H:%M:%f','now','-10 days') || '+00:00'",
            [],
        )?;
        let stuck_ids: Vec<String> = stuck.iter().map(|&i| format!("'{}'", ids[i])).collect();
        c.execute_batch(&format!(
            "CREATE TRIGGER test_stuck BEFORE UPDATE ON tasks
             WHEN OLD.id IN ({}) AND NEW.status = 'dismissed'
             BEGIN SELECT RAISE(ABORT, 'dismiss refused by test'); END;",
            stuck_ids.join(",")
        ))?;
        Ok(())
    })
    .unwrap();
}

/// An outcome row `<glyph> <verb> <id> ...` (the verb column is padded).
fn has_row(stdout: &str, verb: &str, id: &str) -> bool {
    stdout.lines().any(|l| {
        let words: Vec<&str> = l.split_whitespace().collect();
        words.windows(2).any(|w| w[0] == verb && w[1] == id)
    })
}

#[test]
fn reap_exits_non_zero_when_a_dismiss_fails() {
    let pt = Pt::new();
    seed_reap_failure(&pt, &[1]);
    let out = pt.run(&["--no-color", "reap"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "the OnFailure alert must fire:\n{stdout}"
    );
    assert!(stdout.contains("dismissed PT-1"), "{stdout}");
    assert!(
        !stdout.contains("dismissed PT-2"),
        "PT-2 was not dismissed:\n{stdout}"
    );
    assert!(has_row(&stdout, "failed", "PT-2"), "{stdout}");
    assert!(stdout.contains("1 of 2"), "{stdout}");
    assert_ne!(pt.json(&["show", "PT-2"])["status"], "dismissed");
    // --json still prints the report, and still fails the unit.
    let out = pt.run(&["reap", "--json"]);
    assert!(!out.status.success());
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["errors"], 1, "{report:#}");
    assert_eq!(report["failed"][0]["pt_id"], "PT-2", "{report:#}");
    assert!(
        report["failed"][0]["error"]
            .as_str()
            .unwrap()
            .contains("refused")
    );
}

/// Core only lists successful dismisses in `reaped`, so when every dismiss
/// failed the CLI printed "REAP OK nothing stale" and exited 0.
#[test]
fn reap_fails_when_every_dismiss_fails() {
    let pt = Pt::new();
    seed_reap_failure(&pt, &[0, 1]);
    let out = pt.run(&["--no-color", "reap"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stdout}{stderr}");
    assert!(!stdout.contains("nothing stale"), "{stdout}");
    assert!(stdout.contains("REAP FAILED"), "{stdout}");
    assert!(
        has_row(&stdout, "failed", "PT-1") && has_row(&stdout, "failed", "PT-2"),
        "{stdout}"
    );
    assert!(stderr.contains("2 of 2"), "{stderr}");
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

/// serde_json escapes C0 only; DEL, C1 and bidi/invisible characters used
/// to reach the terminal raw from every JSON printer (`pt digest` is
/// JSON-only, so even a plain run leaked them).
#[test]
fn json_output_escapes_del_c1_bidi_and_invisible_characters() {
    let pt = Pt::new();
    let title = "pay del\u{7f} csi\u{9b}31m rlo\u{202e} zw\u{200b} tag\u{e0041} ls\u{2028}";
    pt.ok(&["add", "--raw", title, "--deadline", "2020-01-01"]);
    pt.ok(&["goal", "add", title]);
    let raw_hazard = |c: char| {
        ('\u{7f}'..='\u{9f}').contains(&c)
            || matches!(
                c,
                '\u{202A}'..='\u{202E}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{200B}'..='\u{200F}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{E0000}'..='\u{E007F}'
            )
    };
    for (args, carries_title) in [
        (vec!["digest"], true),
        (vec!["--json", "list"], true),
        (vec!["--json", "show", "PT-1"], true),
        (vec!["--json", "search", "pay"], true),
        (vec!["--json", "goal", "ls"], false),
        (vec!["--json", "context", "PT-1"], true),
        (vec!["--json", "log", "PT-1"], false),
    ] {
        let out = pt.run(&args);
        assert!(out.status.success(), "{args:?}");
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(
            !stdout.chars().any(raw_hazard),
            "{args:?} printed a raw hazard:\n{stdout:?}"
        );
        // Still valid JSON, and the value is the exact title.
        let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        if carries_title {
            assert!(v.to_string().contains(title), "{args:?}: {v}");
        }
    }
}
