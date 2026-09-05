//! Canonical list ordering.
//!
//! **Severity is the primary key.** `tasks.priority` (1 = low … 5 = critical)
//! decides the band a task lists in; the composite `priority_score` only breaks
//! ties *inside* a band.
//!
//! Before v3.24.0 every listing surface ordered by `priority_score DESC,
//! priority DESC`. Because the composite mixes urgency, dependency centrality
//! and neglect with only a 0.30 manual weight (see [`crate::scoring`]), a
//! NORMAL task that was old and neglected outscored a fresh CRITICAL one — so
//! neither `pt list` nor the dashboard actually read as severity-ordered. The
//! composite is still available as [`SortKey::Score`] for callers that want the
//! "what should I work on" ranking rather than the severity board.
//!
//! Every ORDER BY fragment here is a compile-time constant. Nothing
//! user-supplied is ever spliced into SQL: callers map an untrusted string
//! through [`SortKey::parse`] first.

use crate::error::{Error, Result};

/// How a task list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// Severity band first, composite score within the band. The default
    /// everywhere a list is shown to a human.
    #[default]
    Severity,
    /// Composite priority score first — the ranking that deliberately lets
    /// urgency and neglect outrank severity.
    Score,
    /// Newest first.
    Created,
}

impl SortKey {
    /// Every key, in the order a UI should offer them.
    pub const ALL: [SortKey; 3] = [SortKey::Severity, SortKey::Score, SortKey::Created];

    /// Stable wire/CLI name.
    pub const fn as_str(self) -> &'static str {
        match self {
            SortKey::Severity => "severity",
            SortKey::Score => "score",
            SortKey::Created => "created",
        }
    }

    /// `ORDER BY` fragment for queries that alias `tasks` as `t`.
    pub const fn sql(self) -> &'static str {
        match self {
            SortKey::Severity => {
                "t.priority DESC, t.priority_score DESC, t.created_at DESC, t.id DESC"
            }
            SortKey::Score => {
                "t.priority_score DESC, t.priority DESC, t.created_at DESC, t.id DESC"
            }
            SortKey::Created => "t.created_at DESC, t.id DESC",
        }
    }

    /// `ORDER BY` fragment for queries that select from `tasks` unaliased.
    pub const fn sql_bare(self) -> &'static str {
        match self {
            SortKey::Severity => "priority DESC, priority_score DESC, created_at DESC, id DESC",
            SortKey::Score => "priority_score DESC, priority DESC, created_at DESC, id DESC",
            SortKey::Created => "created_at DESC, id DESC",
        }
    }

    /// Parse a CLI/query-string value. Case-insensitive; `priority` is accepted
    /// as a synonym for `severity` because that is the column's name.
    pub fn parse(input: &str) -> Result<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "severity" | "priority" => Ok(SortKey::Severity),
            "score" => Ok(SortKey::Score),
            "created" => Ok(SortKey::Created),
            other => Err(Error::Other(format!(
                "unknown sort '{}' — expected one of severity|score|created",
                other
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_is_the_default() {
        assert_eq!(SortKey::default(), SortKey::Severity);
    }

    #[test]
    fn severity_orders_priority_before_score() {
        // The regression this module exists to prevent: the composite score
        // must never be the leading term of the severity ordering.
        for sql in [SortKey::Severity.sql(), SortKey::Severity.sql_bare()] {
            let p = sql.find("priority DESC").expect("orders by priority");
            let s = sql.find("priority_score DESC").expect("tiebreaks on score");
            assert!(p < s, "severity sort must lead with priority: {sql}");
        }
    }

    #[test]
    fn every_key_is_total() {
        // A non-total ORDER BY makes pagination and test assertions flaky.
        for k in SortKey::ALL {
            assert!(k.sql().ends_with("t.id DESC"), "{} not total", k.as_str());
            assert!(
                k.sql_bare().ends_with("id DESC"),
                "{} not total",
                k.as_str()
            );
        }
    }

    #[test]
    fn parses_names_and_rejects_junk() {
        assert_eq!(SortKey::parse("Severity").unwrap(), SortKey::Severity);
        assert_eq!(SortKey::parse(" priority ").unwrap(), SortKey::Severity);
        assert_eq!(SortKey::parse("score").unwrap(), SortKey::Score);
        assert_eq!(SortKey::parse("created").unwrap(), SortKey::Created);
        assert!(SortKey::parse("priority_score DESC; DROP TABLE tasks").is_err());
        assert!(SortKey::parse("").is_err());
    }

    #[test]
    fn names_round_trip() {
        for k in SortKey::ALL {
            assert_eq!(SortKey::parse(k.as_str()).unwrap(), k);
        }
    }
}
