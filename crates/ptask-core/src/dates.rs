//! Natural-language date parsing.
//!
//! Wraps `interim` (UK dialect) with `jiff::Zoned` as the time type. All
//! parses are anchored to the operator timezone (Europe/London) unless an
//! explicit base time is supplied. Returns `Zoned` so callers can format
//! identically to `tasks::iso_now()` (i.e. `+00:00` offset, 6-digit micros).
//!
//! Examples that parse:
//!   "today", "tomorrow", "yesterday"
//!   "next friday", "this monday", "last sunday"
//!   "3 days", "2 weeks", "3 hours"   (no "in" prefix — interim convention)
//!   "2026-05-20", "20/05/2026", "20 May 2026"
//!   "monday 9am", "next friday 8pm", "tomorrow 10:30"
//!   "5pm", "17:00"

use crate::error::{Error, Result};
use interim::{Dialect, parse_date_string};
use jiff::Zoned;

/// Operator timezone. All natural-language parses anchor here.
pub const OPERATOR_TZ: &str = "Europe/London";

/// `now()` in the operator timezone.
pub fn now_in_operator_tz() -> Result<Zoned> {
    let tz = jiff::tz::TimeZone::get(OPERATOR_TZ)
        .map_err(|e| Error::Other(format!("loading {} tz: {}", OPERATOR_TZ, e)))?;
    Ok(Zoned::now().with_time_zone(tz))
}

/// Parse a natural-language date phrase using `now` as the relative anchor.
/// UK dialect (`Dialect::Uk` — "next friday" means the *following* week's
/// Friday, "01/04/26" is 1 April 2026, etc.).
///
/// Panic-free by contract: `interim` 0.2.1 panics on inputs it should merely
/// reject — a lowercase assertion in its scanner (types.rs:254, hit by
/// mixed-case phrases like "Sat 18 Jul") and other parser paths (e.g. a long
/// identifier after an hour number, parser.rs:459). A date *guess* must never
/// abort the process: pre-fix this killed `pt add` and silently killed the
/// MCP server's request task, hanging the client with no response (PT-1270).
/// Input is lowercased up front (interim's date vocabulary is pure ASCII) and
/// any residual panic is caught and mapped to an ordinary parse error.
pub fn parse_at(input: &str, now: Zoned) -> Result<Zoned> {
    let normalised = input.trim().to_ascii_lowercase();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        parse_date_string(&normalised, now, Dialect::Uk)
    }))
    .map_err(|_| {
        Error::Other(format!(
            "date parse failed for {:?}: parser panicked",
            input
        ))
    })?
    .map_err(|e| Error::Other(format!("date parse failed for {:?}: {}", input, e)))
}

/// Parse a phrase against `now_in_operator_tz()`. Convenience for callers
/// that don't have their own time source.
pub fn parse(input: &str) -> Result<Zoned> {
    parse_at(input, now_in_operator_tz()?)
}

/// Parse one of the timestamp shapes used by the legacy SQLite store and
/// return it in UTC. Handles Python `datetime.isoformat()` strings with an
/// offset, bare UTC timestamps from SQLite `datetime('now')`, and date-only
/// values as midnight UTC.
pub fn parse_iso_to_utc(input: &str) -> Option<Zoned> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let utc = jiff::tz::TimeZone::UTC;
    if let Ok(z) = s.parse::<Zoned>() {
        return Some(z.with_time_zone(utc));
    }
    if let Ok(t) = s.parse::<jiff::Timestamp>() {
        return Some(t.to_zoned(utc));
    }
    // SQLite `datetime('now', ...)` returns 'YYYY-MM-DD HH:MM:SS' (UTC, no offset).
    if let Ok(t) = format!("{}Z", s.replace(' ', "T")).parse::<jiff::Timestamp>() {
        return Some(t.to_zoned(utc));
    }
    // Date-only fallback.
    if !s.contains('T')
        && !s.contains(' ')
        && let Ok(t) = format!("{}T00:00:00Z", s).parse::<jiff::Timestamp>()
    {
        return Some(t.to_zoned(utc));
    }
    None
}

/// Format a `Zoned` as the canonical pTask ISO string.
/// Matches Python `datetime.now(timezone.utc).isoformat()` exactly:
/// `YYYY-MM-DDTHH:MM:SS.ffffff+HH:MM` (or `+00:00` for UTC).
pub fn format_iso(z: &Zoned) -> String {
    let base = z.strftime("%Y-%m-%dT%H:%M:%S").to_string();
    let micros = z.subsec_nanosecond().div_euclid(1_000);
    let offset = z.offset();
    let off_secs = offset.seconds();
    let off_sign = if off_secs < 0 { '-' } else { '+' };
    let off_abs = off_secs.unsigned_abs() as i64;
    let off_h = off_abs / 3600;
    let off_m = (off_abs % 3600) / 60;
    if micros == 0 {
        format!("{base}{off_sign}{off_h:02}:{off_m:02}")
    } else {
        format!("{base}.{micros:06}{off_sign}{off_h:02}:{off_m:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn anchor() -> Zoned {
        // Wednesday, 2026-05-13 12:00 London time.
        let tz = jiff::tz::TimeZone::get(OPERATOR_TZ).unwrap();
        date(2026, 5, 13).at(12, 0, 0, 0).to_zoned(tz).unwrap()
    }

    #[test]
    fn today_returns_anchor_date() {
        let z = parse_at("today", anchor()).unwrap();
        assert_eq!(z.date(), date(2026, 5, 13));
    }

    #[test]
    fn tomorrow_advances_one_day() {
        let z = parse_at("tomorrow", anchor()).unwrap();
        assert_eq!(z.date(), date(2026, 5, 14));
    }

    #[test]
    fn yesterday_retreats_one_day() {
        let z = parse_at("yesterday", anchor()).unwrap();
        assert_eq!(z.date(), date(2026, 5, 12));
    }

    #[test]
    fn weekday_lookup_picks_upcoming() {
        // Wed → Fri (this week)
        let z = parse_at("friday", anchor()).unwrap();
        assert_eq!(z.date(), date(2026, 5, 15));
    }

    #[test]
    fn next_weekday_skips_to_next_week_uk() {
        // UK: "next friday" = following week's Friday (+7 from this Fri)
        let z = parse_at("next friday", anchor()).unwrap();
        assert_eq!(z.date(), date(2026, 5, 22));
    }

    #[test]
    fn last_weekday_picks_previous() {
        let z = parse_at("last monday", anchor()).unwrap();
        assert_eq!(z.date(), date(2026, 5, 11));
    }

    #[test]
    fn interval_n_days() {
        // interim's grammar is "N days", NOT "in N days".
        let z = parse_at("5 days", anchor()).unwrap();
        assert_eq!(z.date(), date(2026, 5, 18));
    }

    #[test]
    fn interval_in_prefix_rejected() {
        // Documented limitation: callers must strip "in " if they want a phrase
        // to parse. The inline-token quick-add layer (v0.1.3) handles this.
        assert!(parse_at("in 5 days", anchor()).is_err());
    }

    #[test]
    fn absolute_iso_date() {
        let z = parse_at("2026-12-25", anchor()).unwrap();
        assert_eq!(z.date(), date(2026, 12, 25));
    }

    #[test]
    fn time_appended_to_weekday() {
        let z = parse_at("friday 9am", anchor()).unwrap();
        assert_eq!(z.date(), date(2026, 5, 15));
        assert_eq!(z.hour(), 9);
        assert_eq!(z.minute(), 0);
    }

    #[test]
    fn format_iso_matches_python_isoformat_no_micros() {
        // 2026-05-13T12:00:00 London (BST = +01:00 in mid May)
        let z = anchor();
        let s = format_iso(&z);
        assert!(
            s == "2026-05-13T12:00:00+01:00" || s == "2026-05-13T12:00:00+00:00",
            "got {s}"
        );
    }

    #[test]
    fn format_iso_includes_six_digit_micros() {
        let tz = jiff::tz::TimeZone::get(OPERATOR_TZ).unwrap();
        let z = date(2026, 5, 13)
            .at(12, 0, 0, 123_456_000)
            .to_zoned(tz)
            .unwrap();
        let s = format_iso(&z);
        assert!(s.contains(".123456+"), "got {s}");
    }

    #[test]
    fn parse_iso_to_utc_handles_offset_timestamp() {
        let z = parse_iso_to_utc("2026-05-13T12:00:00+01:00").unwrap();
        assert_eq!(z.time_zone(), &jiff::tz::TimeZone::UTC);
        assert_eq!(z.hour(), 11);
        assert_eq!(z.minute(), 0);
    }

    #[test]
    fn parse_iso_to_utc_handles_sqlite_datetime() {
        let z = parse_iso_to_utc("2026-05-13 12:34:56").unwrap();
        assert_eq!(z.date(), date(2026, 5, 13));
        assert_eq!(z.hour(), 12);
        assert_eq!(z.minute(), 34);
        assert_eq!(z.second(), 56);
    }

    #[test]
    fn parse_iso_to_utc_handles_date_only() {
        let z = parse_iso_to_utc("2026-05-13").unwrap();
        assert_eq!(z.date(), date(2026, 5, 13));
        assert_eq!(z.hour(), 0);
        assert_eq!(z.minute(), 0);
    }

    #[test]
    fn garbage_returns_error() {
        let err = parse_at("not a date phrase at all", anchor());
        assert!(err.is_err());
    }

    #[test]
    fn mixed_case_phrase_is_normalised_not_a_panic() {
        // Regression (PT-1270): interim 0.2.1's Lowercase::literal asserts
        // every identifier byte is ascii-lowercase (types.rs:254), so
        // "Sat 18 Jul" aborted the whole process. Input is lowercased now —
        // the mixed-case phrase must behave exactly like its lowercase twin.
        let mixed = parse_at("Sat 18 Jul", anchor());
        let lower = parse_at("sat 18 jul", anchor());
        match (mixed, lower) {
            (Ok(m), Ok(l)) => assert_eq!(m, l),
            (Err(_), Err(_)) => {} // same rejection either way is fine
            (m, l) => panic!("case changed the outcome: {:?} vs {:?}", m, l),
        }
        // The plain day-month form must actually parse.
        let z = parse_at("18 Jul", anchor()).unwrap();
        assert_eq!(z.date(), date(2026, 7, 18));
    }

    #[test]
    fn long_identifier_after_hour_is_error_not_panic() {
        // Regression (PT-1270): interim's am/pm branch (parser.rs:459) feeds
        // the identifier after an hour number into Lowercase::literal, which
        // also asserts len < 16 — a 16+ char word panicked even in lowercase.
        // catch_unwind must convert that abort into an ordinary Err.
        let r = parse_at("tomorrow 5 internationalisation", anchor());
        assert!(r.is_err(), "expected parse error, got {:?}", r);
    }

    #[test]
    fn now_in_operator_tz_runs() {
        let z = now_in_operator_tz().unwrap();
        assert_eq!(z.time_zone().iana_name(), Some(OPERATOR_TZ));
    }
}
