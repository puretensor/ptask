//! Inline-token quick-add parser.
//!
//! Parses a single free-form string into a structured [`QuickAdd`].
//!
//! Grammar (whitespace-separated tokens, order-independent except `//`):
//!
//! - `@label`        — single label (multiple `@labels` accepted)
//! - `#project`      — single project (last one wins if repeated)
//! - `p1` `p2` `p3` `p4` — priority (defaults to "normal" / 2 if absent)
//! - `~30m` `~2h` `~1d` — duration estimate in minutes
//! - `!HH:MM`        — reminder time-of-day
//! - `//rest of line` — everything after the `//` is the description
//! - Date phrase — `today` / `tomorrow` / `yesterday` / weekday names (with
//!   optional `this|next|last` prefix), `N days`, month names, ISO dates,
//!   optionally followed by time.
//! - Anything else   — title words
//!
//! Example:
//!   `Buy bread tomorrow 10am @home #fleet p1 ~30m //grocery list`
//!   →  title="Buy bread", deadline=<2026-05-14T10:00 London>, labels=["home"],
//!      project="fleet", priority=1, duration_min=30, description="grocery list"

use crate::dates;
use crate::error::Result;
use crate::priority;
use jiff::Zoned;

/// Result of parsing a quick-add string.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct QuickAdd {
    pub title: String,
    pub priority: Option<i64>,
    pub deadline: Option<String>, // ISO-formatted Zoned
    pub deadline_phrase: Option<String>,
    pub description: String,
    pub labels: Vec<String>,
    pub project: Option<String>,
    pub duration_min: Option<i64>,
    pub reminder: Option<String>,
}

/// Words that can start a date phrase. Used to bound the greedy date scan.
const DATE_STARTERS: &[&str] = &[
    "today",
    "tomorrow",
    "tom",
    "yesterday",
    "next",
    "this",
    "last",
    "monday",
    "mon",
    "tuesday",
    "tue",
    "wednesday",
    "wed",
    "thursday",
    "thu",
    "friday",
    "fri",
    "saturday",
    "sat",
    "sunday",
    "sun",
    "jan",
    "january",
    "feb",
    "february",
    "mar",
    "march",
    "apr",
    "april",
    "may",
    "jun",
    "june",
    "jul",
    "july",
    "aug",
    "august",
    "sep",
    "sept",
    "september",
    "oct",
    "october",
    "nov",
    "november",
    "dec",
    "december",
];

/// Parse a quick-add string against the operator-tz `now()` anchor.
pub fn parse(input: &str) -> Result<QuickAdd> {
    parse_at(input, dates::now_in_operator_tz()?)
}

/// Parse a quick-add string against an explicit time anchor (testable).
pub fn parse_at(input: &str, now: Zoned) -> Result<QuickAdd> {
    let mut out = QuickAdd::default();

    // Description: everything after `//` (greedy, includes spaces).
    let (head, desc) = match input.find("//") {
        Some(idx) => (
            input[..idx].trim_end().to_string(),
            input[idx + 2..].trim().to_string(),
        ),
        None => (input.to_string(), String::new()),
    };
    out.description = desc;

    // Tokenize the head by whitespace.
    let raw: Vec<&str> = head.split_whitespace().collect();
    let mut idx = 0usize;
    let mut title_words: Vec<&str> = Vec::new();

    while idx < raw.len() {
        let tok = raw[idx];

        // Explicit @label
        if let Some(rest) = tok.strip_prefix('@')
            && !rest.is_empty()
        {
            out.labels.push(rest.to_string());
            idx += 1;
            continue;
        }
        // Explicit #project
        if let Some(rest) = tok.strip_prefix('#')
            && !rest.is_empty()
        {
            out.project = Some(rest.to_string());
            idx += 1;
            continue;
        }
        // Priority pN (1..=4). Quick-add uses Todoist's p1..p4 convention.
        // Map to pTask 5..1 scale (p1 → 4 urgent, p2 → 3 high,
        // p3 → 2 normal, p4 → 1 low).
        if let Some(rest) = tok.strip_prefix('p')
            && let Ok(n) = rest.parse::<i64>()
            && (1..=4).contains(&n)
        {
            out.priority = Some(5 - n);
            idx += 1;
            continue;
        }
        // Duration ~Nm / ~Nh / ~Nd
        if let Some(rest) = tok.strip_prefix('~')
            && let Some(mins) = parse_duration(rest)
        {
            out.duration_min = Some(mins);
            idx += 1;
            continue;
        }
        // Reminder !HH:MM
        if let Some(rest) = tok.strip_prefix('!')
            && !rest.is_empty()
            && rest.contains(':')
        {
            out.reminder = Some(rest.to_string());
            idx += 1;
            continue;
        }

        // Date phrase: greedy longest match from this position.
        if (is_date_starter(tok) || looks_like_iso_date(tok) || looks_like_clock_time(tok))
            && let Some((phrase, consumed)) = try_date_match(&raw, idx, &now)
        {
            let parsed = dates::parse_at(&phrase, now.clone())?;
            out.deadline_phrase = Some(phrase);
            out.deadline = Some(dates::format_iso(&parsed));
            idx += consumed;
            continue;
        }

        // Fallback: title word.
        title_words.push(tok);
        idx += 1;
    }

    out.title = title_words.join(" ").trim().to_string();
    // Priority default = normal (2).
    if out.priority.is_none() {
        out.priority = Some(2);
    }
    Ok(out)
}

/// Parse a duration suffix `Nm`, `Nh`, or `Nd` to minutes. Returns None on
/// unrecognised input.
fn parse_duration(s: &str) -> Option<i64> {
    if s.len() < 2 {
        return None;
    }
    let (num_part, unit) = s.split_at(s.len() - 1);
    let n: i64 = num_part.parse().ok()?;
    match unit {
        "m" => Some(n),
        "h" => Some(n.checked_mul(60)?),
        "d" => Some(n.checked_mul(60 * 24)?),
        _ => None,
    }
}

fn is_date_starter(tok: &str) -> bool {
    let lower = tok.to_ascii_lowercase();
    DATE_STARTERS.iter().any(|&w| w == lower)
}

/// Tokens like `2026-05-20`, `20/05/2026`, `5/20`.
fn looks_like_iso_date(tok: &str) -> bool {
    let bytes = tok.as_bytes();
    bytes.contains(&b'-') && bytes.iter().filter(|&&b| b == b'-').count() == 2
        || (bytes.contains(&b'/') && bytes.iter().any(|b| b.is_ascii_digit()))
}

/// Tokens like `5pm`, `10am`, `09:30`, `17:00`.
fn looks_like_clock_time(tok: &str) -> bool {
    let lower = tok.to_ascii_lowercase();
    lower.ends_with("am") || lower.ends_with("pm") || lower.contains(':')
}

/// Greedy longest-match: extend a date phrase by appending subsequent tokens
/// while `interim::parse_date_string` still accepts the result. Stops at the
/// first token that is an explicit non-date marker (`@`, `#`, `p[1-4]`, `~`,
/// `!`, `//`) or after `MAX_DATE_TOKENS`.
fn try_date_match(toks: &[&str], start: usize, now: &Zoned) -> Option<(String, usize)> {
    const MAX_DATE_TOKENS: usize = 5;
    let mut best: Option<(String, usize)> = None;
    let upper = (start + MAX_DATE_TOKENS).min(toks.len());
    for end in (start + 1)..=upper {
        // Stop expanding once we hit an explicit non-date token.
        let last = toks[end - 1];
        if end > start + 1 && is_explicit_marker(last) {
            break;
        }
        let phrase = toks[start..end].join(" ");
        if dates::parse_at(&phrase, now.clone()).is_ok() {
            best = Some((phrase, end - start));
        }
    }
    best
}

/// Tokens that signal a structural quick-add marker, never a date word.
fn is_explicit_marker(tok: &str) -> bool {
    if tok.starts_with('@') || tok.starts_with('#') || tok.starts_with('~') || tok.starts_with('!')
    {
        return true;
    }
    if tok.starts_with("//") {
        return true;
    }
    if let Some(rest) = tok.strip_prefix('p')
        && let Ok(n) = rest.parse::<i64>()
        && (1..=4).contains(&n)
    {
        return true;
    }
    false
}

/// Format a quick-add into a short human echo. Convenience for `pt add` output.
pub fn summarise(q: &QuickAdd) -> String {
    let mut bits: Vec<String> = vec![format!("p={}", priority::label(q.priority.unwrap_or(2)))];
    if let Some(d) = &q.deadline_phrase {
        bits.push(format!("due={}", d));
    }
    if let Some(p) = &q.project {
        bits.push(format!("#{}", p));
    }
    for l in &q.labels {
        bits.push(format!("@{}", l));
    }
    if let Some(m) = q.duration_min {
        bits.push(format!("~{}m", m));
    }
    if let Some(r) = &q.reminder {
        bits.push(format!("!{}", r));
    }
    bits.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn anchor() -> Zoned {
        let tz = jiff::tz::TimeZone::get(dates::OPERATOR_TZ).unwrap();
        date(2026, 5, 13).at(12, 0, 0, 0).to_zoned(tz).unwrap()
    }

    #[test]
    fn explicit_tokens_only() {
        let q = parse_at(
            "Fix Ceph OSD flapping @ops #fleet p1 ~45m //arx3 latency spike",
            anchor(),
        )
        .unwrap();
        assert_eq!(q.title, "Fix Ceph OSD flapping");
        assert_eq!(q.priority, Some(4)); // p1 → pTask urgent (4)
        assert_eq!(q.project.as_deref(), Some("fleet"));
        assert_eq!(q.labels, vec!["ops"]);
        assert_eq!(q.duration_min, Some(45));
        assert_eq!(q.description, "arx3 latency spike");
        assert!(q.deadline.is_none());
    }

    #[test]
    fn priority_default_is_normal() {
        let q = parse_at("walk dog", anchor()).unwrap();
        assert_eq!(q.priority, Some(2));
    }

    #[test]
    fn date_phrase_tomorrow_extracted() {
        let q = parse_at("Buy bread tomorrow", anchor()).unwrap();
        assert_eq!(q.title, "Buy bread");
        assert!(q.deadline.is_some());
        assert!(
            q.deadline.as_deref().unwrap().starts_with("2026-05-14"),
            "got {:?}",
            q.deadline
        );
    }

    #[test]
    fn date_phrase_with_time_extracted() {
        let q = parse_at("Buy bread tomorrow 10am @home", anchor()).unwrap();
        assert_eq!(q.title, "Buy bread");
        assert_eq!(q.labels, vec!["home"]);
        // Tomorrow at 10am: should start with 2026-05-14T10
        assert!(
            q.deadline.as_deref().unwrap().starts_with("2026-05-14T10"),
            "got {:?}",
            q.deadline
        );
    }

    #[test]
    fn next_friday_8pm() {
        let q = parse_at("Meet Joshua next friday 8pm @sf", anchor()).unwrap();
        assert_eq!(q.title, "Meet Joshua");
        assert_eq!(q.labels, vec!["sf"]);
        // Next Friday (UK dialect) from Wed 2026-05-13 = 2026-05-22 at 20:00.
        assert!(
            q.deadline.as_deref().unwrap().starts_with("2026-05-22T20"),
            "got {:?}",
            q.deadline
        );
    }

    #[test]
    fn iso_date_token() {
        let q = parse_at("Pay invoice 2026-12-25", anchor()).unwrap();
        assert_eq!(q.title, "Pay invoice");
        assert!(q.deadline.as_deref().unwrap().starts_with("2026-12-25"));
    }

    #[test]
    fn duration_units() {
        let q = parse_at("review pr ~2h", anchor()).unwrap();
        assert_eq!(q.duration_min, Some(120));
        let q = parse_at("vacation ~3d", anchor()).unwrap();
        assert_eq!(q.duration_min, Some(60 * 24 * 3));
    }

    #[test]
    fn description_after_slashslash_is_greedy() {
        let q = parse_at("title here //long\n description with words", anchor()).unwrap();
        // The `//` consumes everything after it including any apparent tokens.
        assert_eq!(q.description, "long\n description with words");
        assert_eq!(q.title, "title here");
    }

    #[test]
    fn multiple_labels() {
        let q = parse_at("ship release @ops @devops @oncall", anchor()).unwrap();
        assert_eq!(q.labels, vec!["ops", "devops", "oncall"]);
    }

    #[test]
    fn reminder_token() {
        let q = parse_at("call dentist !09:30", anchor()).unwrap();
        assert_eq!(q.reminder.as_deref(), Some("09:30"));
        assert_eq!(q.title, "call dentist");
    }

    #[test]
    fn title_punctuation_preserved() {
        let q = parse_at("Fix bug: foo bar (urgent!)", anchor()).unwrap();
        // The trailing "!)" isn't a valid !HH:MM reminder, so it stays in the title.
        assert!(q.title.contains("(urgent"));
    }

    #[test]
    fn p1_through_p4_map_to_pt_scale() {
        for (input, expected) in &[("foo p1", 4), ("foo p2", 3), ("foo p3", 2), ("foo p4", 1)] {
            let q = parse_at(input, anchor()).unwrap();
            assert_eq!(q.priority, Some(*expected), "input={input}");
        }
    }

    #[test]
    fn unknown_p_token_stays_in_title() {
        // p5 is out of Todoist range; stays as a title word.
        let q = parse_at("foo p5 bar", anchor()).unwrap();
        assert!(q.title.contains("p5"), "got title {:?}", q.title);
    }

    #[test]
    fn summarise_renders_active_fields() {
        let q = QuickAdd {
            title: "x".into(),
            priority: Some(4),
            deadline_phrase: Some("tomorrow".into()),
            project: Some("fleet".into()),
            labels: vec!["ops".into()],
            duration_min: Some(30),
            ..Default::default()
        };
        let s = summarise(&q);
        assert!(s.contains("p=urgent"));
        assert!(s.contains("due=tomorrow"));
        assert!(s.contains("#fleet"));
        assert!(s.contains("@ops"));
        assert!(s.contains("~30m"));
    }
}
