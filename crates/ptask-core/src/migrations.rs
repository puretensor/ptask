//! Embedded refinery migrations for pTask side tables.
//!
//! Migrations are bundled into the binary; running `Db::open()` applies all
//! pending migrations idempotently against the target SQLite file.

refinery::embed_migrations!("migrations");

pub use migrations::runner;

/// Apply all pending migrations on the given connection.
pub fn run(conn: &mut rusqlite::Connection) -> Result<refinery::Report, refinery::Error> {
    runner().set_grouped(true).run(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v018_keeps_approvals_and_tamper_triggers_and_drops_the_task_fk() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = rusqlite::Connection::open(dir.path().join("t.db")).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        runner()
            .set_grouped(true)
            .set_target(refinery::Target::Version(17))
            .run(&mut conn)
            .unwrap();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, created_at, updated_at)
                 VALUES ('t1', 'task', '2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z');
             INSERT INTO approvals (id, seq, kind, title, digest, requester, task_uuid,
                                    status, decided_by, created_at, decided_at)
                 VALUES ('a1', 1, 'other', 'decided', printf('%.64c', 'a'), 'hal', 't1',
                         'approved', 'operator', '2026-09-01T00:00:00Z', '2026-09-02T00:00:00Z');",
        )
        .unwrap();

        run(&mut conn).unwrap();

        let (n, task): (i64, String) = conn
            .query_row("SELECT COUNT(*), MAX(task_uuid) FROM approvals", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((n, task.as_str()), (1, "t1"));
        let fks: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_foreign_key_list('approvals')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fks, 0);
        // The decided row is still frozen, and the task can now be deleted.
        assert!(
            conn.execute("UPDATE approvals SET title = 'x' WHERE id = 'a1'", [])
                .is_err()
        );
        assert!(
            conn.execute("UPDATE approvals SET digest = printf('%.64c', 'b')", [])
                .is_err()
        );
        conn.execute("DELETE FROM tasks WHERE id = 't1'", [])
            .unwrap();
        let objects: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN (
                    'idx_approvals_pending_digest', 'idx_approvals_status_seq',
                    'approvals_immutable_payload', 'approvals_lock_after_decision',
                    'idx_pt_event_log_type_ts')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(objects, 5);
    }
}
