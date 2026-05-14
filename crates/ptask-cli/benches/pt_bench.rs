//! Throughput benches for the core CLI verbs.
//!
//! Run: `cargo bench -p ptask-cli`
//!
//! Phase 1.0 budget: p99 < 50ms on a 10k-task DB for `add`, `list`, `next`.
//! Initial scaffold benches small populations; the v1.0.x re-attack scales
//! to 10k once the perf gate lands in CI.

use criterion::{Criterion, criterion_group, criterion_main};
use ptask_core::{Db, NewTask, dag, quickadd, tasks};
use std::hint::black_box;

fn fresh_db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench.db");
    (dir, Db::open(&path).unwrap())
}

fn seed(db: &Db, n: usize) {
    for i in 0..n {
        tasks::create(
            db,
            NewTask::minimal(format!("bench task {i} — investigate ceph mon quorum")),
        )
        .unwrap();
    }
}

fn bench_add(c: &mut Criterion) {
    c.bench_function("add quickadd-parse", |b| {
        b.iter(|| {
            let q =
                quickadd::parse(black_box("buy bread tomorrow 10am @home #fleet p1 ~30m")).unwrap();
            black_box(q);
        });
    });

    c.bench_function("add insert-then-list-100", |b| {
        let (_d, db) = fresh_db();
        seed(&db, 100);
        b.iter(|| {
            tasks::create(&db, NewTask::minimal("ephemeral bench task")).unwrap();
            let rows = tasks::list_with_filter(&db, None, Some("pending"), None, 100).unwrap();
            black_box(rows);
        });
    });
}

fn bench_list(c: &mut Criterion) {
    let (_d, db) = fresh_db();
    seed(&db, 1_000);
    c.bench_function("list-1000-pending-top20", |b| {
        b.iter(|| {
            let rows = tasks::list_with_filter(&db, None, Some("pending"), None, 20).unwrap();
            black_box(rows);
        });
    });
}

fn bench_next(c: &mut Criterion) {
    let (_d, db) = fresh_db();
    seed(&db, 500);
    c.bench_function("next-500-no-deps", |b| {
        b.iter(|| {
            let rows = dag::next_ready(&db, 20).unwrap();
            black_box(rows);
        });
    });
}

criterion_group!(benches, bench_add, bench_list, bench_next);
criterion_main!(benches);
