//! SQLite storage layer for pTask.
//!
//! - `rusqlite` with the `bundled` feature: SQLite compiled into the binary.
//! - `r2d2` pool with per-connection pragmas applied on acquire.
//! - WAL journal mode (better concurrent reads), busy_timeout 30s, FK on.
//! - Default DB path resolves $PTASK_DB, then ~/puretensor-tasks/tasks.db.

use crate::error::{Error, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::OpenFlags;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

/// Default path: `~/puretensor-tasks/tasks.db` (the existing Python store).
pub fn default_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("PTASK_DB") {
        return PathBuf::from(p);
    }
    // Home dir → ~/puretensor-tasks/tasks.db
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    PathBuf::from(home).join("puretensor-tasks").join("tasks.db")
}

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
        // The migrations reference `tasks(id)` via FK. For tests we create a
        // minimal stub of the Python schema first.
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute(
                "CREATE TABLE tasks (id TEXT PRIMARY KEY, title TEXT NOT NULL)",
                [],
            )
            .unwrap();
        }
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
                .query_row("SELECT value FROM pt_counters WHERE name='pt_id'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(counter, 0);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute(
                "CREATE TABLE tasks (id TEXT PRIMARY KEY, title TEXT NOT NULL)",
                [],
            )
            .unwrap();
        }
        let _db1 = Db::open(&path).unwrap();
        let _db2 = Db::open(&path).unwrap(); // second open re-runs migrations harmlessly
    }
}
