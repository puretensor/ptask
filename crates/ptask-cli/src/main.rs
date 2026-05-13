//! `pt` — pTask command-line interface.
//!
//! Scaffold. v0.0.x commits land `add`, `list`, `done` (Python parity).
//! Subsequent phases add `next`, `edit`, `show`, `rm`, `view`, `serve`, `bot`, `tui`.

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "pt",
    version = ptask_core::VERSION,
    about = "Sovereign task manager for PureTensor",
    long_about = None,
)]
struct Cli {}

fn main() -> Result<()> {
    let _ = Cli::parse();
    println!("pt {} — scaffold. See docs/master-plan.md.", ptask_core::VERSION);
    Ok(())
}
