//! pTask terminal UI.
//!
//! Phase 3 of v1.0.0. `pt tui` enters the alternate screen, runs an
//! [`App`] event loop, exits cleanly on `q` / `Esc` / `Ctrl-C`.

mod app;
mod event;
mod ui;

use anyhow::Result;
use ptask_core::Db;

pub use app::App;

/// Launch the TUI against `db`. Blocks until the user quits.
pub fn run(db: Db) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        let mut app = App::new(db)?;
        app.run(&mut terminal)
    })();
    ratatui::restore();
    result
}
