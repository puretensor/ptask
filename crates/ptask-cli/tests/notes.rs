//! Black-box tests for task notes and closure evidence: `pt note`, `--note`
//! on the closing verbs, and where the trail shows up (show, context, log,
//! digest, export, remote).

mod common;
use common::Pt;
use std::io::Write;
use std::process::Stdio;

fn notes(pt: &Pt, id: &str) -> Vec<serde_json::Value> {
    pt.json(&["show", id])["notes"].as_array().unwrap().clone()
}

#[test]
fn the_trail_is_attributed_and_reaches_show_context_and_log() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Roll the ceph OSDs"]); // PT-1
    pt.ok_as(
        "hal",
        &["note", "PT-1", "osd.3", "rolled;", "osd.4", "next"],
    );
    pt.ok_as(
        "shell",
        &["done", "PT-1", "-m", "all 24 OSDs up: ceph -s HEALTH_OK"],
    );

    let n = notes(&pt, "PT-1");
    assert_eq!(n.len(), 2, "{n:#?}");
    assert_eq!(n[0]["text"], "osd.3 rolled; osd.4 next");
    assert_eq!(n[0]["actor"], "hal");
    assert_eq!(n[0]["kind"], "note");
    assert_eq!(n[1]["text"], "all 24 OSDs up: ceph -s HEALTH_OK");
    assert_eq!(n[1]["actor"], "shell");
    assert_eq!(n[1]["kind"], "done");

    let shown = pt.ok(&["--no-color", "show", "PT-1"]);
    assert!(shown.contains("NOTES   2 · oldest first"), "{shown}");
    assert!(shown.contains("HEALTH_OK"), "{shown}");

    let brief = pt.ok(&["context", "PT-1"]);
    assert!(brief.contains("## Notes"), "{brief}");
    assert!(brief.contains("hal: osd.3 rolled; osd.4 next"), "{brief}");
    assert!(brief.contains("shell (done): all 24 OSDs up"), "{brief}");

    let log = pt.ok(&["--no-color", "log", "PT-1"]);
    assert!(log.contains("task.noted"), "{log}");
    assert!(log.contains("osd.3 rolled"), "{log}");

    // Evidence that arrives after the close: a done task by PT-N.
    pt.ok(&["note", "PT-1", "follow-up: scrub clean"]);
    assert_eq!(notes(&pt, "PT-1").len(), 3);
    // A substring reaches open tasks only, as for every mutating verb.
    assert!(!pt.run(&["note", "ceph", "x"]).status.success());
}

#[test]
fn a_blank_closing_note_refuses_the_close() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Renew TLS cert"]);
    let out = pt.run(&["done", "PT-1", "--note", "   "]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("note is empty"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(pt.json(&["show", "PT-1"])["status"], "todo");
    assert!(!pt.run(&["note", "PT-1", " "]).status.success());
    assert!(notes(&pt, "PT-1").is_empty());
}

#[test]
fn a_lone_dash_reads_the_note_from_stdin() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Fix the backup drill"]);
    let mut child = pt
        .command("hal", &["note", "PT-1", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"restic check: no errors\nsnapshots: 31\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let n = notes(&pt, "PT-1");
    assert_eq!(n[0]["text"], "restic check: no errors\nsnapshots: 31");
}

#[test]
fn dismiss_and_bulk_carry_their_reasons() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "LinkedIn outreach"]); // PT-1
    pt.ok(&["add", "--raw", "LinkedIn outreach again"]); // PT-2
    pt.ok(&["add", "--raw", "batch alpha"]); // PT-3
    pt.ok(&["add", "--raw", "batch beta"]); // PT-4
    pt.ok(&["dismiss", "PT-2", "-m", "duplicate of PT-1"]);
    pt.ok(&[
        "bulk",
        "search: batch",
        "--done",
        "-m",
        "closed by the batch job",
    ]);
    let n = notes(&pt, "PT-2");
    assert_eq!(
        (n[0]["kind"].as_str(), n[0]["text"].as_str()),
        (Some("dismissed"), Some("duplicate of PT-1"))
    );
    for id in ["PT-3", "PT-4"] {
        let n = notes(&pt, id);
        assert_eq!(n[0]["text"], "closed by the batch job", "{id}");
    }
    // --note makes no sense with --set-priority.
    assert!(
        !pt.run(&["bulk", "search: x", "--set-priority", "high", "-m", "x"])
            .status
            .success()
    );
}

#[test]
fn a_keyed_note_replays_once_and_refuses_a_reused_key() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Ship pureMEND antibody"]);
    pt.ok(&["--idempotency-key", "k-note-1", "note", "PT-1", "first"]);
    let replay = pt.ok(&["--idempotency-key", "k-note-1", "note", "PT-1", "first"]);
    assert!(replay.contains("replayed"), "{replay}");
    assert_eq!(notes(&pt, "PT-1").len(), 1);
    let reused = pt.run(&["--idempotency-key", "k-note-1", "note", "PT-1", "other"]);
    assert!(!reused.status.success(), "a key reused for other text");
    assert_eq!(notes(&pt, "PT-1").len(), 1);
}

#[test]
fn digest_and_export_keep_the_closing_evidence() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Rotate the Bedrock key"]); // PT-1
    pt.ok(&["add", "--raw", "Old idea"]); // PT-2
    pt.ok(&["done", "PT-1", "-m", "rotated; old key revoked 14:02"]);
    pt.ok(&["dismiss", "PT-2"]);

    let digest = pt.json(&["digest"]);
    let done = digest["done"].as_array().unwrap();
    assert_eq!(done[0]["note"], "rotated; old key revoked 14:02");
    assert!(digest["dismissed"][0].get("note").is_none());

    let out = pt.dir.path().join("export");
    pt.ok(&["export", "--out", out.to_str().unwrap()]);
    let lines = std::fs::read_to_string(out.join("task_notes.jsonl")).unwrap();
    let rows: Vec<serde_json::Value> = lines
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows.len(), 1, "{lines}");
    assert_eq!(rows[0]["note"], "rotated; old key revoked 14:02");
    assert_eq!(rows[0]["event_type"], "task.completed");
}

#[test]
fn remote_notes_round_trip_through_pt_serve() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Patch fox-n1 firmware"]); // PT-1
    pt.ok(&["add", "--raw", "Retire GCP project"]); // PT-2
    let srv = pt.serve();
    let url = srv.url.as_str();

    pt.ok_as(
        "hal",
        &["remote", "note", "PT-1", "BMC", "at", "2.14", "--url", url],
    );
    pt.ok_as(
        "hal",
        &[
            "remote",
            "done",
            "PT-1",
            "-m",
            "BIOS 2.15 verified",
            "--url",
            url,
        ],
    );
    pt.ok_as(
        "hal",
        &[
            "remote",
            "dismiss",
            "PT-2",
            "--note",
            "retired in August",
            "--url",
            url,
        ],
    );
    // A blank note is refused before the request is sent.
    assert!(
        !pt.run(&["remote", "note", "PT-1", "  ", "--url", url])
            .status
            .success()
    );

    let n = notes(&pt, "PT-1");
    let got: Vec<(&str, &str)> = n
        .iter()
        .map(|x| (x["kind"].as_str().unwrap(), x["text"].as_str().unwrap()))
        .collect();
    assert_eq!(
        got,
        [("note", "BMC at 2.14"), ("done", "BIOS 2.15 verified")]
    );
    // Over /sync the actor is the authenticated client, the surface is sync.
    assert_eq!(n[0]["source"], "sync");
    assert_eq!(notes(&pt, "PT-2")[0]["text"], "retired in August");

    // `pt remote show` reads the trail from /detail.
    let shown = pt.json(&["remote", "show", "PT-1", "--url", url]);
    assert_eq!(shown["detail"]["notes"].as_array().unwrap().len(), 2);
}
