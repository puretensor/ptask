//! pTask terminal UI.
//!
//! v0.2.4 skeleton: enters/leaves the alternate screen, renders a header,
//! a task list placeholder, and a status bar, polls keyboard events, exits
//! on `q` / `Esc` / `Ctrl-C`. Real navigation/edit verbs land in subsequent
//! sub-versions of phase 3.

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
