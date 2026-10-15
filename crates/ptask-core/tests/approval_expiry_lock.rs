use ptask_core::approvals::{
    self, ApprovalError, DecidedVia, Decision, PayloadSource, RequestInput,
};
use ptask_core::event_log::EventCtx;
use ptask_core::{Db, Error};
use std::sync::mpsc;
use std::time::Duration;

fn request(db: &Db) -> approvals::Approval {
    approvals::request(
        db,
        RequestInput {
            kind: "destroy".into(),
            title: "An action with a short deadline".into(),
            request_note: None,
            payload: PayloadSource::Digest("a".repeat(64)),
            task_pt_id: None,
            expires_in: Some("2s".into()),
        },
        &EventCtx::local("requester"),
    )
    .unwrap()
    .approval
}

fn decide(db: &Db, id: &str) -> ptask_core::Result<approvals::Approval> {
    approvals::decide(
        db,
        id,
        Decision::Approve,
        DecidedVia::Api,
        None,
        &EventCtx::local("operator"),
    )
}

fn run_behind_writer(
    db: &Db,
    operation: impl FnOnce(Db) -> ptask_core::Result<approvals::Approval> + Send + 'static,
) -> ptask_core::Result<approvals::Approval> {
    let mut conn = db.get().unwrap();
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let (started, ready) = mpsc::channel();
    let other_db = db.clone();
    let worker = std::thread::spawn(move || {
        started.send(()).unwrap();
        operation(other_db)
    });
    ready.recv().unwrap();
    // The call starts before expiry but its BEGIN IMMEDIATE cannot finish
    // until this writer releases the lock, after the deadline has passed.
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !worker.is_finished(),
        "operation must be waiting for the writer"
    );
    tx.commit().unwrap();
    worker.join().unwrap()
}

#[test]
fn consume_rechecks_expiry_after_waiting_for_the_write_lock() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("approval.db")).unwrap();
    let ap = request(&db);
    decide(&db, &ap.ap_id()).unwrap();
    let id = ap.ap_id();
    let result = run_behind_writer(&db, move |db| {
        approvals::consume(
            &db,
            &id,
            &PayloadSource::Digest("a".repeat(64)),
            &EventCtx::local("requester"),
        )
    });
    assert!(
        matches!(result, Err(Error::Approval(ApprovalError::Terminal(_)))),
        "an approval that expired while waiting must not authorize an action: {result:?}"
    );
    assert!(
        approvals::get(&db, &ap.ap_id())
            .unwrap()
            .consumed_at
            .is_none()
    );
}

#[test]
fn decide_rechecks_expiry_after_waiting_for_the_write_lock() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path().join("approval.db")).unwrap();
    let ap = request(&db);
    let id = ap.ap_id();
    let result = run_behind_writer(&db, move |db| decide(&db, &id));
    assert!(
        result.is_err(),
        "a request that expired while waiting cannot be approved"
    );
    assert_eq!(approvals::get(&db, &ap.ap_id()).unwrap().status, "expired");
}
