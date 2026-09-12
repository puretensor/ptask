# Recurrence

Recurring tasks live in `pt_recurrence`. The parser is hand-written and
compiles natural-language input to an RRULE-style string + a mode flag
(`fixed` or `completion`).

## Modes

- `fixed` — `every` — next occurrence anchored to the previous
  scheduled time, not when the task was completed. Best for
  calendar-like cadences (`every monday at 9am`).
- `completion` — `every!` (bang suffix) — next occurrence anchored to
  *now* on completion. Best for "every five days from when I last
  did it" tasks (`every! 5 days`).

## Phrases the parser accepts

| Input | RRULE shape | Mode |
|---|---|---|
| `every day` | `FREQ=DAILY` | fixed |
| `every weekday` | `FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR` | fixed |
| `every monday` | `FREQ=WEEKLY;BYDAY=MO` | fixed |
| `every monday at 9am` | `FREQ=WEEKLY;BYDAY=MO` + time | fixed |
| `every mon, wed, fri` | `FREQ=WEEKLY;BYDAY=MO,WE,FR` | fixed |
| `every 1, 15, 27` | `FREQ=MONTHLY;BYMONTHDAY=1,15,27` | fixed |
| `every 2 months` | `FREQ=MONTHLY;INTERVAL=2` | fixed |
| `every! 5 days` | `FREQ=DAILY;INTERVAL=5` | completion |
| `every! 2 weeks` | `FREQ=WEEKLY;INTERVAL=2` | completion |

Use a quick-add recurrence phrase, such as `pt add "Standup every weekday at 9am"`.
The parser does not support `every other tuesday`, `every last friday`, or a
`starting ...` suffix.

## Advancement

On `pt done` for a recurring task, the engine advances the same row to its
next `deadline`, preserving its UUID, PT-N and history:

- `fixed`: advance from the **current deadline**, skipping missed occurrences
  until the next deadline is after the completion time.
- `completion`: compute the next RRULE occurrence from **now**.

The next occurrence is `status_v2='todo'` (legacy `status='pending'`) with
`snoozed_until` cleared. A previous claim ends with completion, so the next
occurrence can be claimed again. A `task.recurrence_advanced` event records
the new deadline; no new task is created.

## Editing

```
pt edit <PT-N> --deadline 2099-01-01   # move the next occurrence
```

This also updates `pt_recurrence.next_occurrence`. Clearing the deadline on
a recurring task is rejected. There is no `pt edit --recurrence` flag;
create recurrence through quick-add, and dismiss the task to stop working it.

## Storage

```sql
CREATE TABLE pt_recurrence (
    task_uuid       TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    rrule           TEXT NOT NULL,            -- RFC 5545
    mode            TEXT NOT NULL,            -- 'fixed' | 'completion'
    original_input  TEXT NOT NULL,            -- 'every monday at 9am'
    next_occurrence TEXT NOT NULL             -- ISO datetime UTC
);
```

`next_occurrence` is denormalised so `pt list 'today & recurring'` can
filter without re-evaluating the RRULE on every read.
