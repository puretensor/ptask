//! Rendering — pure functions of [`App`] state.

use crate::app::App;
use ptask_core::priority;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

/// Untrusted text for a span: ratatui drops control characters but passes
/// bidi controls and invisible/format characters straight to the terminal,
/// so every task, goal, view and status string goes through the shared
/// sanitiser (line breaks fold to a visible mark; spans are one line).
fn safe(s: &str) -> String {
    ptask_core::text::one_line(s).into_owned()
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let filter_bar_h: u16 = if app.filter_input.is_some() { 1 } else { 0 };
    let prompt_bar_h: u16 = if app.prompt.is_some() { 1 } else { 0 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),            // header
            Constraint::Min(0),               // body
            Constraint::Length(filter_bar_h), // filter input bar
            Constraint::Length(prompt_bar_h), // prompt bar
            Constraint::Length(1),            // status bar
        ])
        .split(area);

    render_header(frame, chunks[0], app);

    if app.peek_open {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(chunks[1]);
        app.viewport_rows = split[0].height.saturating_sub(2);
        render_list(frame, split[0], app);
        render_peek(frame, split[1], app);
    } else {
        app.viewport_rows = chunks[1].height.saturating_sub(2);
        render_list(frame, chunks[1], app);
    }

    if filter_bar_h > 0 {
        render_filter_bar(frame, chunks[2], app);
    }
    if prompt_bar_h > 0 {
        render_prompt_bar(frame, chunks[3], app);
    }
    render_status(frame, chunks[4], app);
}

fn render_prompt_bar(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let Some(p) = app.prompt.as_ref() else {
        return;
    };
    let line = Line::from(vec![
        Span::styled(
            format!("{}> ", p.label()),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(safe(p.buf())),
        Span::styled("_", Style::default().fg(Color::Magenta)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_filter_bar(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let buf = app.filter_input.as_deref().unwrap_or("");
    let line = Line::from(vec![
        Span::styled(
            "/",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(safe(buf)),
        Span::styled("_", Style::default().fg(Color::Cyan)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_peek(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let task = app.selected_task();
    let block = Block::default().borders(Borders::ALL).title(" detail ");
    let mut lines: Vec<Line> = Vec::new();
    let Some(task) = task else {
        lines.push(Line::from("(no selection)"));
        frame.render_widget(Paragraph::new(lines).block(block), area);
        return;
    };
    let pt = task.pt_id.as_deref().unwrap_or("------");
    lines.push(Line::from(vec![
        Span::styled(
            safe(pt),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::raw(safe(&task.title)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("status   ", Style::default().fg(Color::DarkGray)),
        Span::raw(safe(&task.status)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("priority ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(
            "{} ({})",
            task.priority,
            priority::label(task.priority)
        )),
    ]));
    if let Some(d) = &task.deadline {
        lines.push(Line::from(vec![
            Span::styled("deadline ", Style::default().fg(Color::DarkGray)),
            Span::raw(safe(d)),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("source   ", Style::default().fg(Color::DarkGray)),
        Span::raw(safe(&task.source_type)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("uuid     ", Style::default().fg(Color::DarkGray)),
        Span::raw(&task.id),
    ]));

    if let Some(detail) = &app.peek_detail {
        if !detail.labels.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("labels   ", Style::default().fg(Color::DarkGray)),
                Span::raw(safe(&detail.labels.join(", "))),
            ]));
        }
        if let Some(p) = &detail.project {
            lines.push(Line::from(vec![
                Span::styled("project  ", Style::default().fg(Color::DarkGray)),
                Span::raw(safe(p)),
            ]));
        }
        if let Some(d) = detail.duration_min {
            lines.push(Line::from(vec![
                Span::styled("duration ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{}m", d)),
            ]));
        }
        if let (Some(input), Some(mode), Some(next)) = (
            &detail.recurrence_input,
            &detail.recurrence_mode,
            &detail.recurrence_next,
        ) {
            lines.push(Line::from(vec![
                Span::styled("recurs   ", Style::default().fg(Color::DarkGray)),
                Span::raw(safe(&format!("{} [{}]", input, mode))),
            ]));
            lines.push(Line::from(vec![
                Span::styled("next     ", Style::default().fg(Color::DarkGray)),
                Span::raw(safe(next)),
            ]));
        }
        // depends_on = this task's prerequisites (it is blocked BY them);
        // blocks_tasks = tasks waiting on this one. The labels were swapped.
        if !detail.depends_on.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("blocked  ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("by {} prerequisite(s)", detail.depends_on.len())),
            ]));
        }
        if !detail.blocks_tasks.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("blocks   ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{} downstream", detail.blocks_tasks.len())),
            ]));
        }
    }

    if !task.description.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "description",
            Style::default().fg(Color::DarkGray),
        )));
        for paragraph in task.description.split('\n') {
            lines.push(Line::from(safe(paragraph)));
        }
    }
    if !task.ai_reasoning.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "reasoning",
            Style::default().fg(Color::DarkGray),
        )));
        for paragraph in task.ai_reasoning.split('\n') {
            lines.push(Line::from(safe(paragraph)));
        }
    }
    // Notes and closure evidence, newest last (the pane scrolls with the
    // detail block, so the latest sits nearest the description above).
    if let Some(detail) = &app.peek_detail
        && !detail.notes.is_empty()
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("notes ({})", detail.notes.len()),
            Style::default().fg(Color::DarkGray),
        )));
        for n in &detail.notes {
            let when = n.ts.get(..16).unwrap_or(&n.ts).replace('T', " ");
            let kind = if n.kind == "note" {
                String::new()
            } else {
                format!(" · {}", n.kind)
            };
            lines.push(Line::from(vec![
                Span::styled(when, Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(
                    safe(n.actor.as_deref().unwrap_or("-")),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(safe(&kind), Style::default().fg(Color::Yellow)),
            ]));
            for paragraph in n.text.split('\n') {
                lines.push(Line::from(format!("  {}", safe(paragraph))));
            }
        }
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
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
            safe(&app.view.label()),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{} task(s)", app.tasks.len()),
            Style::default().fg(Color::Yellow),
        ),
    ]);
    frame.render_widget(Paragraph::new(title), area);
}

fn render_list(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .visible()
        .iter()
        .filter_map(|&i| app.tasks.get(i))
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
                Span::styled(format!("{:7} ", safe(pt)), Style::default().fg(Color::Cyan)),
                Span::raw(safe(&t.title)),
            ]);
            ListItem::new(line)
        })
        .collect();
    let total = app.tasks.len();
    let visible = app.visible().len();
    let view = safe(&app.view.label());
    let title = match app.list_state.selected() {
        Some(i) if visible > 0 => {
            if visible == total {
                format!(" {view}  {}/{} ", i + 1, total)
            } else {
                format!(" {view}  {}/{} ({} total) ", i + 1, visible, total)
            }
        }
        _ => format!(" {view}  ({}/{}) ", visible, total),
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
        keybind("Space"),
        Span::raw(" peek  "),
        keybind("/"),
        Span::raw(" filter  "),
        keybind("d"),
        Span::raw(" done  "),
        keybind("p"),
        Span::raw(" prio  "),
        keybind("c"),
        Span::raw(" new  "),
        keybind("Del"),
        Span::raw(" rm  "),
        keybind("gv"),
        Span::raw(" view  "),
        Span::raw(" | "),
        Span::raw(safe(&app.status_msg)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn keybind(s: &str) -> Span<'static> {
    Span::styled(s.to_string(), Style::default().fg(Color::Cyan))
}
