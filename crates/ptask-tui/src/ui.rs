//! Rendering — pure functions of [`App`] state.

use crate::app::App;
use ptask_core::priority;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(0),    // list
            Constraint::Length(1), // status bar
        ])
        .split(area);

    // Capture the list viewport height (minus the two border rows) so
    // PageDown/Up + Ctrl-d/u scale correctly.
    app.viewport_rows = chunks[1].height.saturating_sub(2);

    render_header(frame, chunks[0], app);
    render_list(frame, chunks[1], app);
    render_status(frame, chunks[2], app);
}

fn render_header(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let title = Line::from(vec![
        Span::styled(
            "pt ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("v{}  ", ptask_core::VERSION)),
        Span::styled(
            format!("{} pending", app.tasks.len()),
            Style::default().fg(Color::Yellow),
        ),
    ]);
    frame.render_widget(Paragraph::new(title), area);
}

fn render_list(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .tasks
        .iter()
        .map(|t| {
            let pt = t.pt_id.as_deref().unwrap_or("------");
            let label = priority::label(t.priority).to_ascii_uppercase();
            let prio_color = match t.priority {
                5 => Color::Red,
                4 => Color::LightRed,
                3 => Color::Yellow,
                1 => Color::DarkGray,
                _ => Color::White,
            };
            let line = Line::from(vec![
                Span::styled(format!("[{:8}] ", label), Style::default().fg(prio_color)),
                Span::styled(format!("{:7} ", pt), Style::default().fg(Color::Cyan)),
                Span::raw(t.title.clone()),
            ]);
            ListItem::new(line)
        })
        .collect();
    let title = match app.list_state.selected() {
        Some(i) => format!(" pending  {}/{} ", i + 1, app.tasks.len()),
        None => " pending ".into(),
    };
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .title(title);
    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_status(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let line = Line::from(vec![
        keybind("q"),
        Span::raw(" quit  "),
        keybind("j/k"),
        Span::raw(" move  "),
        keybind("gg/G"),
        Span::raw(" top/bot  "),
        keybind("^d/^u"),
        Span::raw(" page  "),
        keybind("r"),
        Span::raw(" reload  "),
        Span::raw(" | "),
        Span::raw(app.status_msg.clone()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn keybind(s: &str) -> Span<'static> {
    Span::styled(s.to_string(), Style::default().fg(Color::Cyan))
}
