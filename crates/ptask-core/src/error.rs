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

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
