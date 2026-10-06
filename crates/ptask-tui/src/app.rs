//! Top-level [`App`] state + event loop.

use crate::event::{Event, poll_event};
use crate::ui;
use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyModifiers};
use nucleo::Matcher;
use nucleo::Utf32String;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use ptask_core::tasks::TaskDetail;
use ptask_core::views::View;
use ptask_core::{Db, Task, tasks, views};
use ratatui::DefaultTerminal;
use ratatui::widgets::ListState;

const TUI_TASK_LIMIT: usize = 1000;

/// Attribution for TUI-initiated mutations (the operator at the keyboard).
fn tui_ctx() -> ptask_core::event_log::EventCtx {
    ptask_core::event_log::EventCtx {
        actor: "shell".into(),
        source: "tui".into(),
        event_uuid: None,
        command: None,
    }
}

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

    /// Fuzzy filter — applied to tasks via nucleo. Indices into `tasks`,
    /// sorted by score descending. When `filter_query` is empty this is
    /// just 0..tasks.len() in original order.
    pub filtered: Vec<usize>,
    pub filter_query: String,
    /// When Some, the filter bar is open and capturing keystrokes.
    /// The String is the in-progress filter text.
    pub filter_input: Option<String>,
    matcher: Matcher,

    /// When Some, an input prompt is open (Create / etc.). Keys go to
    /// the prompt buffer until Enter / Esc.
    pub prompt: Option<Prompt>,

    /// Most-recent confirmation request. When Some, the next y/n press
    /// resolves it.
    pub confirm: Option<Confirm>,

    /// Active view selector. Drives the task list query in `reload`.
    pub view: ViewSel,
    /// Cached saved views from pt_views (alphabetical). Refreshed on
    /// startup and after view CRUD (deferred to a later iteration).
    pub saved_views: Vec<View>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewSel {
    /// All pending tasks (the default).
    Pending,
    /// A user-saved view (filter DSL).
    Saved { name: String, dsl: String },
}

impl ViewSel {
    pub fn label(&self) -> String {
        match self {
            ViewSel::Pending => "pending".into(),
            ViewSel::Saved { name, .. } => format!("view:{}", name),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Prompt {
    /// Quick-add new task (Enter parses the buffer via quickadd::parse).
    Create { buf: String },
}

impl Prompt {
    pub fn label(&self) -> &'static str {
        match self {
            Prompt::Create { .. } => "create",
        }
    }
    pub fn buf(&self) -> &str {
        match self {
            Prompt::Create { buf } => buf,
        }
    }
    pub fn push(&mut self, c: char) {
        match self {
            Prompt::Create { buf } => buf.push(c),
        }
    }
    pub fn pop(&mut self) {
        match self {
            Prompt::Create { buf } => {
                buf.pop();
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum Confirm {
    /// Delete the selected task UUID. y → delete, n/Esc → cancel.
    Delete { task_uuid: String, title: String },
}

impl App {
    pub fn new(db: Db) -> Result<Self> {
        let tasks = tasks::list_with_filter(&db, None, Some("pending"), None, TUI_TASK_LIMIT)
            .context("loading initial task list")?;
        let saved_views = views::list(&db).unwrap_or_default();
        let mut list_state = ListState::default();
        if !tasks.is_empty() {
            list_state.select(Some(0));
        }
        let filtered: Vec<usize> = (0..tasks.len()).collect();
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
            filtered,
            filter_query: String::new(),
            filter_input: None,
            matcher: Matcher::default(),
            prompt: None,
            confirm: None,
            view: ViewSel::Pending,
            saved_views,
        })
    }

    /// Indices into `tasks` for currently-visible rows.
    pub fn visible(&self) -> &[usize] {
        &self.filtered
    }

    /// Resolve the currently-selected visible row → underlying `tasks` index.
    pub fn selected_task_index(&self) -> Option<usize> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered.get(i).copied())
    }

    pub fn selected_task(&self) -> Option<&Task> {
        self.selected_task_index().and_then(|i| self.tasks.get(i))
    }

    /// Recompute `filtered` from `filter_query`. Empty query → identity;
    /// otherwise nucleo fuzzy score against `PT-N title #project @labels`.
    pub fn apply_filter(&mut self) {
        if self.filter_query.is_empty() {
            self.filtered = (0..self.tasks.len()).collect();
        } else {
            let pat = Pattern::parse(
                &self.filter_query,
                CaseMatching::Smart,
                Normalization::Smart,
            );
            let mut scored: Vec<(usize, u32)> = self
                .tasks
                .iter()
                .enumerate()
                .filter_map(|(i, t)| {
                    let pt = t.pt_id.as_deref().unwrap_or("");
                    let haystack = format!("{} {}", pt, t.title);
                    let utf = Utf32String::from(haystack.as_str());
                    pat.score(utf.slice(..), &mut self.matcher).map(|s| (i, s))
                })
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            self.filtered = scored.into_iter().map(|(i, _)| i).collect();
        }
        if self.filtered.is_empty() {
            self.list_state.select(None);
        } else {
            let cur = self.list_state.selected().unwrap_or(0);
            self.list_state
                .select(Some(cur.min(self.filtered.len() - 1)));
        }
        // Selection changed → invalidate peek cache.
        self.peek_uuid = None;
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

    /// Reload the visible task list from the DB. The query shape depends
    /// on the active [`ViewSel`].
    pub fn reload(&mut self) -> Result<()> {
        self.tasks = match &self.view {
            ViewSel::Pending => {
                tasks::list_with_filter(&self.db, None, Some("pending"), None, TUI_TASK_LIMIT)
                    .context("reloading pending list")?
            }
            ViewSel::Saved { dsl, .. } => {
                let expr = ptask_core::filter::parse(dsl)
                    .map_err(|e| anyhow::anyhow!("saved view DSL parse failed: {}", e))?;
                // Open tasks only, like the pending list and the bot: the DSL
                // has no status predicate, so done/dismissed rows crowded in.
                tasks::list_with_filter(
                    &self.db,
                    Some(&expr),
                    Some("pending"),
                    None,
                    TUI_TASK_LIMIT,
                )
                .context("reloading saved view")?
            }
        };
        self.apply_filter();
        if self.filtered.is_empty() {
            self.list_state.select(None);
        } else if self.list_state.selected().is_none() {
            self.list_state.select(Some(0));
        }
        Ok(())
    }

    /// Cycle to the next saved view. Order: Pending → views[0] → views[1] →
    /// ... → views[N-1] → Pending. If no saved views exist, no-op with a
    /// status message.
    pub fn action_cycle_view(&mut self) {
        // Always re-read saved views so newly-saved ones from the CLI surface.
        if let Ok(v) = views::list(&self.db) {
            self.saved_views = v;
        }
        if self.saved_views.is_empty() {
            self.status_msg = "no saved views — use `pt view save NAME 'DSL'`".into();
            return;
        }
        let next = match &self.view {
            ViewSel::Pending => Some(0),
            ViewSel::Saved { name, .. } => {
                let cur = self.saved_views.iter().position(|v| v.name == *name);
                match cur {
                    Some(i) if i + 1 < self.saved_views.len() => Some(i + 1),
                    _ => None, // wrap back to Pending
                }
            }
        };
        self.view = match next {
            Some(i) => {
                let v = &self.saved_views[i];
                ViewSel::Saved {
                    name: v.name.clone(),
                    dsl: v.filter_dsl.clone(),
                }
            }
            None => ViewSel::Pending,
        };
        match self.reload() {
            Ok(_) => {
                self.status_msg = format!(
                    "view → {}  ({} task(s))",
                    self.view.label(),
                    self.tasks.len()
                );
            }
            Err(e) => self.status_msg = format!("view switch reload failed: {}", e),
        }
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
        let key = match ev {
            Event::Key(key) => key,
            Event::Paste(text) => return self.handle_paste(&text),
            Event::Resize | Event::Tick => {
                self.pending_g = false;
                return;
            }
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Confirmation pending (e.g. Del): consume y/n/Esc.
        if let Some(c) = self.confirm.clone() {
            self.confirm = None;
            self.pending_g = false;
            match (c, key.code) {
                (Confirm::Delete { task_uuid, title }, KeyCode::Char('y'))
                | (Confirm::Delete { task_uuid, title }, KeyCode::Char('Y')) => {
                    match ptask_core::tasks::delete_task(&self.db, &task_uuid, &tui_ctx()) {
                        Ok(_) => {
                            self.status_msg = format!("deleted: {}", title);
                            if let Err(e) = self.reload() {
                                self.status_msg = format!("delete ok; reload failed: {}", e);
                            }
                        }
                        Err(e) => self.status_msg = format!("delete failed: {}", e),
                    }
                }
                _ => self.status_msg = "delete cancelled".into(),
            }
            return;
        }

        // Prompt open — capture text input.
        if self.prompt.is_some() {
            self.handle_prompt(key);
            self.pending_g = false;
            return;
        }

        // When the filter bar is open, all keypresses edit the filter (except
        // Enter / Esc which close it, and Ctrl-C which still quits).
        if let Some(buf) = self.filter_input.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    self.filter_input = None;
                    self.filter_query.clear();
                    self.apply_filter();
                    self.status_msg = "filter cleared".into();
                }
                KeyCode::Enter => {
                    self.filter_query = buf.clone();
                    self.filter_input = None;
                    self.apply_filter();
                    self.status_msg = if self.filter_query.is_empty() {
                        "filter cleared".into()
                    } else {
                        format!(
                            "filter `{}` → {} match(es)",
                            self.filter_query,
                            self.filtered.len()
                        )
                    };
                }
                KeyCode::Backspace => {
                    buf.pop();
                    self.filter_query = buf.clone();
                    self.apply_filter();
                }
                KeyCode::Char('c') if ctrl => self.quit = true,
                KeyCode::Char(c) => {
                    buf.push(c);
                    self.filter_query = buf.clone();
                    self.apply_filter();
                }
                _ => {}
            }
            self.pending_g = false;
            return;
        }

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

            KeyCode::Char('G') => self.cursor_to_last(),
            KeyCode::Char('g') => {
                if was_g {
                    self.cursor_to(0);
                } else {
                    self.pending_g = true;
                }
            }
            KeyCode::Char('v') if was_g => self.action_cycle_view(),
            KeyCode::Home => self.cursor_to(0),
            KeyCode::End => self.cursor_to_last(),

            KeyCode::Char('r') if !ctrl => {
                if let Err(e) = self.reload() {
                    self.status_msg = format!("reload failed: {}", e);
                } else {
                    self.status_msg = format!(
                        "reloaded — {} pending ({} after filter)",
                        self.tasks.len(),
                        self.filtered.len()
                    );
                }
            }
            KeyCode::Char(' ') => {
                self.peek_open = !self.peek_open;
                self.peek_uuid = None; // force reload
            }
            KeyCode::Char('/') => {
                self.filter_input = Some(self.filter_query.clone());
            }

            // Single-key actions on the current selection.
            KeyCode::Char('d') if !ctrl => self.action_done(),
            KeyCode::Char('p') if !ctrl => self.action_cycle_priority(),
            KeyCode::Char('c') if !ctrl => {
                self.prompt = Some(Prompt::Create { buf: String::new() });
            }
            KeyCode::Delete => self.action_delete_prompt(),
            _ => {}
        }
    }

    /// A paste is text for the open input, never keystrokes: its newlines
    /// must not press Enter, and with no input open (including a pending
    /// delete confirmation) nothing in it may fire an action.
    fn handle_paste(&mut self, text: &str) {
        self.pending_g = false;
        let text = paste_text(text);
        if let Some(prompt) = self.prompt.as_mut() {
            text.chars().for_each(|c| prompt.push(c));
        } else if let Some(buf) = self.filter_input.as_mut() {
            buf.push_str(&text);
            self.filter_query = buf.clone();
            self.apply_filter();
        }
    }

    fn handle_prompt(&mut self, key: crossterm::event::KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.prompt = None;
                self.status_msg = "prompt cancelled".into();
            }
            KeyCode::Enter => {
                let taken = self.prompt.take().unwrap();
                match taken {
                    Prompt::Create { buf } => self.action_create(&buf),
                }
            }
            KeyCode::Backspace => prompt.pop(),
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char(c) => prompt.push(c),
            _ => {}
        }
    }

    fn action_done(&mut self) {
        let Some(task) = self.selected_task().cloned() else {
            return;
        };
        let pt = task.pt_id.clone().unwrap_or_default();
        match tasks::mark_done(&self.db, &task, &tui_ctx()) {
            Ok(tasks::DoneOutcome::Completed) => {
                self.status_msg = format!("done: {} {}", pt, task.title);
            }
            Ok(tasks::DoneOutcome::Advanced { next_deadline }) => {
                self.status_msg =
                    format!("advanced: {} {} → next {}", pt, task.title, next_deadline);
            }
            Err(e) => {
                // Reload anyway: the usual cause is a row another surface
                // changed, and the stale copy would keep failing until `r`.
                self.status_msg = format!("done failed: {}", e);
                if let Err(e) = self.reload() {
                    self.status_msg = format!("{} (reload failed: {e})", self.status_msg);
                }
                return;
            }
        }
        if let Err(e) = self.reload() {
            self.status_msg = format!("reload after done failed: {}", e);
        }
    }

    fn action_cycle_priority(&mut self) {
        let Some(task) = self.selected_task().cloned() else {
            return;
        };
        // Cycle: low(1) → normal(2) → high(3) → urgent(4) → critical(5) → low(1)
        let next = if task.priority >= 5 {
            1
        } else {
            task.priority + 1
        };
        match tasks::update_priority(&self.db, &task.id, next, &tui_ctx()) {
            Ok(_) => {
                self.status_msg = format!(
                    "{} → priority {} ({})",
                    task.pt_id.as_deref().unwrap_or("?"),
                    next,
                    ptask_core::priority::label(next)
                );
                if let Err(e) = self.reload() {
                    self.status_msg = format!("priority ok; reload failed: {}", e);
                }
            }
            Err(e) => self.status_msg = format!("priority failed: {}", e),
        }
    }

    fn action_delete_prompt(&mut self) {
        let Some(task) = self.selected_task().cloned() else {
            return;
        };
        let pt = task.pt_id.clone().unwrap_or_else(|| "?".into());
        self.confirm = Some(Confirm::Delete {
            task_uuid: task.id.clone(),
            title: task.title.clone(),
        });
        self.status_msg = format!("delete {}? y/n", pt);
    }

    fn action_create(&mut self, buf: &str) {
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            self.status_msg = "create: empty input".into();
            return;
        }
        let q = match ptask_core::quickadd::parse(trimmed) {
            Ok(q) => q,
            Err(e) => {
                self.status_msg = format!("create failed: {}", e);
                return;
            }
        };
        let (new, ext) = q.task_parts("tui");
        match tasks::create_with_extensions(&self.db, new, ext, &tui_ctx()) {
            Ok(t) => {
                self.status_msg =
                    format!("created {}: {}", t.pt_id.as_deref().unwrap_or("?"), t.title);
                if let Err(e) = self.reload() {
                    self.status_msg = format!("create ok; reload failed: {}", e);
                }
            }
            Err(e) => self.status_msg = format!("create failed: {}", e),
        }
    }

    fn move_cursor(&mut self, delta: i64) {
        if self.filtered.is_empty() {
            return;
        }
        let cur = self.list_state.selected().unwrap_or(0) as i64;
        let max = (self.filtered.len() - 1) as i64;
        let next = (cur + delta).clamp(0, max) as usize;
        self.list_state.select(Some(next));
    }

    fn cursor_to(&mut self, idx: usize) {
        if self.filtered.is_empty() {
            return;
        }
        self.list_state
            .select(Some(idx.min(self.filtered.len() - 1)));
    }

    fn cursor_to_last(&mut self) {
        self.cursor_to(self.filtered.len().saturating_sub(1));
    }

    fn half_page(&self) -> i64 {
        (self.viewport_rows / 2).max(1) as i64
    }
}

/// Pasted text for a one-line input: each line break (CRLF, CR or LF) and
/// tab becomes a space; other control characters are dropped.
fn paste_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(' ');
            }
            '\n' | '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptask_core::NewTask;
    use ptask_core::event_log::EventCtx;

    fn fresh_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (
                    id               TEXT PRIMARY KEY,
                    title            TEXT NOT NULL,
                    description      TEXT DEFAULT '',
                    priority         INTEGER DEFAULT 2,
                    status           TEXT DEFAULT 'pending',
                    created_at       TEXT NOT NULL,
                    updated_at       TEXT NOT NULL,
                    deadline         TEXT,
                    source_type      TEXT DEFAULT 'manual',
                    source_files     TEXT DEFAULT '[]',
                    ai_confidence    REAL DEFAULT 1.0,
                    ai_reasoning     TEXT DEFAULT '',
                    depends_on       TEXT DEFAULT '[]',
                    blocks_tasks     TEXT DEFAULT '[]',
                    escalation_level INTEGER DEFAULT 0,
                    dismissal_count  INTEGER DEFAULT 0,
                    last_reminded    TEXT,
                    next_reminder    TEXT,
                    priority_score   REAL DEFAULT 0.0,
                    score_urgency    REAL DEFAULT 0.0,
                    score_dependency REAL DEFAULT 0.0,
                    score_neglect    REAL DEFAULT 0.0,
                    subtasks         TEXT DEFAULT '[]',
                    task_type        TEXT DEFAULT 'operational',
                    cluster_keywords TEXT DEFAULT '[]'
                );
                CREATE TABLE interactions (
                    id      INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    action  TEXT NOT NULL,
                    ts      TEXT NOT NULL,
                    details TEXT DEFAULT ''
                );",
            )
            .unwrap();
        }
        (dir, Db::open(&path).unwrap())
    }

    #[test]
    fn reload_keeps_no_selection_when_filter_matches_nothing() {
        let (_dir, db) = fresh_db();
        ptask_core::tasks::create(&db, NewTask::minimal("alpha task"), &EventCtx::test()).unwrap();

        let mut app = App::new(db).unwrap();
        app.filter_query = "zzzz-no-match".into();
        app.apply_filter();
        assert!(app.filtered.is_empty());
        assert_eq!(app.list_state.selected(), None);

        app.reload().unwrap();
        assert!(app.filtered.is_empty());
        assert_eq!(app.list_state.selected(), None);
        assert_eq!(app.selected_task_index(), None);
    }

    #[test]
    fn rendered_frame_carries_no_bidi_or_invisible_characters() {
        let hostile = "pay \u{202e}lve\u{2066}x\u{2069} zw\u{200b}j\u{2060} tag\u{e0041} bom\u{feff} khmer\u{17b4}\u{17b5} fff\u{fff1} un\u{e0239}";
        let (dir, db) = fresh_db();
        let ctx = EventCtx::test();
        let mut new = NewTask::minimal(format!("title {hostile}"));
        new.description = format!("desc {hostile}\nline two {hostile}");
        new.ai_reasoning = format!("why {hostile}");
        ptask_core::tasks::create(&db, new, &ctx).unwrap();
        views::create(&db, &format!("v{hostile}"), "search: title").unwrap();
        let mut app = App::new(db).unwrap();
        app.action_cycle_view();
        app.peek_open = true;
        app.status_msg = format!("done: {hostile}");
        app.filter_input = Some(format!("f {hostile}"));
        app.prompt = Some(Prompt::Create {
            buf: format!("p {hostile}"),
        });

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 40)).unwrap();
        terminal.draw(|f| crate::ui::render(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("title pay"), "the task must be on screen");
        for c in text.chars() {
            assert!(
                !matches!(
                    c,
                    '\u{202A}'..='\u{202E}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{200B}'..='\u{200F}'
                        | '\u{2060}'..='\u{206F}'
                        | '\u{FEFF}'
                        | '\u{E0000}'..='\u{E0FFF}'
                        | '\u{17B4}'..='\u{17B5}'
                        | '\u{FFF0}'..='\u{FFF8}'
                ),
                "{c:?} reached the frame:\n{text}"
            );
        }
        drop(dir);
    }

    /// (pt_id, status, priority, title) of every task, any status.
    fn snapshot(db: &Db) -> Vec<(Option<String>, String, i64, String)> {
        ptask_core::tasks::list_with_filter(db, None, None, None, 100)
            .unwrap()
            .into_iter()
            .map(|t| (t.pt_id, t.status, t.priority, t.title))
            .collect()
    }

    fn app_with(titles: &[&str]) -> (tempfile::TempDir, App) {
        let (dir, db) = fresh_db();
        for t in titles {
            ptask_core::tasks::create(&db, NewTask::minimal(*t), &EventCtx::test()).unwrap();
        }
        (dir, App::new(db).unwrap())
    }

    #[test]
    fn paste_into_the_prompt_is_text_not_keystrokes() {
        let (_dir, mut app) = app_with(&["Renew TLS cert", "Book dentist"]);
        let before = snapshot(&app.db);
        app.prompt = Some(Prompt::Create { buf: String::new() });
        app.handle(Event::Paste("a\rGood deal".into()));
        assert_eq!(app.prompt.as_ref().map(Prompt::buf), Some("a Good deal"));
        // CRLF and LF fold to one space each; other controls are dropped.
        app.handle(Event::Paste(", ping\r\nme\n\x1b[2J".into()));
        assert_eq!(
            app.prompt.as_ref().map(Prompt::buf),
            Some("a Good deal, ping me [2J")
        );
        assert_eq!(snapshot(&app.db), before, "a paste must not touch any task");
        assert!(!app.quit);
    }

    #[test]
    fn paste_into_the_filter_bar_flattens_newlines() {
        let (_dir, mut app) = app_with(&["alpha task", "beta task"]);
        app.filter_input = Some(String::new());
        app.handle(Event::Paste("alp\r\nha".into()));
        assert_eq!(app.filter_input.as_deref(), Some("alp ha"));
        assert_eq!(app.filter_query, "alp ha");
    }

    #[test]
    fn paste_with_no_input_open_fires_no_action() {
        let (_dir, mut app) = app_with(&["Renew TLS cert", "Book dentist"]);
        let before = snapshot(&app.db);
        // d = done, p = priority, c = create, q = quit, Enter: none may fire.
        app.handle(Event::Paste("dpcq\r".into()));
        assert!(app.prompt.is_none() && app.filter_input.is_none() && !app.quit);
        // Nor may a pasted "y" answer a pending delete confirmation.
        let target = app.selected_task().cloned().unwrap();
        app.confirm = Some(Confirm::Delete {
            task_uuid: target.id.clone(),
            title: target.title.clone(),
        });
        app.handle(Event::Paste("y".into()));
        assert_eq!(snapshot(&app.db), before);
    }

    #[test]
    fn saved_view_shows_open_tasks_not_done_ones() {
        let (_dir, db) = fresh_db();
        let ctx = EventCtx::test();
        for i in 0..22 {
            let t =
                ptask_core::tasks::create(&db, NewTask::minimal(format!("done task {i}")), &ctx)
                    .unwrap();
            ptask_core::tasks::mark_done(&db, &t, &ctx).unwrap();
        }
        let gone =
            ptask_core::tasks::create(&db, NewTask::minimal("dismissed task"), &ctx).unwrap();
        ptask_core::tasks::dismiss(&db, &gone.id, &ctx).unwrap();
        ptask_core::tasks::create(&db, NewTask::minimal("open task"), &ctx).unwrap();
        views::create(&db, "everything", "search: task").unwrap();

        let mut app = App::new(db).unwrap();
        app.action_cycle_view();
        assert_eq!(app.view.label(), "view:everything");
        let titles: Vec<&str> = app.tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["open task"]);
    }

    #[test]
    fn a_failed_done_reloads_so_the_next_one_works() {
        // Regression (round 2, item 4i): after another surface advanced a
        // recurring task, the TUI's `done` failed on its stale row and kept
        // failing until a manual `r`.
        let (_dir, db) = fresh_db();
        let mut new = NewTask::minimal("daily");
        new.deadline = Some("2099-01-01".into());
        let ext = ptask_core::Extensions {
            recurrence: Some(ptask_core::recurrence::parse("every day").unwrap()),
            ..Default::default()
        };
        let t =
            ptask_core::tasks::create_with_extensions(&db, new, ext, &EventCtx::test()).unwrap();
        let mut app = App::new(db.clone()).unwrap();
        app.list_state.select(Some(0));
        // Another surface completes the occurrence the TUI is showing.
        ptask_core::tasks::mark_done(&db, &t, &EventCtx::test()).unwrap();

        app.action_done();
        assert!(
            app.status_msg.starts_with("done failed"),
            "{}",
            app.status_msg
        );
        app.action_done();
        assert!(app.status_msg.starts_with("advanced"), "{}", app.status_msg);
    }

    #[test]
    fn initial_load_covers_phase_three_task_volume() {
        let (_dir, db) = fresh_db();
        for i in 0..205 {
            ptask_core::tasks::create(
                &db,
                NewTask::minimal(format!("task {i:03}")),
                &EventCtx::test(),
            )
            .unwrap();
        }

        let app = App::new(db).unwrap();
        assert_eq!(app.tasks.len(), 205);
        assert_eq!(app.filtered.len(), 205);
    }
}
