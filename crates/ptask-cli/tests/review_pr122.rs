//! Contract tests from the pre-merge review of PR #122 (closure evidence and
//! task notes). Each test fails on the PR head for the reason its finding
//! describes and passes once that finding is fixed. The cockpit half (finding
//! 6, the drawer's 60-event window) is `dashboard/tests/review_pr122_drawer.test.mjs`.

mod common;
use common::Pt;
use ptask_core::event_log::{self, CommandFingerprint, EventCtx};
use std::io::Write;
use std::process::{Output, Stdio};

fn status(pt: &Pt, id: &str) -> String {
    pt.json(&["show", id])["status"]
        .as_str()
        .unwrap()
        .to_string()
}

fn note_texts(pt: &Pt, id: &str) -> Vec<String> {
    pt.json(&["show", id])["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["text"].as_str().unwrap().to_string())
        .collect()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `pt <args>` as `actor` with `input` on stdin.
fn run_with_stdin(pt: &Pt, actor: &str, args: &[&str], input: &str) -> Output {
    let mut child = pt
        .command(actor, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn open_db(pt: &Pt) -> ptask_core::Db {
    ptask_core::Db::open(pt.dir.path().join("tasks.db")).unwrap()
}

// ---- Finding 1: a note must not make `pt undo` skip the close it follows ----

#[test]
fn undo_after_noting_your_own_close_reverses_that_close() {
    let pt = Pt::new();
    pt.ok_as("shell", &["add", "--raw", "Renew the staging certificate"]); // PT-1
    pt.ok_as("shell", &["add", "--raw", "Rotate the backup key"]); // PT-2
    pt.ok_as("shell", &["done", "PT-1"]);
    pt.ok_as("shell", &["done", "PT-2"]);
    pt.ok_as("shell", &["note", "PT-2", "verified"]);

    let out = pt.run_as("shell", &["undo"]);
    assert!(out.status.success(), "undo failed: {}", stderr(&out));
    assert_ne!(
        status(&pt, "PT-2"),
        "done",
        "undo must reverse the close the note followed (task.noted is transparent to undo)"
    );
    assert_eq!(
        status(&pt, "PT-1"),
        "done",
        "undo reached back past the noted close and reopened an older one"
    );
}

#[test]
fn undo_yes_after_a_note_never_deletes_an_unrelated_untouched_task() {
    let pt = Pt::new();
    pt.ok_as("hal", &["add", "--raw", "Renew the staging certificate"]); // PT-1, filed by another actor
    pt.ok_as("shell", &["add", "--raw", "Draft the quarterly summary"]); // PT-2, yours, untouched
    pt.ok_as("shell", &["done", "PT-1"]);
    pt.ok_as("shell", &["note", "PT-1", "checked by hand"]);

    let out = pt.run_as("shell", &["undo", "--yes"]);
    assert!(
        pt.exists("PT-2"),
        "undo --yes permanently deleted PT-2, which the note-shadowed close was never about \
         (stdout: {})",
        stdout(&out)
    );
    assert_ne!(
        status(&pt, "PT-1"),
        "done",
        "undo must reverse the close the note followed"
    );
}

#[test]
fn a_note_is_transparent_to_undo_and_never_its_target() {
    let pt = Pt::new();
    pt.ok_as("shell", &["add", "--raw", "Patch the build host"]); // PT-1
    pt.ok_as("hal", &["add", "--raw", "Review the runbook"]); // PT-2, another actor's
    pt.ok_as("shell", &["done", "PT-1"]);
    // Another actor's note after your close, then your own note on a task
    // you never changed: neither is a change undo can reverse or must protect.
    pt.ok_as("hal", &["note", "PT-1", "looks right from here"]);
    pt.ok_as("shell", &["note", "PT-2", "seen"]);

    let out = pt.run_as("shell", &["undo"]);
    assert!(
        out.status.success(),
        "a note (yours or another actor's) blocked undo of your close: {}",
        stderr(&out)
    );
    assert_ne!(status(&pt, "PT-1"), "done", "undo must reverse your close");
    assert!(pt.exists("PT-2"), "a note is never an undo target");
    assert_eq!(
        status(&pt, "PT-2"),
        "todo",
        "a note is never an undo target"
    );
}

// ---- Finding 2: a keyed note read from stdin ----

#[test]
fn a_keyed_note_from_stdin_is_never_silently_replayed_with_different_text() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Check the nightly job"]); // PT-1
    let args = ["--idempotency-key", "k-stdin", "note", "PT-1", "-"];
    let _first = run_with_stdin(&pt, "test", &args, "first reading: 12 rows");
    let second = run_with_stdin(&pt, "test", &args, "second reading: 14 rows");

    let texts = note_texts(&pt, "PT-1");
    let swallowed =
        second.status.success() && !texts.iter().any(|t| t == "second reading: 14 rows");
    assert!(
        !swallowed,
        "different stdin under the same key exited 0 and was dropped (the key covers argv \
         only, so the retry read as a replay): stdout={:?} notes={texts:?}",
        stdout(&second)
    );
}

// ---- Finding 3: the optional note field must not change a keyed command's fingerprint ----

/// What pt 3.42.2 (before the optional `note` field) journaled as the
/// fingerprint of a keyed `pt done PT-1` / `pt dismiss PT-1`.
const DONE_BEFORE_NOTES: &str = r#"Done(DoneArgs { queries: ["PT-1"] })"#;
const DISMISS_BEFORE_NOTES: &str = r#"Dismiss(DismissArgs { query: "PT-1" })"#;

#[test]
fn the_done_fingerprint_is_the_same_with_or_without_the_note_field() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Rotate the signing key"]); // PT-1
    pt.ok(&["--idempotency-key", "k-fp", "done", "PT-1"]);
    let journaled = event_log::get_by_uuid(&open_db(&pt), "k-fp")
        .unwrap()
        .expect("the keyed close is journaled under its key")
        .command;
    assert_eq!(
        journaled,
        Some(CommandFingerprint::new("Done", DONE_BEFORE_NOTES)),
        "`pt done PT-1` fingerprints differently now that DoneArgs has an optional note; \
         a None/default field must not change the fingerprint"
    );
}

#[test]
fn a_keyed_done_journaled_before_the_note_field_replays() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Rotate the signing key"]); // PT-1
    {
        // What `pt --idempotency-key k-legacy done PT-1` journaled on 3.42.2.
        let db = open_db(&pt);
        let task = ptask_core::tasks::resolve(&db, "PT-1").unwrap();
        let ctx = EventCtx::local("test")
            .with_uuid("k-legacy")
            .with_command(CommandFingerprint::new("Done", DONE_BEFORE_NOTES));
        ptask_core::tasks::mark_done(&db, &task, &ctx).unwrap();
    }
    let out = pt.run(&["--idempotency-key", "k-legacy", "done", "PT-1"]);
    assert!(
        out.status.success() && stdout(&out).contains("replayed"),
        "a retry of the same keyed close across the upgrade must replay, not fail as a \
         different command: {}",
        stderr(&out)
    );
}

#[test]
fn a_keyed_dismiss_journaled_before_the_note_field_replays() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Retire the old status page"]); // PT-1
    {
        // What `pt --idempotency-key k-legacy-x dismiss PT-1` journaled on 3.42.2.
        let db = open_db(&pt);
        let task = ptask_core::tasks::resolve(&db, "PT-1").unwrap();
        let ctx = EventCtx::local("test")
            .with_uuid("k-legacy-x")
            .with_command(CommandFingerprint::new("Dismiss", DISMISS_BEFORE_NOTES));
        ptask_core::tasks::dismiss(&db, &task.id, &ctx).unwrap();
    }
    let out = pt.run(&["--idempotency-key", "k-legacy-x", "dismiss", "PT-1"]);
    assert!(
        out.status.success() && stdout(&out).contains("replayed"),
        "a retry of the same keyed dismissal across the upgrade must replay: {}",
        stderr(&out)
    );
}

// ---- Finding 5: long notes are truncated in the digest and the worker brief ----

/// About 2,250 characters of closing evidence, ASCII only, so a marker is
/// recognisable.
fn long_note() -> String {
    "step ok; ".repeat(250).trim_end().to_string()
}

/// A truncated rendering of `full`: short, keeps the start, ends in a marker
/// (it is not simply a prefix of the original).
fn assert_truncated_with_marker(shown: &str, full: &str, surface: &str) {
    let n = shown.chars().count();
    assert!(
        n <= 400,
        "{surface} carries the whole {}-character note ({n} characters); it must be cut to \
         about 300 with a marker",
        full.chars().count()
    );
    assert!(
        shown.starts_with(&full[..200]),
        "{surface} must keep the start of the note: {shown:?}"
    );
    assert!(
        !full.starts_with(shown),
        "{surface} cut the note without a truncation marker: {shown:?}"
    );
}

#[test]
fn the_digest_truncates_a_long_closing_note_with_a_marker() {
    let pt = Pt::new();
    let full = long_note();
    pt.ok(&["add", "--raw", "Rebuild the search index"]); // PT-1
    pt.ok(&["done", "PT-1", "--note", &full]);
    let digest = pt.json(&["digest"]);
    let shown = digest["done"][0]["note"]
        .as_str()
        .expect("the digest carries the closing note")
        .to_string();
    assert_truncated_with_marker(&shown, &full, "the digest");
}

#[test]
fn the_worker_brief_truncates_a_long_note_with_a_marker() {
    let pt = Pt::new();
    let full = long_note();
    pt.ok(&["add", "--raw", "Rebuild the search index"]); // PT-1
    pt.ok(&["note", "PT-1", &full]);
    let brief = pt.ok(&["context", "PT-1"]);
    let line = brief
        .lines()
        .find(|l| l.contains("step ok;"))
        .unwrap_or_else(|| panic!("the brief carries the note: {brief}"));
    // `- <when> <who>: <text>`
    let shown = line
        .split_once(": ")
        .map(|(_, text)| text)
        .unwrap_or_else(|| panic!("a note line: {line}"));
    assert_truncated_with_marker(shown, &full, "the worker brief (`pt context`)");
}

// ---- Finding 7: the export carries no notes of hard-deleted tasks ----

#[test]
fn export_writes_no_notes_for_hard_deleted_tasks() {
    let pt = Pt::new();
    pt.ok(&["add", "--raw", "Temporary scratch task"]); // PT-1
    pt.ok(&[
        "note",
        "PT-1",
        "scratch text that was deleted with its task",
    ]);
    pt.ok(&["add", "--raw", "Kept task"]); // PT-2
    pt.ok(&["note", "PT-2", "kept note"]);
    pt.ok(&["rm", "PT-1", "--yes"]);
    assert!(!pt.exists("PT-1"));

    let out = pt.dir.path().join("export");
    pt.ok(&["export", "--out", out.to_str().unwrap()]);
    let notes = std::fs::read_to_string(out.join("task_notes.jsonl")).unwrap();
    assert!(notes.contains("kept note"), "{notes}");
    assert!(
        !notes.contains("scratch text that was deleted with its task"),
        "`pt rm` deleted PT-1 permanently, but its note is still exported: {notes}"
    );
}
