//! Contract tests for recurrence phrase time-suffix splitting.
//!
//! Code under test: `crates/ptask-core/src/recurrence.rs:54-65` (`split_time_suffix`)

use ptask_core::recurrence::split_time_suffix;

#[test]
fn split_time_suffix_extracts_trailing_at_time() {
    assert_eq!(
        split_time_suffix("every monday at 9am"),
        ("every monday", Some("9am"))
    );
    assert_eq!(
        split_time_suffix("  every! 3 days at 17:00  "),
        ("every! 3 days", Some("17:00"))
    );
}

#[test]
fn split_time_suffix_empty_and_whitespace_only() {
    assert_eq!(split_time_suffix(""), ("", None));
    assert_eq!(split_time_suffix("   "), ("", None));
}

#[test]
fn split_time_suffix_without_at_returns_whole_phrase() {
    assert_eq!(split_time_suffix("every 5 days"), ("every 5 days", None));
    assert_eq!(split_time_suffix("every weekday"), ("every weekday", None));
}

#[test]
fn split_time_suffix_trailing_at_without_time_keeps_at_in_rule() {
    // Trailing whitespace is trimmed first; a bare "... at" (no time token) does
    // not match the " at " separator pattern, so the whole phrase is returned.
    assert_eq!(
        split_time_suffix("every monday at "),
        ("every monday at", None)
    );
    assert_eq!(split_time_suffix("every monday at   "), ("every monday at", None));
}

#[test]
fn split_time_suffix_uses_last_at_separator() {
    assert_eq!(
        split_time_suffix("every mon at 5pm at 6pm"),
        ("every mon at 5pm", Some("6pm"))
    );
}

#[test]
fn split_time_suffix_preserves_casing_in_rule_part() {
    let (rule, time) = split_time_suffix("Every Monday At 9AM");
    assert_eq!(rule, "Every Monday");
    assert_eq!(time, Some("9AM"));
}
