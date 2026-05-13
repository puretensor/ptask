//! Priority parsing — preserves the Python `PRIORITY_MAP` / `PRIORITY_LABEL` vocabulary.

use crate::error::{Error, Result};

/// Canonical priority labels in ascending urgency.
pub const LABELS: &[(&str, i64)] = &[
    ("low", 1),
    ("normal", 2),
    ("high", 3),
    ("urgent", 4),
    ("critical", 5),
];

/// Parse either an integer 1..=5 or one of the label strings (case-insensitive).
pub fn parse(input: &str) -> Result<i64> {
    let trimmed = input.trim();
    if let Ok(n) = trimmed.parse::<i64>() {
        if (1..=5).contains(&n) {
            return Ok(n);
        }
        return Err(Error::Other(format!("priority {} out of range 1..=5", n)));
    }
    let lower = trimmed.to_ascii_lowercase();
    for (label, n) in LABELS {
        if *label == lower {
            return Ok(*n);
        }
    }
    Err(Error::Other(format!(
        "unknown priority '{}' — expected one of low|normal|high|urgent|critical or 1..=5",
        input
    )))
}

/// Get the label for an integer priority. Returns "?" if out of range.
pub fn label(p: i64) -> &'static str {
    for (label, n) in LABELS {
        if *n == p {
            return label;
        }
    }
    "?"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_labels_case_insensitive() {
        assert_eq!(parse("low").unwrap(), 1);
        assert_eq!(parse("NORMAL").unwrap(), 2);
        assert_eq!(parse(" High ").unwrap(), 3);
        assert_eq!(parse("urgent").unwrap(), 4);
        assert_eq!(parse("Critical").unwrap(), 5);
    }

    #[test]
    fn parses_integers() {
        for n in 1..=5 {
            assert_eq!(parse(&n.to_string()).unwrap(), n);
        }
    }

    #[test]
    fn rejects_out_of_range() {
        assert!(parse("0").is_err());
        assert!(parse("6").is_err());
        assert!(parse("hot").is_err());
    }

    #[test]
    fn labels_round_trip() {
        for (lab, n) in LABELS {
            assert_eq!(label(*n), *lab);
        }
        assert_eq!(label(99), "?");
    }
}
