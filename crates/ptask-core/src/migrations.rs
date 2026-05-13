//! Embedded refinery migrations for pTask side tables.
//!
//! Migrations are bundled into the binary; running `Db::open()` applies all
//! pending migrations idempotently against the target SQLite file.

refinery::embed_migrations!("migrations");

pub use migrations::runner;

/// Apply all pending migrations on the given connection.
pub fn run(conn: &mut rusqlite::Connection) -> Result<refinery::Report, refinery::Error> {
    runner().run(conn)
}
