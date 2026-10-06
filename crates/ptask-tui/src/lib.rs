//! pTask terminal UI.
//!
//! Phase 3 of v1.0.0. `pt tui` enters the alternate screen, runs an
//! [`App`] event loop, exits cleanly on `q` / `Esc` / `Ctrl-C`.

mod app;
mod event;
mod ui;

use anyhow::Result;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use ptask_core::Db;

use app::App;

/// Launch the TUI against `db`. Blocks until the user quits.
pub fn run(db: Db) -> Result<()> {
    let mut terminal = ratatui::init();
    // Bracketed paste: a pasted block arrives as one Event::Paste instead of
    // keystrokes, so its newlines cannot press Enter and its letters cannot
    // fire d/p/c. Switched off again on exit and on panic.
    let _ = crossterm::execute!(std::io::stdout(), EnableBracketedPaste);
    let restore_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
        restore_hook(info);
    }));
    let result = (|| -> Result<()> {
        let mut app = App::new(db)?;
        app.run(&mut terminal)
    })();
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}
