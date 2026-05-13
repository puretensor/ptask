//! Top-level [`App`] state + event loop.

use crate::event::{Event, poll_event};
use crate::ui;
use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyModifiers};
use ptask_core::tasks::TaskDetail;
use ptask_core::{Db, Task, tasks};
use ratatui::DefaultTerminal;
use ratatui::widgets::ListState;

pub struct App {
    pub db: Db,
    pub tasks: Vec<Task>,
    pub list_state: ListState,
    pub status_msg: String,
    pub quit: bool,

    /// Pending key chord (e.g. waiting for the second `g` after first).
    pub pending_g: bool,
    /// Last viewport height — captured during render so PageDown/Up scale.
    pub viewport_rows: u16,

    /// Peek (detail) pane state. When on, the layout splits horizontally
    /// and the right pane shows TaskDetail for the selected task.
    pub peek_open: bool,
    /// Cached detail; invalidated when selection or task list changes.
    pub peek_detail: Option<TaskDetail>,
    /// task_uuid of the currently-cached peek_detail, for invalidation.
    pub peek_uuid: Option<String>,
}

impl App {
    pub fn new(db: Db) -> Result<Self> {
        let tasks = tasks::list_with_filter(&db, None, Some("pending"), None, 200)
            .context("loading initial task list")?;
        let mut list_state = ListState::default();
        if !tasks.is_empty() {
            list_state.select(Some(0));
        }
        Ok(Self {
            db,
            tasks,
            list_state,
            status_msg: format!("pt {} — TUI", ptask_core::VERSION),
            quit: false,
            pending_g: false,
            viewport_rows: 20,
            peek_open: false,
            peek_detail: None,
            peek_uuid: None,
        })
    }

    pub fn selected(&self) -> Option<usize> {
        self.list_state.selected()
    }

    pub fn selected_task(&self) -> Option<&Task> {
        self.list_state.selected().and_then(|i| self.tasks.get(i))
    }

    /// Refresh the cached peek detail to match the current selection.
    /// No-op when peek is closed or selection unchanged.
    pub fn refresh_peek(&mut self) {
        if !self.peek_open {
            self.peek_detail = None;
            self.peek_uuid = None;
            return;
        }
        let Some(task) = self.selected_task() else {
            self.peek_detail = None;
            self.peek_uuid = None;
            return;
        };
        if self.peek_uuid.as_deref() == Some(task.id.as_str()) {
            return;
        }
        match tasks::load_detail(&self.db, &task.id) {
            Ok(d) => {
                self.peek_uuid = Some(task.id.clone());
                self.peek_detail = Some(d);
            }
            Err(e) => {
                self.peek_detail = None;
                self.peek_uuid = None;
                self.status_msg = format!("peek load failed: {}", e);
            }
        }
    }

    /// Reload the visible task list from the DB. Currently fixed to
    /// pending + ordered by `list_with_filter`. Filtering/view switching
    /// will reshape this in later sub-versions.
    pub fn reload(&mut self) -> Result<()> {
        self.tasks = tasks::list_with_filter(&self.db, None, Some("pending"), None, 200)
            .context("reloading task list")?;
        if self.tasks.is_empty() {
            self.list_state.select(None);
        } else if let Some(i) = self.list_state.selected() {
            if i >= self.tasks.len() {
                self.list_state.select(Some(self.tasks.len() - 1));
            }
        } else {
            self.list_state.select(Some(0));
        }
        Ok(())
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            self.refresh_peek();
            terminal.draw(|f| ui::render(f, self))?;
            if let Some(ev) = poll_event(std::time::Duration::from_millis(200))? {
                self.handle(ev);
            }
        }
        Ok(())
    }

    fn handle(&mut self, ev: Event) {
        let Event::Key(key) = ev else {
            self.pending_g = false;
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let was_g = self.pending_g;
        self.pending_g = false;

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,

            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char('d') if ctrl => self.move_cursor(self.half_page()),
            KeyCode::Char('u') if ctrl => self.move_cursor(-self.half_page()),
            KeyCode::PageDown => self.move_cursor(self.viewport_rows as i64),
            KeyCode::PageUp => self.move_cursor(-(self.viewport_rows as i64)),

            KeyCode::Char('G') => self.cursor_to(self.tasks.len().saturating_sub(1)),
            KeyCode::Char('g') => {
                if was_g {
                    self.cursor_to(0);
                } else {
                    self.pending_g = true;
                }
            }
            KeyCode::Home => self.cursor_to(0),
            KeyCode::End => self.cursor_to(self.tasks.len().saturating_sub(1)),

            KeyCode::Char('r') if !ctrl => {
                if let Err(e) = self.reload() {
                    self.status_msg = format!("reload failed: {}", e);
                } else {
                    self.status_msg = format!("reloaded — {} pending", self.tasks.len());
                }
            }
            KeyCode::Char(' ') => {
                self.peek_open = !self.peek_open;
                self.peek_uuid = None; // force reload
            }
            _ => {}
        }
    }

    fn move_cursor(&mut self, delta: i64) {
        if self.tasks.is_empty() {
            return;
        }
        let cur = self.list_state.selected().unwrap_or(0) as i64;
        let max = (self.tasks.len() - 1) as i64;
        let next = (cur + delta).clamp(0, max) as usize;
        self.list_state.select(Some(next));
    }

    fn cursor_to(&mut self, idx: usize) {
        if self.tasks.is_empty() {
            return;
        }
        self.list_state.select(Some(idx.min(self.tasks.len() - 1)));
    }

    fn half_page(&self) -> i64 {
        (self.viewport_rows / 2).max(1) as i64
    }
}
