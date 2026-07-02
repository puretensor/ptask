//! The v2 status model (V010, schema v2).
//!
//! Eight checked states. The legacy `tasks.status` vocabulary
//! (pending/delayed/done/dismissed/blocked) remains the compat surface for
//! the not-yet-retired legacy consumers (Python distill writes it; the
//! dashboard sidecar and accountability SQL read it), so every write path
//! sets BOTH columns via [`legacy`]'s total mapping. The legacy column is
//! dropped when those consumers retire (Phases 5–7).

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Triage,
    Backlog,
    Todo,
    InProgress,
    Snoozed,
    Done,
    Dismissed,
    Blocked,
}

impl Status {
    pub fn parse(s: &str) -> Result<Status> {
        match s.trim().to_ascii_lowercase().as_str() {
            "triage" => Ok(Status::Triage),
            "backlog" => Ok(Status::Backlog),
            "todo" => Ok(Status::Todo),
            "in_progress" | "in-progress" | "doing" => Ok(Status::InProgress),
            "snoozed" => Ok(Status::Snoozed),
            "done" => Ok(Status::Done),
            "dismissed" => Ok(Status::Dismissed),
            "blocked" => Ok(Status::Blocked),
            other => Err(Error::Other(format!("unknown status {:?}", other))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Triage => "triage",
            Status::Backlog => "backlog",
            Status::Todo => "todo",
            Status::InProgress => "in_progress",
            Status::Snoozed => "snoozed",
            Status::Done => "done",
            Status::Dismissed => "dismissed",
            Status::Blocked => "blocked",
        }
    }

    /// Total mapping onto the legacy vocabulary the untouched consumers read.
    pub fn legacy(&self) -> &'static str {
        match self {
            Status::Triage | Status::Backlog | Status::Todo | Status::InProgress => "pending",
            Status::Snoozed => "delayed",
            Status::Done => "done",
            Status::Dismissed => "dismissed",
            Status::Blocked => "blocked",
        }
    }

    /// True when the task no longer competes for attention.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Status::Done | Status::Dismissed)
    }
}

/// The two columns every status write must set together.
pub fn columns(v2: Status) -> (&'static str, &'static str) {
    (v2.as_str(), v2.legacy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_is_total_and_roundtrips() {
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
            assert_eq!(Status::parse(s.as_str()).unwrap(), s);
            let (v2, legacy) = columns(s);
            assert_eq!(v2, s.as_str());
            assert!(["pending", "delayed", "done", "dismissed", "blocked"].contains(&legacy));
        }
    }
}
