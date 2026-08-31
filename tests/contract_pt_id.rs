//! Contract tests for PT-N identifier formatting (`ptask_core::pt_id::format_pt_id`).
//!
//! Code under test: `crates/ptask-core/src/pt_id.rs:14-16`

use ptask_core::pt_id::format_pt_id;

#[test]
fn format_pt_id_zero_and_positive_counters() {
    assert_eq!(format_pt_id(0), "PT-0");
    assert_eq!(format_pt_id(1), "PT-1");
    assert_eq!(format_pt_id(42), "PT-42");
    assert_eq!(format_pt_id(999_999), "PT-999999");
}

#[test]
fn format_pt_id_negative_and_max_i64() {
    // No validation today — callers must never pass negatives, but the
    // formatter blindly interpolates the integer.
    assert_eq!(format_pt_id(-1), "PT--1");
    assert_eq!(format_pt_id(i64::MIN), format!("PT-{}", i64::MIN));
    assert_eq!(format_pt_id(i64::MAX), format!("PT-{}", i64::MAX));
}

#[test]
fn format_pt_id_is_pure_and_stateless() {
    assert_eq!(format_pt_id(7), "PT-7");
    assert_eq!(format_pt_id(7), "PT-7");
}
