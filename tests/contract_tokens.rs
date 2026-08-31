//! Contract tests for API token hashing and scope parsing.
//!
//! Code under test:
//!   - `crates/ptask-core/src/tokens.rs:62-66` (`hash_token`)
//!   - `crates/ptask-core/src/tokens.rs:26-34` (`Scope::parse`)

use ptask_core::tokens::{Scope, hash_token};

#[test]
fn hash_token_empty_string_is_sha256_of_empty() {
    assert_eq!(
        hash_token(""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn hash_token_is_deterministic_lowercase_hex() {
    let once = hash_token("pt_deadbeef");
    let twice = hash_token("pt_deadbeef");
    assert_eq!(once, twice);
    assert_eq!(once.len(), 64);
    assert!(once.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
}

#[test]
fn hash_token_utf8_bytes_not_normalised() {
    let plain = "pt_café";
    let nfc = "pt_caf\u{e9}";
    let nfd = "pt_cafe\u{0301}";
    assert_ne!(hash_token(nfc), hash_token(nfd));
    assert_eq!(hash_token(plain), hash_token(nfc));
}

#[test]
fn scope_parse_accepts_trimmed_case_insensitive_labels() {
    assert_eq!(Scope::parse("read"), Some(Scope::Read));
    assert_eq!(Scope::parse("  CAPTURE  "), Some(Scope::Capture));
    assert_eq!(Scope::parse("Write"), Some(Scope::Write));
    assert_eq!(Scope::parse("ADMIN"), Some(Scope::Admin));
}

#[test]
fn scope_parse_rejects_unknown_and_empty() {
    assert_eq!(Scope::parse(""), None);
    assert_eq!(Scope::parse("   "), None);
    assert_eq!(Scope::parse("readonly"), None);
    assert_eq!(Scope::parse("superadmin"), None);
    assert_eq!(Scope::parse("🔐"), None);
}
