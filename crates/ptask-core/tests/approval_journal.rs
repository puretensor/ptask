//! Journal-verified consume (feature flag PTASK_APPROVAL_JOURNAL).
//! Hermetic: temp DB, temp journal, temp key. Env is process-global, so
//! every test holds ENV for its whole run.
use ptask_core::approvals::{
    self, ApprovalError, DecidedVia, Decision, PayloadSource, RequestInput,
};
use ptask_core::event_log::EventCtx;
use ptask_core::{Db, Error};
use std::sync::Mutex;

static ENV: Mutex<()> = Mutex::new(());

struct Setup {
    dir: tempfile::TempDir,
    db: Db,
    journal: std::path::PathBuf,
}

impl Setup {
    fn db_path(&self) -> std::path::PathBuf {
        self.dir.path().join("t.db")
    }
}

fn setup(mode: Option<&str>) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key");
    std::fs::write(&key, "0123456789abcdef0123456789abcdef\n").unwrap();
    let journal = dir.path().join("journal.jsonl");
    // SAFETY: serialised by ENV; no other thread reads env concurrently.
    unsafe {
        match mode {
            Some(m) => {
                std::env::set_var("PTASK_APPROVAL_JOURNAL", m);
                std::env::set_var("PTASK_APPROVAL_JOURNAL_PATH", &journal);
                std::env::set_var("PTASK_APPROVAL_JOURNAL_KEY_FILE", &key);
            }
            None => std::env::remove_var("PTASK_APPROVAL_JOURNAL"),
        }
    }
    let db = Db::open(dir.path().join("t.db")).unwrap();
    Setup { dir, db, journal }
}

fn digest() -> String {
    "a".repeat(64)
}

fn request(db: &Db, expires_in: Option<&str>) -> approvals::Approval {
    approvals::request(
        db,
        RequestInput {
            kind: "destroy".into(),
            title: "Wipe the thing".into(),
            request_note: None,
            payload: PayloadSource::Digest(digest()),
            task_pt_id: None,
            expires_in: expires_in.map(str::to_string),
        },
        &EventCtx::local("requester"),
    )
    .unwrap()
    .approval
}

fn approve(db: &Db, id: &str) {
    approvals::decide(
        db,
        id,
        Decision::Approve,
        DecidedVia::Api,
        None,
        &EventCtx::local("operator"),
    )
    .unwrap();
}

fn consume(db: &Db, id: &str, d: &str) -> ptask_core::Result<approvals::Approval> {
    approvals::consume(
        db,
        id,
        &PayloadSource::Digest(d.to_string()),
        &EventCtx::local("executor"),
    )
}

/// Write an `approved` row straight into SQLite, as a seat with write
/// access to the store could.
fn forge_approved_row(s: &Setup) -> String {
    let conn = rusqlite::Connection::open(s.db_path()).unwrap();
    conn.execute(
        "INSERT INTO approvals (id, seq, kind, title, digest, requester, status,
                                created_at, decided_by, decided_via, decided_at)
         VALUES ('forged-1', 9001, 'destroy', 'forged', ?1, 'requester', 'approved',
                 '2026-01-01T00:00:00+00:00', 'operator', 'api', '2026-01-01T00:00:01+00:00')",
        [digest()],
    )
    .unwrap();
    "AP-9001".to_string()
}

#[test]
fn forged_row_consume_fails() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let s = setup(Some("enforce"));
    let id = forge_approved_row(&s);
    let r = consume(&s.db, &id, &digest());
    assert!(
        r.is_err(),
        "a row with no journal record must not be consumable: {r:?}"
    );
    assert!(
        matches!(r, Err(Error::Approval(ApprovalError::Unverified(_)))),
        "{r:?}"
    );
    assert_eq!(
        ApprovalError::Unverified(String::new()).verify_exit_code(),
        Some(7)
    );
}

#[test]
fn journaled_flow_consumes_once_and_replay_fails() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let s = setup(Some("enforce"));
    let ap = request(&s.db, Some("1h"));
    approve(&s.db, &ap.ap_id());
    consume(&s.db, &ap.ap_id(), &digest()).expect("journaled approval consumes");
    assert!(consume(&s.db, &ap.ap_id(), &digest()).is_err(), "replay");
}

#[test]
fn wrong_action_digest_fails() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let s = setup(Some("enforce"));
    let ap = request(&s.db, None);
    approve(&s.db, &ap.ap_id());
    assert!(consume(&s.db, &ap.ap_id(), &"b".repeat(64)).is_err());
}

#[test]
fn row_edited_after_approval_is_refused() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let s = setup(Some("enforce"));
    let ap = request(&s.db, Some("1h"));
    approve(&s.db, &ap.ap_id());
    // Extend the expiry behind the triggers' back, as a writer of the file could.
    let conn = rusqlite::Connection::open(s.db_path()).unwrap();
    conn.execute_batch("DROP TRIGGER IF EXISTS approvals_lock_after_decision;")
        .unwrap();
    conn.execute(
        "UPDATE approvals SET expires_at = '2099-01-01T00:00:00+00:00'",
        [],
    )
    .unwrap();
    let r = consume(&s.db, &ap.ap_id(), &digest());
    assert!(r.is_err(), "expiry no longer matches the journal: {r:?}");
}

#[test]
fn expired_approval_is_refused() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let s = setup(Some("enforce"));
    let ap = request(&s.db, Some("1s"));
    approve(&s.db, &ap.ap_id());
    std::thread::sleep(std::time::Duration::from_millis(2100));
    assert!(consume(&s.db, &ap.ap_id(), &digest()).is_err());
}

#[test]
fn tampered_journal_is_refused() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let s = setup(Some("enforce"));
    let ap = request(&s.db, None);
    approve(&s.db, &ap.ap_id());
    let text = std::fs::read_to_string(&s.journal).unwrap();
    std::fs::write(&s.journal, text.replace("operator", "operatox")).unwrap();
    assert!(consume(&s.db, &ap.ap_id(), &digest()).is_err());
    // Truncating the chain (dropping the approve record) is also refused.
    let first: String = text.lines().take(1).collect::<Vec<_>>().join("\n") + "\n";
    std::fs::write(&s.journal, first).unwrap();
    assert!(consume(&s.db, &ap.ap_id(), &digest()).is_err());
}

#[test]
fn shadow_mode_records_but_does_not_refuse() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let s = setup(Some("shadow"));
    let id = forge_approved_row(&s);
    assert!(consume(&s.db, &id, &digest()).is_ok(), "shadow allows");
    let ap = request(&s.db, None);
    approve(&s.db, &ap.ap_id());
    let text = std::fs::read_to_string(&s.journal).unwrap();
    assert!(text.contains("\"ev\":\"approve\""));
}

#[test]
fn default_off_keeps_legacy_behaviour() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let s = setup(None);
    let id = forge_approved_row(&s);
    assert!(consume(&s.db, &id, &digest()).is_ok());
    assert!(!s.journal.exists());
}

#[test]
fn unrecognised_mode_fails_closed_not_off() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    // A typo in the flag must not silently turn the gate off.
    let s = setup(Some("enforcd"));
    let id = forge_approved_row(&s);
    let r = consume(&s.db, &id, &digest());
    assert!(
        matches!(r, Err(Error::Approval(ApprovalError::Unverified(_)))),
        "typo'd mode must behave as enforce: {r:?}"
    );
}

#[test]
fn explicit_off_values_stay_off() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    for v in ["off", "0", "false", ""] {
        let s = setup(Some(v));
        let id = forge_approved_row(&s);
        assert!(consume(&s.db, &id, &digest()).is_ok(), "{v:?} must be off");
    }
}

#[test]
fn verify_of_missing_journal_is_an_error_not_zero_records() {
    use ptask_core::approval_journal::{Journal, Mode};
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key");
    std::fs::write(&key, "0123456789abcdef0123456789abcdef\n").unwrap();
    let j = Journal::new(Mode::Enforce, dir.path().join("absent.jsonl"), &key);
    assert!(
        j.verify().is_err(),
        "a missing journal must not verify green"
    );
}
