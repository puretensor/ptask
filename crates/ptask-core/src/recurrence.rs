//! Recurrence rules.
//!
//! Supports the common Todoist "every X" / "every! X" patterns:
//!
//! - `every day`
//! - `every N days` / `every N weeks` / `every N months`
//! - `every weekday`   (Mon-Fri)
//! - `every monday`, `every mon`, etc.
//! - `every monday, wednesday, friday`
//!
//! `every X`  → mode = `Fixed` (next instance scheduled from the original due).
//! `every! X` → mode = `Completion` (next instance scheduled from completion time).
//!
//! Storage: serialise as an RRULE-style string in `pt_recurrence.rrule`,
//! e.g. `FREQ=WEEKLY;BYDAY=MO,WE,FR;INTERVAL=1`. We do not currently round-
//! trip arbitrary RFC 5545 — the string is descriptive metadata for now,
//! and `next_after` works directly off the parsed [`Recurrence`] struct.

use crate::error::{Error, Result};
use jiff::Zoned;
use jiff::civil::Weekday;

/// Whether the next instance is scheduled relative to the *original due*
/// (`Fixed`) or relative to the *completion time* (`Completion`).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Mode {
    Fixed,
    Completion,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recurrence {
    pub mode: Mode,
    pub freq: Freq,
    pub interval: u32,
    pub bydays: Vec<Weekday>,
    pub bymonthday: Vec<i8>,
    pub original_input: String,
    pub rrule_str: String,
}

/// Split an optional trailing ` at <time>` suffix from a recurrence phrase.
///
/// The returned rule part is what the recurrence grammar consumes. The full
/// original phrase can still be persisted for operator-facing audit/debug
/// output.
pub fn split_time_suffix(input: &str) -> (&str, Option<&str>) {
    let trimmed = input.trim();
    let lower = trimmed.to_ascii_lowercase();
    if let Some(at_idx) = lower.rfind(" at ") {
        let rule = trimmed[..at_idx].trim_end();
        let time = trimmed[at_idx + 4..].trim();
        if !time.is_empty() {
            return (rule, Some(time));
        }
    }
    (trimmed, None)
}

/// Parse a recurrence phrase. Accepts an explicit leading marker:
/// - "every X"  → Fixed
/// - "every! X" → Completion
/// - bare "X"   → Fixed (caller may strip the prefix beforehand)
pub fn parse(input: &str) -> Result<Recurrence> {
    let trimmed = input.trim();
    let (rule, _time) = split_time_suffix(trimmed);
    let (mode, rest) = if let Some(r) = rule.strip_prefix("every!") {
        (Mode::Completion, r.trim())
    } else if let Some(r) = rule.strip_prefix("every") {
        (Mode::Fixed, r.trim())
    } else {
        (Mode::Fixed, rule)
    };
    if rest.is_empty() {
        return Err(Error::Other(format!(
            "recurrence: empty rule after 'every': {:?}",
            input
        )));
    }
    let lower = rest.to_ascii_lowercase();
    let original = trimmed.to_string();

    // every day, every weekday
    if lower == "day" || lower == "days" {
        return Ok(build(mode, Freq::Daily, 1, vec![], vec![], original));
    }
    if lower == "weekday" || lower == "workday" {
        return Ok(build(
            mode,
            Freq::Weekly,
            1,
            vec![
                Weekday::Monday,
                Weekday::Tuesday,
                Weekday::Wednesday,
                Weekday::Thursday,
                Weekday::Friday,
            ],
            vec![],
            original,
        ));
    }
    if lower == "week" || lower == "weeks" {
        return Ok(build(mode, Freq::Weekly, 1, vec![], vec![], original));
    }
    if lower == "month" || lower == "months" {
        return Ok(build(mode, Freq::Monthly, 1, vec![], vec![], original));
    }

    // every N <unit>
    if let Some((n_str, unit)) = lower.split_once(' ')
        && let Ok(n) = n_str.parse::<u32>()
    {
        let freq = match unit.trim_end_matches('s') {
            "day" => Some(Freq::Daily),
            "week" => Some(Freq::Weekly),
            "month" => Some(Freq::Monthly),
            _ => None,
        };
        if let Some(f) = freq {
            if n == 0 {
                return Err(Error::Other(format!(
                    "recurrence: interval must be ≥ 1: {:?}",
                    input
                )));
            }
            return Ok(build(mode, f, n, vec![], vec![], original));
        }
    }

    // every <weekday>[, <weekday>...]
    let parts: Vec<&str> = lower.split(',').map(|s| s.trim()).collect();
    if !parts.is_empty() && parts.iter().all(|p| weekday_from_word(p).is_some()) {
        let days: Vec<Weekday> = parts
            .iter()
            .map(|p| weekday_from_word(p).unwrap())
            .collect();
        return Ok(build(mode, Freq::Weekly, 1, days, vec![], original));
    }

    // every <N>[, <N>...] (days of month, e.g. "every 1, 15, 27")
    if !parts.is_empty()
        && parts
            .iter()
            .all(|p| p.parse::<i8>().ok().is_some_and(|n| (1..=31).contains(&n)))
    {
        let mds: Vec<i8> = parts.iter().map(|p| p.parse().unwrap()).collect();
        return Ok(build(mode, Freq::Monthly, 1, vec![], mds, original));
    }

    Err(Error::Other(format!(
        "recurrence: unrecognised pattern {:?} (try 'every day', 'every weekday', \
         'every monday', 'every 3 days', 'every mon, wed, fri', 'every 1, 15')",
        input
    )))
}

fn build(
    mode: Mode,
    freq: Freq,
    interval: u32,
    bydays: Vec<Weekday>,
    bymonthday: Vec<i8>,
    original: String,
) -> Recurrence {
    let freq_str = match freq {
        Freq::Daily => "DAILY",
        Freq::Weekly => "WEEKLY",
        Freq::Monthly => "MONTHLY",
    };
    let mut parts = vec![format!("FREQ={}", freq_str)];
    if interval != 1 {
        parts.push(format!("INTERVAL={}", interval));
    }
    if !bydays.is_empty() {
        let s = bydays
            .iter()
            .map(|d| weekday_to_rfc(*d).to_string())
            .collect::<Vec<_>>()
            .join(",");
        parts.push(format!("BYDAY={}", s));
    }
    if !bymonthday.is_empty() {
        let s = bymonthday
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        parts.push(format!("BYMONTHDAY={}", s));
    }
    Recurrence {
        mode,
        freq,
        interval,
        bydays,
        bymonthday,
        original_input: original,
        rrule_str: parts.join(";"),
    }
}

/// Compute the next occurrence strictly after `after`.
pub fn next_after(rec: &Recurrence, after: &Zoned) -> Result<Zoned> {
    match rec.freq {
        Freq::Daily => after
            .checked_add(jiff::Span::new().days(rec.interval as i64))
            .map_err(|e| Error::Other(format!("recurrence advance: {}", e))),
        Freq::Weekly if !rec.bydays.is_empty() => next_weekday(after, &rec.bydays),
        Freq::Weekly => after
            .checked_add(jiff::Span::new().weeks(rec.interval as i64))
            .map_err(|e| Error::Other(format!("recurrence advance: {}", e))),
        Freq::Monthly if !rec.bymonthday.is_empty() => next_monthday(after, &rec.bymonthday),
        Freq::Monthly => after
            .checked_add(jiff::Span::new().months(rec.interval as i64))
            .map_err(|e| Error::Other(format!("recurrence advance: {}", e))),
    }
}

fn next_weekday(after: &Zoned, days: &[Weekday]) -> Result<Zoned> {
    // Walk forward day-by-day up to 14 days (handles every-2-weeks edge fully).
    for delta in 1..=14 {
        let candidate = after
            .checked_add(jiff::Span::new().days(delta))
            .map_err(|e| Error::Other(format!("weekday advance: {}", e)))?;
        if days.contains(&candidate.weekday()) {
            return Ok(candidate);
        }
    }
    Err(Error::Other(
        "weekday advance: no match within 14 days (bug?)".into(),
    ))
}

fn next_monthday(after: &Zoned, days: &[i8]) -> Result<Zoned> {
    // Walk forward day-by-day up to a year. A 60-day window misses valid
    // "every 31" advances such as Mar 31 -> May 31 across a 30-day month.
    for delta in 1..=370 {
        let candidate = after
            .checked_add(jiff::Span::new().days(delta))
            .map_err(|e| Error::Other(format!("monthday advance: {}", e)))?;
        if days.contains(&(candidate.day())) {
            return Ok(candidate);
        }
    }
    Err(Error::Other(
        "monthday advance: no match within one year".into(),
    ))
}

fn weekday_from_word(w: &str) -> Option<Weekday> {
    Some(match w {
        "monday" | "mon" => Weekday::Monday,
        "tuesday" | "tue" | "tues" => Weekday::Tuesday,
        "wednesday" | "wed" => Weekday::Wednesday,
        "thursday" | "thu" | "thur" | "thurs" => Weekday::Thursday,
        "friday" | "fri" => Weekday::Friday,
        "saturday" | "sat" => Weekday::Saturday,
        "sunday" | "sun" => Weekday::Sunday,
        _ => return None,
    })
}

fn weekday_to_rfc(w: Weekday) -> &'static str {
    match w {
        Weekday::Monday => "MO",
        Weekday::Tuesday => "TU",
        Weekday::Wednesday => "WE",
        Weekday::Thursday => "TH",
        Weekday::Friday => "FR",
        Weekday::Saturday => "SA",
        Weekday::Sunday => "SU",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates;
    use jiff::civil::date;

    fn anchor() -> Zoned {
        // Wednesday, 2026-05-13 12:00 London.
        let tz = jiff::tz::TimeZone::get(dates::OPERATOR_TZ).unwrap();
        date(2026, 5, 13).at(12, 0, 0, 0).to_zoned(tz).unwrap()
    }

    #[test]
    fn every_day_fixed() {
        let r = parse("every day").unwrap();
        assert_eq!(r.mode, Mode::Fixed);
        assert_eq!(r.freq, Freq::Daily);
        assert_eq!(r.rrule_str, "FREQ=DAILY");
    }

    #[test]
    fn every_bang_5_days_completion_mode() {
        let r = parse("every! 5 days").unwrap();
        assert_eq!(r.mode, Mode::Completion);
        assert_eq!(r.interval, 5);
        assert_eq!(r.rrule_str, "FREQ=DAILY;INTERVAL=5");
    }

    #[test]
    fn every_weekday() {
        let r = parse("every weekday").unwrap();
        assert_eq!(r.bydays.len(), 5);
        assert!(r.rrule_str.contains("BYDAY=MO,TU,WE,TH,FR"));
    }

    #[test]
    fn every_monday_wednesday_friday() {
        let r = parse("every mon, wed, fri").unwrap();
        assert_eq!(
            r.bydays,
            vec![Weekday::Monday, Weekday::Wednesday, Weekday::Friday]
        );
        assert_eq!(r.rrule_str, "FREQ=WEEKLY;BYDAY=MO,WE,FR");
    }

    #[test]
    fn every_monday_at_time_preserves_original_but_parses_rule() {
        let r = parse("every monday at 9am").unwrap();
        assert_eq!(r.original_input, "every monday at 9am");
        assert_eq!(r.bydays, vec![Weekday::Monday]);
        assert_eq!(r.rrule_str, "FREQ=WEEKLY;BYDAY=MO");
    }

    #[test]
    fn every_n_weeks() {
        let r = parse("every 2 weeks").unwrap();
        assert_eq!(r.freq, Freq::Weekly);
        assert_eq!(r.interval, 2);
        assert_eq!(r.rrule_str, "FREQ=WEEKLY;INTERVAL=2");
    }

    #[test]
    fn every_n_months() {
        let r = parse("every 3 months").unwrap();
        assert_eq!(r.freq, Freq::Monthly);
        assert_eq!(r.interval, 3);
    }

    #[test]
    fn every_specific_monthdays() {
        let r = parse("every 1, 15, 27").unwrap();
        assert_eq!(r.freq, Freq::Monthly);
        assert_eq!(r.bymonthday, vec![1, 15, 27]);
        assert!(r.rrule_str.contains("BYMONTHDAY=1,15,27"));
    }

    #[test]
    fn unknown_pattern_errors() {
        assert!(parse("every never").is_err());
    }

    #[test]
    fn next_after_daily() {
        let r = parse("every 5 days").unwrap();
        let n = next_after(&r, &anchor()).unwrap();
        assert_eq!(n.date(), date(2026, 5, 18));
    }

    #[test]
    fn next_after_weekday_lands_on_friday() {
        // Wednesday anchor; next weekday MWF should be Friday.
        let r = parse("every mon, wed, fri").unwrap();
        let n = next_after(&r, &anchor()).unwrap();
        assert_eq!(n.date(), date(2026, 5, 15));
    }

    #[test]
    fn next_after_weekday_skips_to_monday_from_friday() {
        let r = parse("every monday").unwrap();
        let tz = jiff::tz::TimeZone::get(dates::OPERATOR_TZ).unwrap();
        let friday = date(2026, 5, 15).at(12, 0, 0, 0).to_zoned(tz).unwrap();
        let n = next_after(&r, &friday).unwrap();
        assert_eq!(n.date(), date(2026, 5, 18));
    }

    #[test]
    fn next_after_monthday_15_from_may_13() {
        let r = parse("every 1, 15, 27").unwrap();
        let n = next_after(&r, &anchor()).unwrap();
        assert_eq!(n.date(), date(2026, 5, 15));
    }

    #[test]
    fn next_after_monthday_31_skips_short_months() {
        let r = parse("every 31").unwrap();
        let tz = jiff::tz::TimeZone::get(dates::OPERATOR_TZ).unwrap();
        let mar31 = date(2026, 3, 31).at(12, 0, 0, 0).to_zoned(tz).unwrap();
        let n = next_after(&r, &mar31).unwrap();
        assert_eq!(n.date(), date(2026, 5, 31));
    }

    #[test]
    fn next_after_monthly_default() {
        // "every month" with no bymonthday picks the same day next month.
        let r = parse("every month").unwrap();
        let n = next_after(&r, &anchor()).unwrap();
        assert_eq!(n.date().month(), 6);
        assert_eq!(n.date().day(), 13);
    }
}
