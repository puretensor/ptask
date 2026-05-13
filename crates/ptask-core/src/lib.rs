//! ptask-core — domain logic and storage for pTask.
//!
//! Currently a scaffold. Subsequent v0.0.x commits will land:
//!   - storage::Db        (rusqlite + r2d2 pool, WAL, busy_timeout)
//!   - migrations         (refinery V001..V006 for pt_* side tables)
//!   - pt_id              (PT-N minting + lookup)
//!   - tasks              (CRUD parity with the existing Python schema)
//!
//! See docs/master-plan.md § v0.1.0 for the full sub-section breakdown.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
