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
//! - Future ISO date — an exact `YYYY-MM-DD` token. Other date-like prose is
//!   kept as literal title text.
//! - `"quoted text"` — words inside double quotes are literal title text,
//!   never interpreted as markers or dates (`add 'Review the "p1 incident"'`).
//! - Anything else   — title words
//!
//! Example:
//!   `Buy bread 2099-05-14 @home #fleet p1 ~30m //grocery list`
//!   →  title="Buy bread", deadline=<2099-05-14T00:00 London>, labels=["home"],
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
    /// Scheduled date from an explicit `due:<date>` token — when the
    /// operator PLANS to do it, distinct from the hard deadline.
    pub due: Option<String>,
    pub description: String,
    pub labels: Vec<String>,
    pub project: Option<String>,
    pub duration_min: Option<i64>,
    pub reminder: Option<String>,
    /// Parsed recurrence rule, if the input contained an `every` / `every!`
    /// clause. The deadline above is the first occurrence.
    pub recurrence: Option<Recurrence>,
    /// Non-fatal parse caveats the surface should echo to the operator.
    pub warnings: Vec<String>,
}

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

        // Explicit due:<date> — scheduled date (distinct from deadline).
        if let Some(rest) = tok.strip_prefix("due:")
            && !rest.is_empty()
        {
            let parsed = dates::parse_at(rest, now.clone())
                .map_err(|e| crate::Error::Other(format!("due: date {:?}: {}", rest, e)))?;
            out.due = Some(dates::format_iso(&parsed));
            idx += 1;
            continue;
        }
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

        // Body-text deadline inference is deliberately narrow. Only a full,
        // standalone ISO date that is strictly in the future is eligible;
        // ambiguous fragments (`4/5`), natural-language dates, and past ISO
        // dates remain ordinary title text and never set-then-warn.
        if is_full_iso_date(tok)
            && let Ok(parsed) = dates::parse_at(tok, now.clone())
            && parsed > now
        {
            out.deadline_phrase = Some(tok.to_string());
            out.deadline = Some(dates::format_iso(&parsed));
            idx += 1;
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
    // Split off the last *char* (not byte): `s.len() - 1` can land inside a
    // trailing multi-byte scalar (e.g. `~5°`), and `str::split_at` panics on a
    // non-char boundary. The valid units are single ASCII chars, so anything
    // multi-byte falls through to the `_ => None` arm.
    let unit = s.chars().last()?;
    let num_part = &s[..s.len() - unit.len_utf8()];
    if num_part.is_empty() {
        return None;
    }
    let n: i64 = num_part.parse().ok()?;
    match unit {
        'm' => Some(n),
        'h' => Some(n.checked_mul(60)?),
        'd' => Some(n.checked_mul(60 * 24)?),
        _ => None,
    }
}

/// Exact ASCII `YYYY-MM-DD`; semantic date validation is left to `dates`.
fn is_full_iso_date(tok: &str) -> bool {
    matches!(
        tok.as_bytes(),
        [y0, y1, y2, y3, b'-', m0, m1, b'-', d0, d1]
            if [y0, y1, y2, y3, m0, m1, d0, d1]
                .iter()
                .all(|b| b.is_ascii_digit())
    )
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

    fn incident_anchor() -> Zoned {
        let tz = jiff::tz::TimeZone::get(dates::OPERATOR_TZ).unwrap();
        date(2026, 8, 1).at(12, 0, 0, 0).to_zoned(tz).unwrap()
    }

    #[test]
    fn task_add_body_iso_provenance_date_does_not_set_deadline() {
        let input = "...discovered 2026-08-01 during PT-1478 integration...";
        let q = parse_at(input, incident_anchor()).unwrap();

        assert!(
            q.deadline.is_none(),
            "unexpected deadline: {:?}",
            q.deadline
        );
        assert_eq!(q.title, input);
        assert!(q.warnings.is_empty(), "warnings: {:?}", q.warnings);
    }

    #[test]
    fn task_add_body_step_fraction_does_not_set_deadline() {
        let input = "...so step 4/5 restart+health-poll...";
        let q = parse_at(input, incident_anchor()).unwrap();

        assert!(
            q.deadline.is_none(),
            "unexpected deadline: {:?}",
            q.deadline
        );
        assert_eq!(q.title, input);
        assert!(q.warnings.is_empty(), "warnings: {:?}", q.warnings);
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
    fn parse_duration_rejects_trailing_multibyte_without_panic() {
        // Regression: `split_at(s.len() - 1)` panicked when the final char was
        // multi-byte — reachable from `pt add "task ~5°"` and the MCP task_add
        // tool, both of which funnel through quickadd. Must return None.
        assert_eq!(parse_duration("5°"), None);
        assert_eq!(parse_duration("5€"), None);
        assert_eq!(parse_duration("1²"), None);
        // Valid ASCII units still parse.
        assert_eq!(parse_duration("5m"), Some(5));
        assert_eq!(parse_duration("2h"), Some(120));
        assert_eq!(parse_duration("3d"), Some(3 * 60 * 24));
        // Degenerate inputs resolve to None, never panic.
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("m"), None);
        assert_eq!(parse_duration("°"), None);
    }

    #[test]
    fn quickadd_parse_survives_multibyte_duration_token() {
        // End-to-end: the token that used to panic is now just literal title
        // text (unrecognised duration), not a crash.
        let q = parse_at("ship the thing ~5°", anchor()).unwrap();
        assert!(q.duration_min.is_none());
        assert!(q.title.contains("ship the thing"));
    }

    #[test]
    fn quoted_date_words_are_not_parsed() {
        let q = parse_at("Plan \"May offsite\" kickoff", anchor()).unwrap();
        assert_eq!(q.title, "Plan May offsite kickoff");
        assert!(q.deadline.is_none(), "quoted month name must not be a date");
        assert!(q.warnings.is_empty());
    }

    #[test]
    fn quoted_span_preserves_natural_language_date_text() {
        let q = parse_at("Ship build tomorrow \"10am demo notes\"", anchor()).unwrap();
        assert_eq!(q.title, "Ship build tomorrow 10am demo notes");
        assert!(q.deadline.is_none());
        assert!(q.warnings.is_empty());
    }

    #[test]
    fn unmatched_quote_is_ordinary_text() {
        let q = parse_at("say \"hello p1", anchor()).unwrap();
        assert_eq!(q.title, "say \"hello");
        // Outside any closed quote span, p1 still parses as priority (low).
        assert_eq!(q.priority, Some(1));
    }

    #[test]
    fn natural_language_past_date_is_literal_without_warning() {
        let q = parse_at("Pay invoice yesterday", anchor()).unwrap();
        assert_eq!(q.title, "Pay invoice yesterday");
        assert!(q.deadline.is_none());
        assert!(q.warnings.is_empty());
    }

    #[test]
    fn bare_month_in_past_is_literal_without_warning() {
        let tz = jiff::tz::TimeZone::get(dates::OPERATOR_TZ).unwrap();
        let june = date(2026, 6, 10).at(12, 0, 0, 0).to_zoned(tz).unwrap();
        let q = parse_at("Plan May offsite", june).unwrap();
        assert_eq!(q.title, "Plan May offsite");
        assert!(q.deadline.is_none());
        assert!(q.warnings.is_empty());
    }

    #[test]
    fn mixed_case_date_phrase_is_literal_without_panic() {
        let q = parse_at("book flights Sat 18 Jul", anchor()).unwrap();
        assert_eq!(q.title, "book flights Sat 18 Jul");
        assert!(q.deadline.is_none());
        assert!(q.warnings.is_empty());
    }

    #[test]
    fn month_day_phrase_is_literal() {
        let q = parse_at("pay rent Jul 18", anchor()).unwrap();
        assert_eq!(q.title, "pay rent Jul 18");
        assert!(q.deadline.is_none());
    }

    #[test]
    fn bare_month_token_is_title_text_not_deadline() {
        // Regression (PT-1267): prose "Jul" mid-title silently set
        // deadline=2026-07-01 (the past) while due:2026-08-09 parsed fine.
        // A single-token bare month is rejected as a date phrase now.
        let q = parse_at("draft the Jul board report due:2026-08-09", anchor()).unwrap();
        assert_eq!(q.title, "draft the Jul board report");
        assert!(
            q.deadline.is_none(),
            "bare month must not set a deadline: {:?}",
            q.deadline
        );
        assert!(
            q.due.as_deref().unwrap().starts_with("2026-08-09"),
            "got {:?}",
            q.due
        );
    }

    #[test]
    fn long_date_like_phrase_is_literal_without_panic() {
        let q = parse_at("standup tomorrow 5 internationalisation", anchor()).unwrap();
        assert_eq!(q.title, "standup tomorrow 5 internationalisation");
        assert!(q.deadline.is_none());
    }

    #[test]
    fn natural_language_future_date_is_literal_without_warning() {
        let q = parse_at("Pay invoice tomorrow 10am", anchor()).unwrap();
        assert_eq!(q.title, "Pay invoice tomorrow 10am");
        assert!(q.deadline.is_none());
        assert!(q.warnings.is_empty());
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
    fn date_phrase_tomorrow_is_literal() {
        let q = parse_at("Buy bread tomorrow", anchor()).unwrap();
        assert_eq!(q.title, "Buy bread tomorrow");
        assert!(q.deadline.is_none());
    }

    #[test]
    fn date_phrase_with_time_is_literal() {
        let q = parse_at("Buy bread tomorrow 10am @home", anchor()).unwrap();
        assert_eq!(q.title, "Buy bread tomorrow 10am");
        assert_eq!(q.labels, vec!["home"]);
        assert!(q.deadline.is_none());
    }

    #[test]
    fn weekday_with_time_is_literal() {
        let q = parse_at("Meet Joshua next friday 8pm @sf", anchor()).unwrap();
        assert_eq!(q.title, "Meet Joshua next friday 8pm");
        assert_eq!(q.labels, vec!["sf"]);
        assert!(q.deadline.is_none());
    }

    #[test]
    fn iso_date_token() {
        let q = parse_at("Pay invoice 2026-12-25", anchor()).unwrap();
        assert_eq!(q.title, "Pay invoice");
        assert!(q.deadline.as_deref().unwrap().starts_with("2026-12-25"));
    }

    #[test]
    fn relative_interval_is_literal() {
        let q = parse_at("Water plants 5 days", anchor()).unwrap();
        assert_eq!(q.title, "Water plants 5 days");
        assert!(q.deadline_phrase.is_none());
        assert!(q.deadline.is_none());
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
    fn in_relative_interval_is_literal() {
        let q = parse_at("Water plants in 5 days", anchor()).unwrap();
        assert_eq!(q.title, "Water plants in 5 days");
        assert!(q.deadline_phrase.is_none());
        assert!(q.deadline.is_none());
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
