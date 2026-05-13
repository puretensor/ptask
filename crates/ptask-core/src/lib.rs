//! ptask-core — domain logic and storage for pTask.
//!
//! Modules land per the master plan (`docs/master-plan.md`). v0.0.x focuses on
//! storage + migrations + PT-N minting; later phases add parsing, scoring,
//! accountability, and distillation.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod dag;
pub mod dates;
pub mod error;
pub mod filter;
pub mod migrations;
pub mod priority;
pub mod pt_id;
pub mod quickadd;
pub mod recurrence;
pub mod storage;
pub mod tasks;

pub use error::{Error, Result};
pub use storage::{Db, default_db_path};
pub use tasks::{Extensions, NewTask, Task};
