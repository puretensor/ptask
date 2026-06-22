//! Inline-token quick-add parser.
//!
//! Parses a single free-form string into a structured [`QuickAdd`].
//!
//! Grammar (whitespace-separated tokens, order-independent except `//`):
//!
//! - `@label`        — single label (multiple `@labels` accepted)
//! - `#project`      — single project (last one wins if repeated)
//! - `p1`..`p5`     — priority, native scale (p1=low, p2=normal, p3=high,
//!   p4=urgent, p5=critical; defaults to "normal" / 2 if absent)
//! - `~30m` `~2h` `~1d` — duration estimate in minutes
//! - `!HH:MM`        — reminder time-of-day
//! - `//rest of line` — everything after the `//` is the description
//! - Date phrase — `today` / `tomorrow` / `yesterday` / weekday names (with
//!   optional `this|next|last` prefix), `N days`, month names, ISO dates,
//!   optionally followed by time.
//! - `"quoted text"` — words inside double quotes are literal title text,
//!   never interpreted as markers or dates (`add 'Review the "p1 incident"'`).
//! - Anything else   — title words
//!
//! Example:
//!   `Buy bread tomorrow 10am @home #fleet p1 ~30m //grocery list`
//!   →  title="Buy bread", deadline=<2026-05-14T10:00 London>, labels=["home"],
//!      project="fleet", priority=1, duration_min=30, description="grocery list"

use crate::dates;
use crate::error::{Error, Result};
use crate::priority;
use crate::recurrence::{self, Recurrence};
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
    /// Parsed recurrence rule, if the input contained an `every` / `every!`
    /// clause. The deadline above is the first occurrence.
    pub recurrence: Option<Recurrence>,
    /// Non-fatal parse caveats the surface should echo to the operator —
    /// e.g. a date phrase that resolved to a moment already in the past.
    pub warnings: Vec<String>,
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

    // Tokenize the head by whitespace, honouring double-quoted spans:
    // words inside "..." are literal title text, never parsed as markers
    // or date phrases. (Note: the `//` description split above runs first,
    // so a quoted `//` still starts the description.)
    let (raw, literal) = tokenize_quoted(&head);
    let mut idx = 0usize;
    let mut title_words: Vec<&str> = Vec::new();

    while idx < raw.len() {
        let tok = raw[idx];

        // Quoted: straight to the title, no token interpretation.
        if literal[idx] {
            title_words.push(tok);
            idx += 1;
            continue;
        }
        // Multi-token lookahead (dates, recurrence) must not consume into a
        // quoted span; bound the scan at the next literal token.
        let scan_end = literal[idx..]
            .iter()
            .position(|&l| l)
            .map(|off| idx + off)
            .unwrap_or(raw.len());

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
        // Priority pN (1..=5), native pTask scale: p1=low, p2=normal,
        // p3=high, p4=urgent, p5=critical. Matches the display, `--priority`,
        // and `pt priority` — no Todoist inversion.
        if let Some(rest) = tok.strip_prefix('p')
            && let Ok(n) = rest.parse::<i64>()
            && (1..=5).contains(&n)
        {
            out.priority = Some(n);
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

        // Recurrence: `every X` / `every! X`. Greedy consumption up to next
        // explicit marker. Optional trailing " at <time>" sets the time-of-day
        // for the first (and subsequent) occurrence.
        if (tok.eq_ignore_ascii_case("every") || tok.eq_ignore_ascii_case("every!"))
            && let Some((rec, time_of_day, consumed, phrase)) =
                try_recurrence_match(&raw[..scan_end], idx, &now)
        {
            let deadline = first_recurrence_deadline(&rec, &now, time_of_day.as_ref())?;
            out.deadline_phrase = Some(phrase);
            out.deadline = Some(dates::format_iso(&deadline));
            out.recurrence = Some(rec);
            idx += consumed;
            continue;
        }

        // Date phrase: greedy longest match from this position.
        if (is_date_starter(tok)
            || looks_like_iso_date(tok)
            || looks_like_clock_time(tok)
            || starts_relative_interval(&raw[..scan_end], idx)
            || starts_in_relative_interval(&raw[..scan_end], idx))
            && let Some((phrase, consumed)) = try_date_match(&raw[..scan_end], idx, &now)
        {
            let parsed = parse_quickadd_date(&phrase, &now)?;
            if parsed < now {
                out.warnings.push(format!(
                    "deadline '{}' resolved to {} — already in the past",
                    phrase,
                    dates::format_iso(&parsed)
                ));
            }
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
    if out.title.is_empty() {
        return Err(Error::Other(
            "quick-add: title is empty after parsing inline tokens; use --raw for a literal token-only title"
                .into(),
        ));
    }
    // Priority default = normal (2).
    if out.priority.is_none() {
        out.priority = Some(2);
    }
    Ok(out)
}

/// Whitespace tokenizer with double-quote spans. Returns the tokens plus a
/// parallel `literal` flag: words inside `"..."` are title text, exempt from
/// marker/date interpretation. An unmatched `"` is kept as ordinary text.
fn tokenize_quoted(head: &str) -> (Vec<&str>, Vec<bool>) {
    let mut toks: Vec<&str> = Vec::new();
    let mut lits: Vec<bool> = Vec::new();
    let mut rest = head;
    while let Some(q) = rest.find('"') {
        for w in rest[..q].split_whitespace() {
            toks.push(w);
            lits.push(false);
        }
        let after = &rest[q + 1..];
        match after.find('"') {
            Some(close) => {
                for w in after[..close].split_whitespace() {
                    toks.push(w);
                    lits.push(true);
                }
                rest = &after[close + 1..];
            }
            None => {
                // Unmatched quote: keep the remainder (quote included) as
                // ordinary tokens rather than guessing intent.
                for w in rest[q..].split_whitespace() {
                    toks.push(w);
                    lits.push(false);
                }
                rest = "";
            }
        }
    }
    for w in rest.split_whitespace() {
        toks.push(w);
        lits.push(false);
    }
    (toks, lits)
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

fn starts_relative_interval(toks: &[&str], start: usize) -> bool {
    toks.get(start).is_some_and(|tok| is_quantity(tok))
        && toks.get(start + 1).is_some_and(|tok| is_relative_unit(tok))
}

fn starts_in_relative_interval(toks: &[&str], start: usize) -> bool {
    toks.get(start)
        .is_some_and(|tok| tok.eq_ignore_ascii_case("in"))
        && toks.get(start + 1).is_some_and(|tok| is_quantity(tok))
        && toks.get(start + 2).is_some_and(|tok| is_relative_unit(tok))
}

fn is_quantity(tok: &str) -> bool {
    !tok.is_empty() && tok.chars().all(|ch| ch.is_ascii_digit())
}

fn is_relative_unit(tok: &str) -> bool {
    matches!(
        tok.to_ascii_lowercase().as_str(),
        "minute"
            | "minutes"
            | "min"
            | "mins"
            | "hour"
            | "hours"
            | "day"
            | "days"
            | "week"
            | "weeks"
            | "month"
            | "months"
    )
}

fn parse_quickadd_date(phrase: &str, now: &Zoned) -> Result<Zoned> {
    dates::parse_at(normalise_date_phrase(phrase), now.clone())
}

/// Consume tokens starting at `start` (`every` / `every!`) up to the next
/// explicit marker or end-of-input. Returns the parsed Recurrence, an optional
/// time-of-day (from a trailing " at <time>"), the number of tokens consumed,
/// and the original phrase string (for `deadline_phrase`).
fn try_recurrence_match(
    toks: &[&str],
    start: usize,
    now: &Zoned,
) -> Option<(Recurrence, Option<Zoned>, usize, String)> {
    let mut end = start + 1;
    while end < toks.len() {
        let t = toks[end];
        if is_explicit_marker(t) {
            break;
        }
        // Don't fold a second `every` clause into this one.
        if t.eq_ignore_ascii_case("every") || t.eq_ignore_ascii_case("every!") {
            break;
        }
        end += 1;
    }
    if end == start + 1 {
        // Bare `every` with no rule body.
        return None;
    }
    let phrase = toks[start..end].join(" ");

    let rec = recurrence::parse(&phrase).ok()?;
    let (_rule_part, time_part) = recurrence::split_time_suffix(&phrase);
    let time = time_part.and_then(|t| dates::parse_at(&format!("today {}", t), now.clone()).ok());
    Some((rec, time, end - start, phrase))
}

fn first_recurrence_deadline(
    rec: &Recurrence,
    now: &Zoned,
    time_of_day: Option<&Zoned>,
) -> Result<Zoned> {
    let first = recurrence::next_after(rec, now)?;
    let Some(time) = time_of_day else {
        return Ok(first);
    };

    let today = combine_date_with_time(now, time)?;
    let can_occur_today = match rec.freq {
        recurrence::Freq::Daily => rec.interval == 1,
        recurrence::Freq::Weekly => rec.bydays.contains(&now.weekday()),
        recurrence::Freq::Monthly => rec.bymonthday.contains(&now.day()),
    };
    if can_occur_today && today > now.clone() {
        return Ok(today);
    }

    combine_date_with_time(&first, time)
}

/// Replace the time-of-day of `date_z` with the time-of-day of `time_z`,
/// keeping `date_z`'s timezone.
fn combine_date_with_time(date_z: &Zoned, time_z: &Zoned) -> Result<Zoned> {
    let tz = date_z.time_zone().clone();
    let civil = date_z.date().at(
        time_z.hour(),
        time_z.minute(),
        time_z.second(),
        time_z.subsec_nanosecond(),
    );
    civil
        .to_zoned(tz)
        .map_err(|e| Error::Other(format!("combine date+time: {}", e)))
}

fn normalise_date_phrase(phrase: &str) -> &str {
    let trimmed = phrase.trim();
    if let Some(after_in) = trimmed
        .get(..2)
        .filter(|prefix| prefix.eq_ignore_ascii_case("in"))
        .and_then(|_| trimmed.get(2..))
        && after_in.chars().next().is_some_and(|ch| ch.is_whitespace())
    {
        after_in.trim_start()
    } else {
        trimmed
    }
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
        if parse_quickadd_date(&phrase, now).is_ok() {
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
        && (1..=5).contains(&n)
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
    fn quoted_tokens_are_literal_title_text() {
        // Verified live failure pre-fix: a bare "p1" inside a sentence silently
        // set the task's priority. Quotes now protect literal text.
        let q = parse_at("Review the \"p1 incident\" postmortem", anchor()).unwrap();
        assert_eq!(q.title, "Review the p1 incident postmortem");
        assert_eq!(q.priority, Some(2), "quoted p1 must not set priority");
        assert!(q.deadline.is_none());
    }

    #[test]
    fn quoted_date_words_are_not_parsed() {
        let q = parse_at("Plan \"May offsite\" kickoff", anchor()).unwrap();
        assert_eq!(q.title, "Plan May offsite kickoff");
        assert!(q.deadline.is_none(), "quoted month name must not be a date");
        assert!(q.warnings.is_empty());
    }

    #[test]
    fn quoted_span_bounds_date_lookahead() {
        // The greedy date scan must stop at the quote boundary: `tomorrow`
        // parses, but the quoted `10am` stays title text.
        let q = parse_at("Ship build tomorrow \"10am demo notes\"", anchor()).unwrap();
        assert_eq!(q.title, "Ship build 10am demo notes");
        let d = q.deadline.expect("tomorrow still parses");
        assert!(d.starts_with("2026-05-14"), "deadline was {}", d);
    }

    #[test]
    fn unmatched_quote_is_ordinary_text() {
        let q = parse_at("say \"hello p1", anchor()).unwrap();
        assert_eq!(q.title, "say \"hello");
        // Outside any closed quote span, p1 still parses as priority (low).
        assert_eq!(q.priority, Some(1));
    }

    #[test]
    fn past_deadline_emits_warning() {
        let q = parse_at("Pay invoice yesterday", anchor()).unwrap();
        assert!(q.deadline.is_some());
        assert_eq!(q.warnings.len(), 1, "warnings: {:?}", q.warnings);
        assert!(q.warnings[0].contains("in the past"));
    }

    #[test]
    fn bare_month_in_past_warns_instead_of_silently_backdating() {
        // Anchor June: "May" resolves to May 1 of the current year — in the
        // past. Verified live pre-fix: silent past deadline, no signal.
        let tz = jiff::tz::TimeZone::get(dates::OPERATOR_TZ).unwrap();
        let june = date(2026, 6, 10).at(12, 0, 0, 0).to_zoned(tz).unwrap();
        let q = parse_at("Plan May offsite", june).unwrap();
        if q.deadline.is_some() {
            assert!(
                !q.warnings.is_empty(),
                "past-resolving month must warn; got none"
            );
        }
    }

    #[test]
    fn future_deadline_no_warning() {
        let q = parse_at("Pay invoice tomorrow 10am", anchor()).unwrap();
        assert!(q.deadline.is_some());
        assert!(q.warnings.is_empty(), "warnings: {:?}", q.warnings);
    }

    #[test]
    fn explicit_tokens_only() {
        let q = parse_at(
            "Fix Ceph OSD flapping @ops #fleet p4 ~45m //arx3 latency spike",
            anchor(),
        )
        .unwrap();
        assert_eq!(q.title, "Fix Ceph OSD flapping");
        assert_eq!(q.priority, Some(4)); // p4 → pTask urgent (4), native scale
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
    fn relative_interval_extracted() {
        let q = parse_at("Water plants 5 days", anchor()).unwrap();
        assert_eq!(q.title, "Water plants");
        assert_eq!(q.deadline_phrase.as_deref(), Some("5 days"));
        assert!(
            q.deadline.as_deref().unwrap().starts_with("2026-05-18"),
            "got {:?}",
            q.deadline
        );
    }

    #[test]
    fn recurrence_every_monday_extracted() {
        // Wed 2026-05-13 anchor → next monday = 2026-05-18.
        let q = parse_at("Standup every monday @ops", anchor()).unwrap();
        assert_eq!(q.title, "Standup");
        assert_eq!(q.labels, vec!["ops"]);
        let rec = q.recurrence.expect("recurrence parsed");
        assert_eq!(rec.original_input, "every monday");
        assert!(
            q.deadline.as_deref().unwrap().starts_with("2026-05-18"),
            "got {:?}",
            q.deadline
        );
    }

    #[test]
    fn recurrence_every_monday_at_9am_sets_time_of_day() {
        let q = parse_at("Standup every monday at 9am", anchor()).unwrap();
        let rec = q.recurrence.expect("recurrence parsed");
        assert_eq!(rec.original_input, "every monday at 9am");
        assert_eq!(q.deadline_phrase.as_deref(), Some("every monday at 9am"));
        // Mon 2026-05-18 09:00 in operator tz (BST = +01:00 in May).
        let dl = q.deadline.expect("deadline set");
        assert!(dl.starts_with("2026-05-18T09:00"), "got {dl}");
    }

    #[test]
    fn recurrence_every_day_at_future_time_can_land_today() {
        let q = parse_at("Backup every day at 9pm", anchor()).unwrap();
        assert_eq!(q.title, "Backup");
        let dl = q.deadline.expect("deadline set");
        assert!(dl.starts_with("2026-05-13T21:00"), "got {dl}");
    }

    #[test]
    fn recurrence_every_day_at_past_time_lands_tomorrow() {
        let q = parse_at("Backup every day at 9am", anchor()).unwrap();
        assert_eq!(q.title, "Backup");
        let dl = q.deadline.expect("deadline set");
        assert!(dl.starts_with("2026-05-14T09:00"), "got {dl}");
    }

    #[test]
    fn recurrence_weekday_at_future_time_can_land_today() {
        let tz = jiff::tz::TimeZone::get(dates::OPERATOR_TZ).unwrap();
        let monday_morning = jiff::civil::date(2026, 5, 18)
            .at(8, 0, 0, 0)
            .to_zoned(tz)
            .unwrap();
        let q = parse_at("Standup every monday at 9am", monday_morning).unwrap();
        let dl = q.deadline.expect("deadline set");
        assert!(dl.starts_with("2026-05-18T09:00"), "got {dl}");
    }

    #[test]
    fn recurrence_every_bang_5_days_is_completion_mode() {
        let q = parse_at("Water plants every! 5 days", anchor()).unwrap();
        let rec = q.recurrence.expect("recurrence parsed");
        assert_eq!(rec.mode, crate::recurrence::Mode::Completion);
        // First occurrence = anchor + 5 days = 2026-05-18.
        assert!(
            q.deadline.as_deref().unwrap().starts_with("2026-05-18"),
            "got {:?}",
            q.deadline
        );
    }

    #[test]
    fn recurrence_weekday_list_with_commas() {
        let q = parse_at("Workout every mon, wed, fri @gym", anchor()).unwrap();
        assert_eq!(q.title, "Workout");
        assert_eq!(q.labels, vec!["gym"]);
        let rec = q.recurrence.expect("recurrence parsed");
        assert!(rec.rrule_str.contains("BYDAY=MO,WE,FR"));
    }

    #[test]
    fn in_relative_interval_extracted() {
        let q = parse_at("Water plants in 5 days", anchor()).unwrap();
        assert_eq!(q.title, "Water plants");
        assert_eq!(q.deadline_phrase.as_deref(), Some("in 5 days"));
        assert!(
            q.deadline.as_deref().unwrap().starts_with("2026-05-18"),
            "got {:?}",
            q.deadline
        );
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
    fn p1_through_p5_map_to_native_scale() {
        // Native pTask scale, identical to the display / `pt priority`:
        // p1=low(1) .. p5=critical(5). No Todoist inversion.
        for (input, expected) in &[
            ("foo p1", 1),
            ("foo p2", 2),
            ("foo p3", 3),
            ("foo p4", 4),
            ("foo p5", 5),
        ] {
            let q = parse_at(input, anchor()).unwrap();
            assert_eq!(q.priority, Some(*expected), "input={input}");
        }
    }

    #[test]
    fn unknown_p_token_stays_in_title() {
        // p6 is out of the 1..=5 range; stays as a title word.
        let q = parse_at("foo p6 bar", anchor()).unwrap();
        assert!(q.title.contains("p6"), "got title {:?}", q.title);
    }

    #[test]
    fn token_only_input_is_rejected() {
        let err = parse_at("@ops p1 ~5m", anchor()).unwrap_err();
        assert!(format!("{}", err).contains("title is empty"));
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
