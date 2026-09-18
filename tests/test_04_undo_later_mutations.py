"""Finding 4 — undo must not delete a task mutated after create.

The reviewed SQL only loaded task.completed / task.created / task.updated.
A later task.promoted, task.claimed or task.recurrence_advanced was
invisible, so the scan walked back to task.created and deleted the row.
HEAD already loads every event and only the newest event per task is
eligible — this test pins that behaviour.
"""

from __future__ import annotations

from source import fn_body, read

PRE_FIX_TYPES = ("task.completed", "task.created", "task.updated")
PROTECTING = ("task.promoted", "task.claimed", "task.recurrence_advanced")


def undo_deletes_create(events: list[tuple[str, str]], *, type_filter: tuple[str, ...] | None) -> bool:
    """events: newest-first (id, type) pairs for one task.

    Pre-fix filtered the SQL, so later mutation types never entered the scan.
    HEAD considers every event and only the newest one is eligible.
    """
    visible = [et for et in events if type_filter is None or et in type_filter]
    if not visible:
        return False
    newest = visible[0]
    return newest == "task.created"


def undo_sql_loads_all_events(src: str) -> bool:
    body = fn_body(src, "undo_last")
    newest_wins = "seen.insert" in body or "newest event" in body.lower()
    if "event_type IN" in body:
        return newest_wins and all(t in body for t in PROTECTING)
    return newest_wins and "FROM pt_event_log" in body and "ORDER BY id DESC" in body


def test_pre_fix_filter_deletes_a_promoted_task():
    """Negative control: create then promote looks like a bare create."""
    events = ["task.promoted", "task.created"]
    assert undo_deletes_create(events, type_filter=PRE_FIX_TYPES)
    assert undo_deletes_create(["task.claimed", "task.created"], type_filter=PRE_FIX_TYPES)
    assert undo_deletes_create(
        ["task.recurrence_advanced", "task.created"], type_filter=PRE_FIX_TYPES
    )


def test_head_protects_promoted_claimed_and_advanced_tasks():
    """FIXED-ALREADY at HEAD: newest event wins, including non-undoable types."""
    src = read("crates/ptask-core/src/tasks.rs")
    assert undo_sql_loads_all_events(src), "undo_last must inspect every later mutation type"
    events = ["task.promoted", "task.created"]
    assert not undo_deletes_create(events, type_filter=None)
    assert not undo_deletes_create(["task.claimed", "task.created"], type_filter=None)
    assert not undo_deletes_create(
        ["task.recurrence_advanced", "task.created"], type_filter=None
    )
    # Bare create is still reversible.
    assert undo_deletes_create(["task.created"], type_filter=None)
