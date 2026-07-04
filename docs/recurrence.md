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
| `every other tuesday` | `FREQ=WEEKLY;INTERVAL=2;BYDAY=TU` | fixed |
| `every 1, 15, 27` | `FREQ=MONTHLY;BYMONTHDAY=1,15,27` | fixed |
| `every last friday` | `FREQ=MONTHLY;BYDAY=-1FR` | fixed |
| `every! 5 days` | `FREQ=DAILY;INTERVAL=5` | completion |
| `every! 2 weeks` | `FREQ=WEEKLY;INTERVAL=2` | completion |

Combine with the date parser for the start anchor:
`every weekday starting tomorrow 9am`.

## Advancement

On `pt done` for a recurring task, the engine clones the row with a
new `deadline` and a fresh PT-N:

- `fixed`: compute the next RRULE occurrence from the **original due**.
- `completion`: compute the next RRULE occurrence from **now**.

The completed instance keeps `status='done'`; the clone is `status='pending'`.

## Editing

```
pt edit <PT-N> --recurrence 'every weekday'   # set / change
pt edit <PT-N> --recurrence none              # stop
```

(`pt edit` lands in the v1.0.x polish phase; until then mutate
`pt_recurrence` via `pt view`-driven SQL or wait for the verb.)

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
