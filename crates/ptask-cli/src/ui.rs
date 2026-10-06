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
//!
//! Task text is untrusted (fleet captures, email, Telegram, git, agents), so
//! every renderer here passes it through `sanitize` on the way out: control
//! and bidi characters become a visible U+FFFD instead of reaching the
//! terminal as an escape sequence.

use std::borrow::Cow;
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

const PALETTE: [Ink; 9] = [
    Ink::Cyan,
    Ink::Violet,
    Ink::Magenta,
    Ink::Green,
    Ink::Amber,
    Ink::Red,
    Ink::Slate,
    Ink::Paper,
    Ink::Steel,
];

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
    #[cfg(test)]
    if let Some(on) = TEST_COLOR.with(std::cell::Cell::get) {
        return on;
    }
    *COLOR.get().unwrap_or(&false)
}

// Per-thread colour override so unit tests can exercise both modes without
// racing on the process-wide switch.
#[cfg(test)]
thread_local! {
    static TEST_COLOR: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

// ── Sanitising ────────────────────────────────────────────────────────────────

/// Visible, width-1 stand-in for a character the terminal would act on.
const STAND_IN: char = '\u{FFFD}';

fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
    )
}

/// A character a terminal would act on instead of showing: every control
/// character but `\n` and `\t` (ESC, CR, BS, BEL, DEL, C1 such as U+009B
/// CSI), and the bidi overrides, isolates and marks that reorder what follows.
fn is_hazard(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t') || is_bidi_control(c)
}

/// Make untrusted text safe to print: hazardous characters become U+FFFD, a
/// tab becomes a space, and a CRLF line end becomes `\n` (it displays the
/// same, so it hides nothing). Borrows when there is nothing to change.
pub fn sanitize(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|c| c == '\t' || is_hazard(c)) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\t' => out.push(' '),
            '\r' if chars.peek() == Some(&'\n') => {}
            c if is_hazard(c) => out.push(STAND_IN),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// True when `text` holds something `sanitize` must neutralise beyond tab
/// expansion and CRLF line ends — bytes a terminal would act on, so what is
/// displayed differs from what is stored.
pub fn has_hazard(text: &str) -> bool {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() == Some(&'\n') {
            continue;
        }
        if is_hazard(c) {
            return true;
        }
    }
    false
}

/// Length of the SGR sequence at the start of `s` if it is one this module
/// emits outside gradients: reset, bold, dim, or a palette foreground.
fn own_sgr_len(s: &str) -> Option<usize> {
    for fixed in ["\x1b[0m", "\x1b[1m", "\x1b[2m"] {
        if s.starts_with(fixed) {
            return Some(fixed.len());
        }
    }
    PALETTE.iter().find_map(|ink| {
        let (r, g, b) = ink.rgb();
        let seq = format!("\x1b[38;2;{r};{g};{b}m");
        s.starts_with(&seq).then_some(seq.len())
    })
}

/// `sanitize` for text that may already carry this module's paint (kv values,
/// table cells, prompt and bullet text): with colour on, our own SGR
/// sequences survive and every other escape is neutralised. Painters sanitise
/// what they paint, so untrusted text should reach these slots painted;
/// anything raw that slips through can at most pick a palette colour, never
/// move the cursor, erase, conceal, retitle or touch the clipboard.
fn sanitize_painted(text: &str) -> Cow<'_, str> {
    if !enabled() || !text.contains('\x1b') {
        return sanitize(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('\x1b') {
        out.push_str(&sanitize(&rest[..i]));
        let tail = &rest[i..];
        match own_sgr_len(tail) {
            Some(n) => {
                out.push_str(&tail[..n]);
                rest = &tail[n..];
            }
            None => {
                out.push(STAND_IN);
                rest = &tail[1..];
            }
        }
    }
    out.push_str(&sanitize(rest));
    Cow::Owned(out)
}

/// Single-line layout slots: a newline would break the row (or forge one).
fn one_line(text: Cow<'_, str>) -> Cow<'_, str> {
    if text.contains('\n') {
        Cow::Owned(text.replace('\n', " "))
    } else {
        text
    }
}

/// Paint plain text; keep text that is already painted (sanitised).
fn paint_or_keep(text: &str, ink: Ink) -> String {
    if enabled() && text.contains('\x1b') {
        sanitize_painted(text).into_owned()
    } else {
        paint(text, ink)
    }
}

// ── Painters ──────────────────────────────────────────────────────────────────

fn sgr(text: &str, (r, g, b): (u8, u8, u8), bold: bool, dim: bool) -> String {
    let text = sanitize(text);
    if !enabled() || text.is_empty() {
        return text.into_owned();
    }
    let mut out = String::with_capacity(text.len() + 24);
    if bold {
        out.push_str("\x1b[1m");
    }
    if dim {
        out.push_str("\x1b[2m");
    }
    out.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
    out.push_str(&text);
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
    let text = sanitize(text);
    if !enabled() || text.is_empty() {
        return text.into_owned();
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

/// Clip to a display width, ellipsised. Returns PLAIN, sanitised, single-line
/// text — clip before you paint.
pub fn clip(text: &str, width: usize) -> String {
    let plain = one_line(Cow::Owned(strip_ansi(&sanitize_painted(text)))).into_owned();
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
                let cell = one_line(sanitize_painted(cell));
                let cell = if vis_len(&cell) <= c.width {
                    cell.into_owned()
                } else {
                    clip(&cell, c.width)
                };
                format!(" {} ", pad(&cell, c.width, c.align))
            })
            .collect();
        out.push(format!("{indent}{bar}{}{bar}", cells.join(&bar)));
    }
    out.push(format!("{indent}{}", line(&format!("└{}┘", seg("┴")))));
    out
}

/// Key/value block: slate key column, paper value (a pre-painted value keeps
/// its own paint).
pub fn kv(pairs: &[(&str, String)], key_width: usize) -> Vec<String> {
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "  {}{}",
                pad(&paint(k, Ink::Slate), key_width, Align::Left),
                paint_or_keep(v, Ink::Paper)
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
        paint_or_keep(&one_line(Cow::Borrowed(detail)), Ink::Slate)
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
    let text = sanitize(text);
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
        paint_or_keep(&one_line(Cow::Borrowed(text)), Ink::Paper),
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

    /// OSC 52 clipboard write, screen clear, carriage return, C1 CSI.
    const HOSTILE: &str = "\x1b]52;c;eA==\x07\x1b[2J\r\u{9b}31m";

    fn with_colour<T>(on: bool, f: impl FnOnce() -> T) -> T {
        TEST_COLOR.with(|c| c.set(Some(on)));
        let out = f();
        TEST_COLOR.with(|c| c.set(None));
        out
    }

    /// Every ESC opens an SGR form this module emits (any 24-bit colour, for
    /// gradients); no other control character but '\n'; no bidi control.
    fn assert_safe(label: &str, s: &str) {
        let mut rest = s;
        while let Some(c) = rest.chars().next() {
            if c == '\x1b' {
                let tail = &rest[1..];
                let n = ["[0m", "[1m", "[2m"]
                    .iter()
                    .find(|f| tail.starts_with(**f))
                    .map(|f| f.len())
                    .or_else(|| {
                        let body = tail.strip_prefix("[38;2;")?;
                        let end = body.find('m')?;
                        body[..end]
                            .split(';')
                            .all(|ch| ch.parse::<u8>().is_ok())
                            .then_some("[38;2;".len() + end + 1)
                    })
                    .unwrap_or_else(|| panic!("{label}: foreign escape in {s:?}"));
                rest = &tail[n..];
                continue;
            }
            assert!(
                !(c.is_control() && c != '\n') && !is_bidi_control(c),
                "{label}: {c:?} in {s:?}"
            );
            rest = &rest[c.len_utf8()..];
        }
    }

    fn renderers(t: &str) -> Vec<(&'static str, String)> {
        let task = ptask_core::Task {
            id: "uuid-1".into(),
            pt_id: Some(t.to_string()),
            title: t.to_string(),
            description: t.to_string(),
            priority: 3,
            status: t.to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            deadline: Some(t.to_string()),
            source_type: "cli".into(),
            ai_reasoning: String::new(),
            kind: "ship".into(),
            deliverable: None,
        };
        let mut bands = BTreeMap::new();
        bands.insert(0usize, "tier".to_string());
        vec![
            ("sanitize", sanitize(t).into_owned()),
            ("paint", paint(t, Ink::Paper)),
            ("bold", bold(t, Ink::Paper)),
            ("dim", dim(t, Ink::Slate)),
            ("gradient", gradient(t, &[])),
            ("clip", clip(t, 12)),
            ("clip wide", clip(t, 200)),
            ("pad", pad(&paint(t, Ink::Paper), 40, Align::Right)),
            ("pill", pill(Status::Warn, t)),
            ("status_pill", status_pill(t)),
            ("pt_id", pt_id(t)),
            ("due_cell", due_cell(Some(t))),
            ("headline", headline(t, Some((t, Ink::Amber)), t).join("\n")),
            ("banner", banner(t, t, Some((t, Ink::Cyan))).join("\n")),
            (
                "table",
                table(
                    &[Column::new("A", 8), Column::new("B", 60)],
                    &[vec![t.to_string(), paint(t, Ink::Paper)]],
                    &bands,
                )
                .join("\n"),
            ),
            (
                "kv",
                kv(
                    &[("plain", t.to_string()), ("painted", paint(t, Ink::Red))],
                    10,
                )
                .join("\n"),
            ),
            ("section", section(t, Ink::Red, t)),
            ("bullet", bullet(t, t, Ink::Cyan, 10)),
            ("note", note(t)),
            ("outcome", outcome(Status::Ok, t, t, t, t)),
            ("footer", footer(2, t, t)),
            ("empty", empty(t)),
            ("wrap", wrap(t, 8, "  ").join("\n")),
            ("prompt", prompt(t, t)),
            (
                "task_table",
                task_table(&[task], true, true, true).join("\n"),
            ),
        ]
    }

    #[test]
    fn sanitize_replaces_controls_and_bidi_with_a_visible_stand_in() {
        assert!(matches!(sanitize("plain · ünïcode 日本"), Cow::Borrowed(_)));
        assert_eq!(
            sanitize(HOSTILE),
            "\u{fffd}]52;c;eA==\u{fffd}\u{fffd}[2J\u{fffd}\u{fffd}31m"
        );
        // Bidi overrides, isolates and marks reorder what follows.
        assert_eq!(
            sanitize("a\u{202e}b\u{2066}c\u{2069}d\u{200f}e\u{61c}f"),
            "a\u{fffd}b\u{fffd}c\u{fffd}d\u{fffd}e\u{fffd}f"
        );
        // Newlines survive; a CRLF line end displays the same as LF; a tab is
        // a space; a lone CR, BS, DEL and NEL do not survive.
        assert_eq!(sanitize("a\r\nb\n\tc"), "a\nb\n c");
        // has_hazard: only what changes what the terminal shows counts.
        assert!(has_hazard(HOSTILE) && has_hazard("x\ry") && has_hazard("a\u{202e}b"));
        assert!(!has_hazard("Dear Alan,\r\n\tthe numbers.\n") && !has_hazard(""));
        assert_eq!(
            sanitize("x\ry\u{8}z\u{7f}w\u{85}"),
            "x\u{fffd}y\u{fffd}z\u{fffd}w\u{fffd}"
        );
    }

    #[test]
    fn every_renderer_neutralises_hostile_text_without_colour() {
        let t = format!("pay {HOSTILE} \u{202e}now");
        for (name, out) in with_colour(false, || renderers(&t)) {
            assert!(
                out.chars().all(|c| !c.is_control() || c == '\n'),
                "{name}: {out:?}"
            );
            assert_safe(name, &out);
        }
        // Single-line slots flatten a newline instead of forging a row.
        assert_eq!(clip("a\nb", 10), "a b");
        let rows = with_colour(false, || {
            table(
                &[Column::new("T", 5)],
                &[vec!["a\nb".into()]],
                &BTreeMap::new(),
            )
        });
        assert_eq!(rows.len(), 5, "{rows:?}");
        assert!(rows[3].contains("│ a b   │"), "{rows:?}");
    }

    #[test]
    fn every_renderer_keeps_only_its_own_sgr_with_colour() {
        let t = format!("pay {HOSTILE} \u{202e}now");
        for (name, out) in with_colour(true, || renderers(&t)) {
            assert_safe(name, &out);
        }
        // Raw text cannot smuggle SGR either: painters neutralise all of it
        // (here a black foreground and conceal, which would hide text).
        let sneaky = "ok\x1b[38;2;0;0;0mhidden\x1b[8m; curl evil|sh";
        let out = with_colour(true, || paint(sneaky, Ink::Paper));
        assert!(
            !out.contains("\x1b[38;2;0;0;0m") && !out.contains("\x1b[8m"),
            "{out:?}"
        );
        assert!(out.contains("\u{fffd}[8m; curl evil|sh"), "{out:?}");
    }

    #[test]
    fn pre_painted_values_keep_their_paint_with_colour() {
        with_colour(true, || {
            let pill = status_pill("done");
            // kv and bullet keep a value that is already painted...
            assert!(kv(&[("status", pill.clone())], 8)[0].ends_with(&pill));
            let detail = format!("{}  {}", pill, paint("title", Ink::Paper));
            assert!(bullet("PT-1", &detail, Ink::Amber, 6).ends_with(&detail));
            // ...but a foreign escape inside one is still neutralised.
            let tainted = format!("{pill}\x1b]52;c;eA==\x07");
            let line = kv(&[("status", tainted)], 8).join("");
            assert!(
                line.contains(&pill) && !line.contains("\x1b]52"),
                "{line:?}"
            );
            assert_safe("tainted kv", &line);
            // A clipped pre-painted cell loses its paint, never gains garbage.
            assert_eq!(clip(&paint("abcdef", Ink::Red), 4), "abc…");
        });
    }
}
