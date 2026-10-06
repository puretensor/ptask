//! pTask error type.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("connection pool: {0}")]
    Pool(#[from] r2d2::Error),

    #[error("migration: {0}")]
    Migration(#[from] refinery::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid db path: {0}")]
    InvalidDbPath(String),

    #[error("pt_id not found: {0}")]
    PtIdNotFound(String),

    /// The task has `depends_on` edges to tasks that are still open, so it
    /// cannot be completed yet. The message names every open blocker.
    #[error("{0}")]
    Blocked(String),

    #[error(transparent)]
    Approval(#[from] crate::approvals::ApprovalError),

    #[error(transparent)]
    Goal(#[from] crate::goals::GoalError),

    /// Date arithmetic left the representable range (years 1..9999 in the
    /// operator timezone): a recurrence with no next occurrence.
    #[error("date out of range: {0}")]
    OutOfRange(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
