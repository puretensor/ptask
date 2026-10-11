//! HAL: ready-but-unclaimable rows must not hide later claimable work.
use ptask_core::{claims, dag, event_log::EventCtx, storage::Db, tasks::{self,NewTask}};

fn verify_tail_claim(in_progress: bool) {
    let dir=tempfile::tempdir().unwrap();
    let db=Db::open(dir.path().join("fixture.db")).unwrap();
    let ctx=EventCtx::test();
    let tail=tasks::create(&db,NewTask::minimal("available tail"),&ctx).unwrap();
    tasks::update_priority(&db,&tail.id,1,&ctx).unwrap();
    let mut skip=Vec::new();
    for i in 0..50 {
        let t=tasks::create(&db,NewTask::minimal(&format!("already owned or skipped {i}")),&ctx).unwrap();
        tasks::update_priority(&db,&t.id,5,&ctx).unwrap();
        if in_progress {claims::claim(&db,&t.id,None,&EventCtx::local("other")).unwrap();}
        else {skip.push(t.id);}
    }
    let got=dag::claim_next(&db,&EventCtx::local("hal"),&skip,None).unwrap();
    assert_eq!(got.map(|(t,_)|t.id),Some(tail.id),"ready high-priority rows hid a claimable tail task");
}
#[test]
fn quota90_claim_next_passes_fifty_in_progress_rows(){verify_tail_claim(true);}
#[test]
fn quota90_claim_next_passes_fifty_explicitly_skipped_rows(){verify_tail_claim(false);}
