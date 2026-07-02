//! SQLite storage layer for pTask.
//!
//! - `rusqlite` with the `bundled` feature: SQLite compiled into the binary.
//! - `r2d2` pool with per-connection pragmas applied on acquire.
//! - WAL journal mode (better concurrent reads), busy_timeout 30s, FK on.
//! - Path resolution lives in `crate::config` — storage itself never
//!   touches the process environment.

use crate::error::{Error, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::OpenFlags;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

/// Default path: `$PTASK_DB`, else `~/puretensor-tasks/tasks.db`. Delegates
/// to [`crate::config::env_db_path`] — kept as a re-export so existing
/// callers don't churn.
pub use crate::config::env_db_path as default_db_path;

#[derive(Clone)]
pub struct Db {
    inner: Arc<DbInner>,
}

struct DbInner {
    path: PathBuf,
    pool: Pool<SqliteConnectionManager>,
}

impl Db {
    /// Open the DB at the default path, applying pending migrations.
    pub fn open_default() -> Result<Self> {
        Self::open(default_db_path())
    }

    /// Open a DB at `path`, applying pending migrations.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            // Create parent dir if missing.
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
        }
        info!(target: "ptask::storage", path = %path.display(), "opening db");

        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;

        let manager = SqliteConnectionManager::file(&path)
            .with_flags(flags)
            .with_init(|c| {
                // Pragmas applied on every acquired connection.
                c.pragma_update(None, "journal_mode", "WAL")?;
                c.pragma_update(None, "synchronous", "NORMAL")?;
                c.pragma_update(None, "foreign_keys", "ON")?;
                c.busy_timeout(Duration::from_secs(30))?;
                Ok(())
            });

        let pool = r2d2::Pool::builder()
            .max_size(8)
            .min_idle(Some(1))
            .connection_timeout(Duration::from_secs(30))
            .build(manager)
            .map_err(Error::Pool)?;

        // Apply migrations on a pooled connection. The pool's init has already
        // applied pragmas.
        let mut conn = pool.get().map_err(Error::Pool)?;
        let report = crate::migrations::run(&mut conn).map_err(Error::Migration)?;
        drop(conn);
        debug!(
            target: "ptask::storage",
            applied = report.applied_migrations().len(),
            "migrations complete"
        );

        Ok(Self {
            inner: Arc::new(DbInner { path, pool }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Acquire a pooled connection.
    pub fn get(&self) -> Result<r2d2::PooledConnection<SqliteConnectionManager>> {
        self.inner.pool.get().map_err(Error::Pool)
    }

    /// Convenience: run a closure with a connection.
    pub fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut rusqlite::Connection) -> Result<T>,
    {
        let mut conn = self.get()?;
        f(&mut conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Open a temp DB and verify migrations created the side tables.
    #[test]
    fn opens_fresh_db_and_applies_migrations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let db = Db::open(&path).expect("open ok");
        db.with_conn(|c| {
            // Each pt_* table should exist after migrations.
            for table in [
                "pt_counters",
                "pt_extensions",
                "pt_views",
                "pt_recurrence",
                "pt_event_log",
                "pt_webhook_log",
            ] {
                let exists: i64 = c
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                        [table],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(exists, 1, "expected table {} to exist", table);
            }
            // The counter seed should be present.
            let counter: i64 = c
                .query_row(
                    "SELECT value FROM pt_counters WHERE name='pt_id'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(counter, 0);
            Ok(())
        })
        .unwrap();
    }

    /// Greenfield bootstrap: no Python stub, just `Db::open` on a fresh path.
    /// V008 must create the legacy base schema so a brand-new install can
    /// accept writes (previously: "no such table: tasks").
    #[test]
    fn greenfield_db_bootstraps_base_schema_and_accepts_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh.db");
        let db = Db::open(&path).expect("open ok");
        db.with_conn(|c| {
            for table in [
                "tasks",
                "interactions",
                "notifications",
                "raw_items",
                "canonical_tasks",
                "ingested_files",
                "daily_budget",
            ] {
                let exists: i64 = c
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                        [table],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(exists, 1, "expected base table {} to exist", table);
            }
            Ok(())
        })
        .unwrap();

        // End-to-end write through the real task path.
        let task = crate::tasks::create_with_extensions(
            &db,
            crate::tasks::NewTask::minimal("first task on a fresh install"),
            crate::tasks::Extensions::default(),
        )
        .expect("create works on greenfield DB");
        assert_eq!(task.pt_id.as_deref(), Some("PT-1"));
    }

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let _db1 = Db::open(&path).unwrap();
        let _db2 = Db::open(&path).unwrap(); // second open re-runs migrations harmlessly
    }

    /// A DB that already carries the legacy Python schema (the live-fleet
    /// case) must keep working: V008's IF NOT EXISTS guards make it a no-op
    /// over pre-existing tables, even ones created with a different column
    /// set than V008 would write.
    #[test]
    fn legacy_python_db_still_opens_after_v008() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            // Live-shape excerpt: enough columns for the V008 indices.
            conn.execute_batch(
                "CREATE TABLE tasks (
                    id TEXT PRIMARY KEY, title TEXT NOT NULL,
                    priority INTEGER DEFAULT 2, status TEXT DEFAULT 'pending',
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                    priority_score REAL DEFAULT 0.0
                 );
                 CREATE INDEX idx_tasks_status ON tasks(status);",
            )
            .unwrap();
        }
        let db = Db::open(&path).expect("legacy-shaped DB opens");
        db.with_conn(|c| {
            // Pre-existing table kept (no clobber): the legacy column set
            // survives, and the side tables exist.
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('tasks','pt_extensions')",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(n, 2);
            Ok(())
        })
        .unwrap();
    }
}
