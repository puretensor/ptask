//! Top-level [`App`] state + event loop.

use crate::event::{Event, poll_event};
use crate::ui;
use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyModifiers};
use ptask_core::{Db, Task, tasks};
use ratatui::DefaultTerminal;

pub struct App {
    pub db: Db,
    pub tasks: Vec<Task>,
    pub selected: usize,
    pub status_msg: String,
    pub quit: bool,
}

impl App {
    pub fn new(db: Db) -> Result<Self> {
        let tasks = tasks::list_with_filter(&db, None, Some("pending"), None, 200)
            .context("loading initial task list")?;
        Ok(Self {
            db,
            tasks,
            selected: 0,
            status_msg: format!("pt {} — TUI v0.2.4 skeleton", ptask_core::VERSION),
            quit: false,
        })
    }

    /// Reload the visible task list from the DB. Currently fixed to
    /// pending + ordered by `list_with_filter`. Filtering/view switching
    /// will reshape this in later sub-versions.
    pub fn reload(&mut self) -> Result<()> {
        self.tasks = tasks::list_with_filter(&self.db, None, Some("pending"), None, 200)
            .context("reloading task list")?;
        if self.selected >= self.tasks.len() {
            self.selected = self.tasks.len().saturating_sub(1);
        }
        Ok(())
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            terminal.draw(|f| ui::render(f, self))?;
            if let Some(ev) = poll_event(std::time::Duration::from_millis(200))? {
                self.handle(ev);
            }
        }
        Ok(())
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Key(key) => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Char('q') => self.quit = true,
                    KeyCode::Esc => self.quit = true,
                    KeyCode::Char('c') if ctrl => self.quit = true,
                    _ => {}
                }
            }
            Event::Resize | Event::Tick => {}
        }
    }
}
