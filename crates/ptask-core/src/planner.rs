//! Advisory day planner: fit the ready-task queue into free calendar slots.
//!
//! `pack()` is a pure integer first-fit (no I/O, no time parsing) so it is
//! trivially unit-testable. The CLI (`pt plan`) sources free slots from
//! `gcalendar.py freebusy --json` and formats the placements into wall-clock
//! times. Nothing here mutates the calendar or the task DB — planning is
//! advisory; only `pt plan --write` (in the CLI) creates tentative events.

use crate::error::Result;
use crate::storage::Db;
use serde::Serialize;

/// A ready task eligible for scheduling, with its estimated duration.
#[derive(Debug, Clone, Serialize)]
pub struct PlanCandidate {
    pub pt_id: Option<String>,
    pub title: String,
    pub duration_min: i64,
    pub energy: Option<String>,
}

/// Dependency-met, non-snoozed ready tasks in priority order, each carrying a
/// duration (NULL/zero durations default to `slot_default_min`). Mirrors
/// `dag::next_ready`'s filter + ordering.
pub fn ready_candidates(db: &Db, limit: usize, slot_default_min: i64) -> Result<Vec<PlanCandidate>> {
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT t.pt_id, t.title, t.duration_min, t.energy,
                (SELECT COUNT(*) FROM task_links l JOIN tasks d ON d.id = l.to_uuid
                 WHERE l.from_uuid = t.id AND l.kind = 'depends_on'
                   AND d.status_v2 != 'done') AS unmet
         FROM tasks t
         WHERE t.status_v2 IN ('triage','backlog','todo','in_progress')
         ORDER BY t.priority_score DESC, t.priority DESC, t.created_at DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        let pt_id: Option<String> = r.get(0)?;
        let title: String = r.get(1)?;
        let duration_min: Option<i64> = r.get(2)?;
        let energy: Option<String> = r.get(3)?;
        let unmet: i64 = r.get(4)?;
        let dur = duration_min.filter(|d| *d > 0).unwrap_or(slot_default_min);
        Ok((PlanCandidate { pt_id, title, duration_min: dur, energy }, unmet))
    })?;
    let mut out = Vec::new();
    for entry in rows {
        let (cand, unmet) = entry?;
        if unmet == 0 {
            out.push(cand);
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(out)
}

/// A placement of candidate `cand` into free slot `slot` at `offset_min` from
/// the slot's start, occupying `duration_min`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub cand: usize,
    pub slot: usize,
    pub offset_min: i64,
    pub duration_min: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackResult {
    pub scheduled: Vec<Placement>,
    pub unscheduled: Vec<usize>,
}

/// Greedy first-fit: walk candidates in priority order, placing each into the
/// earliest free slot with enough remaining capacity. `slot_caps[i]` is slot
/// i's capacity in minutes (slots assumed chronological). A candidate that
/// fits no remaining slot goes to `unscheduled`. Pure — no I/O.
pub fn pack(candidates: &[PlanCandidate], slot_caps: &[i64]) -> PackResult {
    let mut used: Vec<i64> = vec![0; slot_caps.len()];
    let mut scheduled = Vec::new();
    let mut unscheduled = Vec::new();
    for (ci, c) in candidates.iter().enumerate() {
        let dur = c.duration_min.max(1);
        let mut placed = false;
        for (si, cap) in slot_caps.iter().enumerate() {
            if cap - used[si] >= dur {
                scheduled.push(Placement {
                    cand: ci,
                    slot: si,
                    offset_min: used[si],
                    duration_min: dur,
                });
                used[si] += dur;
                placed = true;
                break;
            }
        }
        if !placed {
            unscheduled.push(ci);
        }
    }
    PackResult { scheduled, unscheduled }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(title: &str, dur: i64) -> PlanCandidate {
        PlanCandidate {
            pt_id: None,
            title: title.into(),
            duration_min: dur,
            energy: None,
        }
    }

    #[test]
    fn fits_sequential_into_one_slot() {
        let c = vec![cand("a", 30), cand("b", 60)];
        let r = pack(&c, &[480]);
        assert!(r.unscheduled.is_empty());
        assert_eq!(r.scheduled[0].offset_min, 0);
        assert_eq!(r.scheduled[1].offset_min, 30);
        assert_eq!(r.scheduled[1].slot, 0);
    }

    #[test]
    fn overflow_spills_to_unscheduled() {
        let c = vec![cand("a", 300), cand("b", 300)];
        let r = pack(&c, &[480]);
        assert_eq!(r.scheduled.len(), 1);
        assert_eq!(r.unscheduled, vec![1]);
    }

    #[test]
    fn second_slot_used_when_first_full() {
        let c = vec![cand("a", 400), cand("b", 400)];
        let r = pack(&c, &[480, 480]);
        assert_eq!(r.scheduled.len(), 2);
        assert_eq!(r.scheduled[0].slot, 0);
        assert_eq!(r.scheduled[1].slot, 1);
    }

    #[test]
    fn working_hours_boundary_respected() {
        // one 8h slot; three 3h tasks -> two fit (6h), third spills
        let c = vec![cand("a", 180), cand("b", 180), cand("c", 180)];
        let r = pack(&c, &[480]);
        assert_eq!(r.scheduled.len(), 2);
        assert_eq!(r.unscheduled, vec![2]);
    }

    #[test]
    fn empty_queue_and_zero_slots() {
        assert!(pack(&[], &[480]).scheduled.is_empty());
        assert_eq!(pack(&[cand("a", 30)], &[]).unscheduled, vec![0]);
    }
}
