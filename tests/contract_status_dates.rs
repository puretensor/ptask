//! Contract tests for the v2 status model and legacy timestamp parsing.
//!
//! Code under test:
//!   - `crates/ptask-core/src/status.rs:24-71` (`Status::parse`, `legacy`, `is_terminal`, `columns`)
//!   - `crates/ptask-core/src/dates.rs:66-90` (`parse_iso_to_utc`)

use ptask_core::dates::parse_iso_to_utc;
use ptask_core::status::{Status, columns};

#[test]
fn status_parse_aliases_and_whitespace() {
    assert_eq!(Status::parse("todo").unwrap(), Status::Todo);
    assert_eq!(Status::parse("IN_PROGRESS").unwrap(), Status::InProgress);
    assert_eq!(Status::parse("in-progress").unwrap(), Status::InProgress);
    assert_eq!(Status::parse(" doing ").unwrap(), Status::InProgress);
}

#[test]
fn status_parse_empty_and_unknown_error() {
    assert!(Status::parse("").is_err());
    assert!(Status::parse("   ").is_err());
    let err = Status::parse("shipped").unwrap_err().to_string();
    assert!(err.contains("unknown status"));
}

#[test]
fn status_is_terminal_only_done_and_dismissed() {
    let terminal = [Status::Done, Status::Dismissed];
    let non_terminal = [
        Status::Triage,
        Status::Backlog,
        Status::Todo,
        Status::InProgress,
        Status::Snoozed,
        Status::Blocked,
    ];
    for s in terminal {
        assert!(s.is_terminal());
    }
    for s in non_terminal {
        assert!(!s.is_terminal());
    }
}

#[test]
fn status_legacy_mapping_is_total() {
    let pending_like = [
        Status::Triage,
        Status::Backlog,
        Status::Todo,
        Status::InProgress,
    ];
    for s in pending_like {
        assert_eq!(s.legacy(), "pending");
    }
    assert_eq!(Status::Snoozed.legacy(), "delayed");
    assert_eq!(Status::Done.legacy(), "done");
    assert_eq!(Status::Dismissed.legacy(), "dismissed");
    assert_eq!(Status::Blocked.legacy(), "blocked");

    for s in [
        Status::Triage,
        Status::Backlog,
        Status::Todo,
        Status::InProgress,
        Status::Snoozed,
        Status::Done,
        Status::Dismissed,
        Status::Blocked,
    ] {
        let (v2, legacy) = columns(s);
        assert_eq!(v2, s.as_str());
        assert_eq!(legacy, s.legacy());
    }
}

#[test]
fn parse_iso_to_utc_empty_and_whitespace_returns_none() {
    assert_eq!(parse_iso_to_utc(""), None);
    assert_eq!(parse_iso_to_utc("   "), None);
    assert_eq!(parse_iso_to_utc("\t\n"), None);
}

#[test]
fn parse_iso_to_utc_garbage_and_impossible_dates_return_none() {
    assert_eq!(parse_iso_to_utc("not-a-timestamp"), None);
    assert_eq!(parse_iso_to_utc("2026-13-40"), None);
    assert_eq!(parse_iso_to_utc("🗓️"), None);
}

#[test]
fn parse_iso_to_utc_normalises_known_shapes_to_utc() {
    let z = parse_iso_to_utc("2026-05-13T12:00:00+01:00").expect("offset timestamp");
    assert_eq!(z.hour(), 11);
    assert_eq!(z.minute(), 0);

    let sqlite = parse_iso_to_utc("2026-05-13 12:34:56").expect("sqlite datetime");
    assert_eq!(sqlite.date().to_string(), "2026-05-13");
    assert_eq!(sqlite.hour(), 12);
    assert_eq!(sqlite.minute(), 34);

    let date_only = parse_iso_to_utc("2026-05-13").expect("date only");
    assert_eq!(date_only.date().to_string(), "2026-05-13");
    assert_eq!(date_only.hour(), 0);
}
