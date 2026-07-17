//! ptask-core — domain logic and storage for pTask.
//!
//! Modules land per the master plan (`docs/master-plan.md`). v0.0.x focuses on
//! storage + migrations + PT-N minting; later phases add parsing, scoring,
//! accountability, and distillation.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod accountability;
pub mod config;
pub mod convert;
pub mod dag;
pub mod dates;
pub mod digest;
pub mod error;
pub mod event_log;
pub mod filter;
pub mod magic_words;
pub mod migrations;
pub mod planner;
pub mod priority;
pub mod pt_id;
pub mod quickadd;
pub mod raw_items;
pub mod reap;
pub mod recurrence;
pub mod scoring;
pub mod status;
pub mod storage;
pub mod tasks;
pub mod tokens;
pub mod views;
pub mod webhook_log;

pub use jiff;
pub use rusqlite;

pub use config::Config;
pub use error::{Error, Result};
pub use storage::{Db, default_db_path};
pub use tasks::{Extensions, NewTask, Task};
