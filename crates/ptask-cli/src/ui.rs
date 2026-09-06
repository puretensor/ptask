//! ui — the PureTensor terminal look for `pt`.
//!
//! A Rust port of `tensor-scripts/lib/ptui.py`, the theme every fleet tool
//! (`fleet-upgrade`, `fleet-health`, …) renders through, so `pt list` reads as
//! the same product as `fleet-upgrade --status`. The design rules are the
//! same and deliberately narrow:
//!
//!   * ONE accent ramp — cyan → violet → magenta — and it appears only in the
//!     header rules and the table bands. Nowhere else.
//!   * Everything else is semantic, not decorative: green means done/ok, amber
//!     means needs a human, red means broken or critical, slate means
//!     "context, don't read this first".
//!   * Structure carries the meaning — box rules, bands, aligned columns.
//!     Colour is the second signal, never the only one, so the output
//!     survives `--no-color`, `NO_COLOR`, and a pipe.
//!   * No emoji, no ASCII art, no boxes drawn around boxes.
//!
//! Colour is decided once at startup (`init`) and read through `enabled()`;
//! every painter is a no-op when it is off, so callers never branch on it.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::sync::OnceLock;
use unicode_width::UnicodeWidthStr;

// ── Palette ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    Cyan,
    Violet,
    Magenta,
    Green,
    Amber,
    Red,
    Slate,
    Paper,
    Steel,
}

impl Ink {
    pub const fn rgb(self) -> (u8, u8, u8) {
        match self {
            Ink::Cyan => (56, 232, 255),
            Ink::Violet => (160, 107, 255),
            Ink::Magenta => (255, 94, 196),
            Ink::Green => (52, 224, 122),
            Ink::Amber => (255, 200, 87),
            Ink::Red => (255, 95, 86),
            Ink::Slate => (107, 115, 148),
            Ink::Paper => (238, 242, 255),
            Ink::Steel => (150, 160, 195),
        }
    }
}

/// The accent ramp. Header rules run it forward on the top rule and reversed
/// on the bottom one, exactly like ptui.
pub const ACCENT: [Ink; 3] = [Ink::Cyan, Ink::Violet, Ink::Magenta];

// ── Colour switch ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// Colour when stdout is a terminal, `NO_COLOR` is unset and `TERM` is
    /// not `dumb`. `PT_COLOR=always|never` overrides the terminal check.
    Auto,
    Always,
    Never,
}

static COLOR: OnceLock<bool> = OnceLock::new();

pub fn init(mode: ColorMode) {
    let on = match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => match std::env::var("PT_COLOR").as_deref() {
            Ok("always") => true,
            Ok("never") => false,
            _ => {
                std::io::stdout().is_terminal()
                    && std::env::var_os("NO_COLOR").is_none()
                    && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
            }
        },
    };
    let _ = COLOR.set(on);
}

pub fn enabled() -> bool {
    *COLOR.get().unwrap_or(&false)
}

// ── Painters ──────────────────────────────────────────────────────────────────

fn sgr(text: &str, (r, g, b): (u8, u8, u8), bold: bool, dim: bool) -> String {
    if !enabled() || text.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 24);
    if bold {
        out.push_str("\x1b[1m");
    }
    if dim {
        out.push_str("\x1b[2m");
    }
    out.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
    out.push_str(text);
    out.push_str("\x1b[0m");
    out
}

pub fn paint(text: &str, ink: Ink) -> String {
    sgr(text, ink.rgb(), false, false)
}

pub fn bold(text: &str, ink: Ink) -> String {
    sgr(text, ink.rgb(), true, false)
}

pub fn dim(text: &str, ink: Ink) -> String {
    sgr(text, ink.rgb(), false, true)
}

fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (ch(a.0, b.0), ch(a.1, b.1), ch(a.2, b.2))
}

/// Per-character colour ramp across two or more palette stops.
pub fn gradient(text: &str, stops: &[Ink]) -> String {
    if !enabled() || text.is_empty() {
        return text.to_string();
    }
    let cols: Vec<(u8, u8, u8)> = if stops.is_empty() {
        ACCENT.iter().map(|i| i.rgb()).collect()
    } else {
        stops.iter().map(|i| i.rgb()).collect()
    };
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len().saturating_sub(1).max(1) as f32;
    let mut out = String::with_capacity(text.len() * 20);
    for (i, ch) in chars.iter().enumerate() {
        let pos = (i as f32 / n) * (cols.len() - 1) as f32;
        let lo = (pos.floor() as usize).min(cols.len().saturating_sub(2));
        let col = if cols.len() > 1 {
            mix(cols[lo], cols[lo + 1], pos - lo as f32)
        } else {
            cols[0]
        };
        out.push_str(&sgr(&ch.to_string(), col, false, false));
    }
    out
}

// ── Measurement and layout ────────────────────────────────────────────────────

/// Strip SGR sequences. Only the `ESC [ … m`-style CSI forms this module emits.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Display width of a string, ANSI-blind and wide-glyph aware.
pub fn vis_len(text: &str) -> usize {
    strip_ansi(text).width()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Right,
}

pub fn pad(text: &str, width: usize, align: Align) -> String {
    let gap = width.saturating_sub(vis_len(text));
    match align {
        Align::Left => format!("{text}{}", " ".repeat(gap)),
        Align::Right => format!("{}{text}", " ".repeat(gap)),
    }
}

/// Clip to a display width, ellipsised. Returns PLAIN text — clip before you paint.
pub fn clip(text: &str, width: usize) -> String {
    let plain = strip_ansi(text);
    if plain.width() <= width {
        return plain;
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in plain.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > width - 1 {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// Terminal width, clamped so a tiny window still gets a readable board and a
/// cinema display does not stretch the table across it.
pub fn term_width() -> usize {
    const DEFAULT: usize = 108;
    const MIN: usize = 84;
    const MAX: usize = 150;
    let cols = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .or_else(|| crossterm::terminal::size().ok().map(|(c, _)| c as usize))
        .unwrap_or(DEFAULT);
    cols.clamp(MIN, MAX)
}

pub fn utc_stamp() -> String {
    ptask_core::jiff::Timestamp::now()
        .strftime("%Y-%m-%d %H:%M UTC")
        .to_string()
}

// ── Semantic vocabulary ───────────────────────────────────────────────────────
// The six words every fleet tool speaks, so a green mark means the same thing
// in `pt` as it does in `fleet-upgrade`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Changed,
    Warn,
    Bad,
    Busy,
    Mute,
}

impl Status {
    pub const fn ink(self) -> Ink {
        match self {
            Status::Ok => Ink::Green,
            Status::Changed => Ink::Cyan,
            Status::Warn => Ink::Amber,
            Status::Bad => Ink::Red,
            Status::Busy => Ink::Amber,
            Status::Mute => Ink::Slate,
        }
    }
    pub const fn glyph(self) -> &'static str {
        match self {
            Status::Ok | Status::Changed => "✔",
            Status::Warn => "▲",
            Status::Bad => "✖",
            Status::Busy => "⟲",
            Status::Mute => "·",
        }
    }
}

pub fn pill(status: Status, label: &str) -> String {
    bold(&format!("{} {}", status.glyph(), label), status.ink())
}

/// Task priority → ink. Red and amber keep their fleet meaning (broken / needs
/// a human); the rest step down the slate scale so the eye lands on the top.
pub fn priority_ink(priority: i64) -> Ink {
    match priority {
        5 => Ink::Red,
        4 => Ink::Magenta,
        3 => Ink::Amber,
        2 => Ink::Steel,
        _ => Ink::Slate,
    }
}

/// `● CRITICAL`, `● high`, … — the priority column and the show-page badge.
pub fn priority_pill(priority: i64) -> String {
    let label = ptask_core::priority::label(priority);
    let ink = priority_ink(priority);
    if priority >= 4 {
        bold(&format!("● {}", label.to_ascii_uppercase()), ink)
    } else {
        paint(&format!("● {label}"), ink)
    }
}

/// Task status → (glyph, ink). Same vocabulary as the fleet pills: done is
/// green, blocked is amber, in-progress is the busy spinner glyph.
pub fn task_status_style(status: &str) -> (&'static str, Ink) {
    match status {
        "done" => ("✔", Ink::Green),
        "dismissed" => ("–", Ink::Slate),
        "blocked" => ("▲", Ink::Amber),
        "in_progress" | "in-progress" | "doing" => ("⟲", Ink::Cyan),
        "snoozed" | "delayed" => ("◷", Ink::Slate),
        "triage" => ("?", Ink::Violet),
        "backlog" => ("·", Ink::Slate),
        _ => ("·", Ink::Steel),
    }
}

pub fn status_pill(status: &str) -> String {
    let (glyph, ink) = task_status_style(status);
    paint(&format!("{glyph} {}", status.replace('_', " ")), ink)
}

pub fn pt_id(id: &str) -> String {
    bold(id, Ink::Cyan)
}

// ── Building blocks ───────────────────────────────────────────────────────────

pub fn rule(width: usize, reverse: bool) -> String {
    let stops: Vec<Ink> = if reverse {
        ACCENT.iter().rev().copied().collect()
    } else {
        ACCENT.to_vec()
    };
    gradient(&"━".repeat(width), &stops)
}

/// Section header: gradient rule, `TITLE  BADGE   note`, reversed rule.
pub fn headline(text: &str, badge: Option<(&str, Ink)>, note: &str) -> Vec<String> {
    let width = term_width();
    let mut line = format!("  {}", gradient(&text.to_ascii_uppercase(), &[]));
    if let Some((b, ink)) = badge {
        line.push_str("  ");
        line.push_str(&bold(&format!(" {} ", b.to_ascii_uppercase()), ink));
    }
    if !note.is_empty() {
        line.push_str(&paint(&format!("   {note}"), Ink::Slate));
    }
    vec![rule(width, false), line, rule(width, true), String::new()]
}

/// Boxed banner: the one gradient frame, for the no-args splash.
pub fn banner(title: &str, subtitle: &str, badge: Option<(&str, Ink)>) -> Vec<String> {
    let width = term_width();
    let mut head = gradient(&title.to_ascii_uppercase(), &[]);
    if let Some((b, ink)) = badge {
        head.push_str("   ");
        head.push_str(&bold(&format!(" {} ", b.to_ascii_uppercase()), ink));
    }
    let mut lines = vec![gradient(&format!("╭{}╮", "─".repeat(width - 2)), &[])];
    lines.push(format!("│ {} │", pad(&head, width - 4, Align::Left)));
    if !subtitle.is_empty() {
        lines.push(format!(
            "│ {} │",
            pad(&paint(subtitle, Ink::Slate), width - 4, Align::Left)
        ));
    }
    let rev: Vec<Ink> = ACCENT.iter().rev().copied().collect();
    lines.push(gradient(&format!("╰{}╯", "─".repeat(width - 2)), &rev));
    lines
}

#[derive(Debug, Clone)]
pub struct Column {
    pub title: &'static str,
    pub width: usize,
    pub align: Align,
}

impl Column {
    pub const fn new(title: &'static str, width: usize) -> Self {
        Column {
            title,
            width,
            align: Align::Left,
        }
    }
    pub const fn right(title: &'static str, width: usize) -> Self {
        Column {
            title,
            width,
            align: Align::Right,
        }
    }
}

/// Width the box will occupy including the two-space indent.
pub fn table_width(columns: &[Column]) -> usize {
    columns.iter().map(|c| c.width + 2).sum::<usize>() + columns.len() + 1 + 2
}

/// Box-ruled table. `bands` maps a row index to a band label drawn above that
/// row (the severity tiers in `pt list`, the way fleet-upgrade bands nodes by
/// tier). Cells may be pre-painted; they are clipped to the column width.
pub fn table(
    columns: &[Column],
    rows: &[Vec<String>],
    bands: &BTreeMap<usize, String>,
) -> Vec<String> {
    let indent = "  ";
    let inner: usize = columns.iter().map(|c| c.width + 2).sum::<usize>() + columns.len() - 1;
    let line = |s: &str| dim(s, Ink::Slate);
    let seg = |glyph: &str| -> String {
        columns
            .iter()
            .map(|c| "─".repeat(c.width + 2))
            .collect::<Vec<_>>()
            .join(glyph)
    };
    let bar = line("│");
    let mut out = Vec::with_capacity(rows.len() + 4);
    out.push(format!("{indent}{}", line(&format!("┌{}┐", seg("┬")))));
    let head: Vec<String> = columns
        .iter()
        .map(|c| {
            let title = bold(&clip(c.title, c.width), Ink::Steel);
            format!(" {} ", pad(&title, c.width, c.align))
        })
        .collect();
    out.push(format!("{indent}{bar}{}{bar}", head.join(&bar)));
    out.push(format!("{indent}{}", line(&format!("├{}┤", seg("┼")))));
    for (i, row) in rows.iter().enumerate() {
        if let Some(label) = bands.get(&i) {
            let label = format!(" {label} ");
            let fill = inner.saturating_sub(label.width() + 1);
            out.push(format!(
                "{indent}{}{}{}",
                line("├"),
                paint(&format!("─{label}{}", "─".repeat(fill)), Ink::Violet),
                line("┤")
            ));
        }
        let cells: Vec<String> = columns
            .iter()
            .enumerate()
            .map(|(j, c)| {
                let cell = row.get(j).map(String::as_str).unwrap_or("");
                let cell = if vis_len(cell) <= c.width {
                    cell.to_string()
                } else {
                    clip(cell, c.width)
                };
                format!(" {} ", pad(&cell, c.width, c.align))
            })
            .collect();
        out.push(format!("{indent}{bar}{}{bar}", cells.join(&bar)));
    }
    out.push(format!("{indent}{}", line(&format!("└{}┘", seg("┴")))));
    out
}

/// Key/value block: slate key column, paper value.
pub fn kv(pairs: &[(&str, String)], key_width: usize) -> Vec<String> {
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "  {}{}",
                pad(&paint(k, Ink::Slate), key_width, Align::Left),
                if v.contains('\x1b') {
                    v.clone()
                } else {
                    paint(v, Ink::Paper)
                }
            )
        })
        .collect()
}

/// Section line: glyph + upper-case title in the semantic ink, slate note.
pub fn section(title: &str, ink: Ink, note: &str) -> String {
    let glyph = match ink {
        Ink::Green => "✔",
        Ink::Amber => "▲",
        Ink::Red => "✖",
        Ink::Magenta => "⟳",
        _ => "•",
    };
    let mut line = format!(
        "  {}",
        bold(&format!("{glyph} {}", title.to_ascii_uppercase()), ink)
    );
    if !note.is_empty() {
        line.push_str(&paint(&format!("   {note}"), Ink::Slate));
    }
    line
}

/// `     • name   detail` — a name and its detail, name clipped to a column so
/// the detail column never drifts.
pub fn bullet(name: &str, detail: &str, ink: Ink, width: usize) -> String {
    format!(
        "     {} {} {}",
        paint("•", ink),
        pad(&paint(&clip(name, width), Ink::Paper), width, Align::Left),
        paint(detail, Ink::Slate)
    )
}

/// `  ▸ text` — the running-commentary line fleet tools print between steps.
pub fn note(text: &str) -> String {
    format!("  {}{}", paint("▸ ", Ink::Violet), paint(text, Ink::Slate))
}

/// One-line outcome of a mutation: `  ✔ done      PT-2041  title   detail`.
pub fn outcome(status: Status, verb: &str, id: &str, title: &str, detail: &str) -> String {
    let width = term_width();
    let head = format!(
        "  {} {} {}  ",
        bold(status.glyph(), status.ink()),
        pad(&bold(verb, status.ink()), 9, Align::Left),
        pt_id(id)
    );
    let tail = if detail.is_empty() {
        String::new()
    } else {
        format!("   {}", paint(detail, Ink::Slate))
    };
    let room = width
        .saturating_sub(vis_len(&head) + vis_len(&tail))
        .max(24);
    format!("{head}{}{tail}", paint(&clip(title, room), Ink::Paper))
}

/// Footer count line under a table: `  20 tasks shown   hint`.
pub fn footer(count: usize, noun: &str, hint: &str) -> String {
    let plural = if count == 1 {
        noun.to_string()
    } else {
        format!("{noun}s")
    };
    let mut line = format!(
        "  {} {}",
        bold(&count.to_string(), Ink::Paper),
        paint(&format!("{plural} shown"), Ink::Slate)
    );
    if !hint.is_empty() {
        line.push_str(&dim(&format!("   {hint}"), Ink::Slate));
    }
    line
}

/// Empty-state line: `  · no tasks found`.
pub fn empty(text: &str) -> String {
    format!("  {}", paint(&format!("· {text}"), Ink::Slate))
}

// ── Task table ────────────────────────────────────────────────────────────────

/// Shared list renderer for `pt list`, `pt next`, `pt view show`,
/// `pt remote list`, `pt remote next`. Bands by severity tier when `banded`.
pub fn task_table(
    tasks: &[ptask_core::Task],
    banded: bool,
    show_due: bool,
    verbose: bool,
) -> Vec<String> {
    let width = term_width();
    let mut cols = vec![
        Column::new("PRIORITY", 10),
        Column::new("ID", 7),
        Column::new("STATUS", 13),
    ];
    if show_due {
        cols.push(Column::new("DUE", 10));
    }
    let fixed = table_width(&cols) + 3;
    cols.push(Column::new("TITLE", width.saturating_sub(fixed).max(24)));
    let title_w = cols.last().map(|c| c.width).unwrap_or(24);

    let mut rows: Vec<Vec<String>> = Vec::with_capacity(tasks.len());
    let mut bands = BTreeMap::new();
    let mut last_tier: Option<i64> = None;
    for t in tasks {
        if banded && last_tier != Some(t.priority) {
            bands.insert(
                rows.len(),
                ptask_core::priority::label(t.priority).to_string(),
            );
            last_tier = Some(t.priority);
        }
        let id = t.pt_id.as_deref().unwrap_or("------");
        let mut row = vec![priority_pill(t.priority), pt_id(id), status_pill(&t.status)];
        if show_due {
            row.push(due_cell(t.deadline.as_deref()));
        }
        row.push(paint(&clip(&t.title, title_w), Ink::Paper));
        rows.push(row);
        if verbose {
            let blank = cols.len() - 1;
            if !t.description.is_empty() {
                let snippet: String = t.description.lines().next().unwrap_or("").to_string();
                let mut r = vec![String::new(); blank];
                r.push(paint(&clip(&snippet, title_w), Ink::Slate));
                rows.push(r);
            }
            let mut r = vec![String::new(); blank];
            let mut meta = format!("uuid {}", t.id);
            if let Some(d) = &t.deadline
                && !show_due
            {
                meta.push_str(&format!(" · due {d}"));
            }
            r.push(dim(&clip(&meta, title_w), Ink::Slate));
            rows.push(r);
        }
    }
    table(&cols, &rows, &bands)
}

/// Deadline cell: overdue in red, today in amber, otherwise steel; `--` in slate.
pub fn due_cell(deadline: Option<&str>) -> String {
    let Some(d) = deadline else {
        return dim("--", Ink::Slate);
    };
    let day = d.get(..10).unwrap_or(d);
    let today = ptask_core::jiff::Zoned::now().date().to_string();
    let ink = match day.cmp(today.as_str()) {
        std::cmp::Ordering::Less => Ink::Red,
        std::cmp::Ordering::Equal => Ink::Amber,
        std::cmp::Ordering::Greater => Ink::Steel,
    };
    paint(day, ink)
}

/// Word-wrap plain text to a display width, indenting continuation lines.
pub fn wrap(text: &str, width: usize, indent: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let ww = word.width();
        if !cur.is_empty() && cur.width() + 1 + ww > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| if i == 0 { l } else { format!("{indent}{l}") })
        .collect()
}

/// Prompt line for interactive loops: `  ▸ question  [k]eep [d]one …  > `.
pub fn prompt(text: &str, keys: &str) -> String {
    format!(
        "  {}{}  {} {} ",
        paint("▸ ", Ink::Violet),
        paint(text, Ink::Paper),
        dim(keys, Ink::Slate),
        bold(">", Ink::Cyan)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_only_sgr() {
        let s = "\x1b[1m\x1b[38;2;1;2;3mhi\x1b[0m there";
        assert_eq!(strip_ansi(s), "hi there");
        assert_eq!(vis_len(s), 8);
    }

    #[test]
    fn pad_and_clip_are_width_aware() {
        assert_eq!(pad("ab", 5, Align::Left), "ab   ");
        assert_eq!(pad("ab", 5, Align::Right), "   ab");
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("abc", 4), "abc");
        // A wide glyph counts double so the box stays square.
        assert_eq!(vis_len("日本"), 4);
        assert_eq!(clip("日本語", 4), "日…");
    }

    #[test]
    fn painters_are_plain_when_colour_is_off() {
        // COLOR is unset in tests → enabled() is false.
        assert_eq!(paint("x", Ink::Red), "x");
        assert_eq!(gradient("rule", &[]), "rule");
        assert_eq!(pill(Status::Ok, "done"), "✔ done");
        assert_eq!(priority_pill(5), "● CRITICAL");
        assert_eq!(priority_pill(2), "● normal");
        assert_eq!(status_pill("in_progress"), "⟲ in progress");
    }

    #[test]
    fn table_is_square_and_banded() {
        let cols = [Column::new("A", 3), Column::right("B", 4)];
        let mut bands = BTreeMap::new();
        bands.insert(0usize, "tier".to_string());
        let rows = vec![
            vec!["x".into(), "1".into()],
            vec!["toolong".into(), "22".into()],
        ];
        let out = table(&cols, &rows, &bands);
        // top rule, header, header rule, band, two rows, bottom rule
        assert_eq!(out.len(), 7);
        let w = vis_len(&out[0]);
        assert!(out.iter().all(|l| vis_len(l) == w), "{out:?}");
        assert_eq!(out[0], "  ┌─────┬──────┐");
        assert_eq!(out[3], "  ├─ tier ─────┤");
        assert!(out[4].contains("│ x   │    1 │"));
        assert!(out[5].contains("│ to… │   22 │"));
        assert_eq!(out[6], "  └─────┴──────┘");
    }

    #[test]
    fn task_table_bands_by_severity() {
        let mk = |p: i64, id: &str| ptask_core::Task {
            id: format!("uuid-{id}"),
            pt_id: Some(id.to_string()),
            title: "t".into(),
            description: String::new(),
            priority: p,
            status: "todo".into(),
            created_at: String::new(),
            updated_at: String::new(),
            deadline: None,
            source_type: "cli".into(),
            ai_reasoning: String::new(),
            kind: "ship".into(),
            deliverable: None,
        };
        let tasks = vec![mk(5, "PT-1"), mk(5, "PT-2"), mk(3, "PT-3")];
        let out = task_table(&tasks, true, true, false).join("\n");
        assert!(out.contains("─ critical "));
        assert!(out.contains("─ high "));
        assert_eq!(out.matches("─ critical ").count(), 1);
        assert!(out.contains("PT-3"));
        let unbanded = task_table(&tasks, false, true, false).join("\n");
        assert!(!unbanded.contains("─ critical "));
    }

    #[test]
    fn wrap_breaks_on_words_and_indents() {
        let w = wrap("alpha beta gamma delta", 11, "  ");
        assert_eq!(w, vec!["alpha beta", "  gamma delta"]);
        assert_eq!(wrap("", 10, ""), vec![""]);
    }

    #[test]
    fn outcome_line_reads_verb_id_title() {
        let l = outcome(Status::Ok, "done", "PT-9", "ship it", "rescored 3");
        assert_eq!(l, "  ✔ done      PT-9  ship it   rescored 3");
    }
}
