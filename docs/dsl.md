# DSL Reference

pTask has two parsers: **quick-add** (composing a new task in one shot)
and the **filter DSL** (selecting rows from `tasks` for `pt list`).

## Quick-add (`pt add`, `pt remote add`)

Free text with inline tokens. The non-token remainder is the title;
trailing `//description` becomes the description.

### Tokens

| Token | Effect |
|---|---|
| `p1`..`p5` | priority, native scale: p1=low, p2=normal, p3=high, p4=urgent, p5=critical. Identical to `--priority` and `pt priority` — no Todoist inversion. |
| `@label` | append to `pt_extensions.labels` JSON. Multiple allowed. |
| `#project` | `pt_extensions.project` (last wins). |
| `~30m`, `~2h`, `~1d` | `pt_extensions.duration_min`. |
| `!HH:MM` | reminder hour. Stored alongside `deadline` if a date phrase is present. |
| `//description` | everything from `//` to end-of-text → description. |
| `every monday`, `every weekday`, `every! 5 days`, etc. | recurrence — see [recurrence.md](recurrence.md). |
| date phrases | parsed by `interim` 0.2 (chrono-english-like). |

### Date phrases

The date parser handles:

- absolute: `2026-05-20`, `May 20`, `5/20/2026`
- relative: `tomorrow`, `next monday`, `in 3 days`, `next week`
- combined with time: `tomorrow 10am`, `friday 18:00`, `monday at 9am`
- this/next/last: `this friday`, `next friday`, `last friday`

Operator timezone: `Europe/London` (DST-correct via jiff).

### Examples

```
pt add 'gym monday 8am @health every! monday p2 ~45m'
pt add 'buy bread tomorrow 10am @home p1 ~30m //sourdough from baker'
pt add 'investigate ceph mon quorum @ops p4 #fleet'
pt add 'review PR #42 //sync via gh pr view 42'
```

## Filter DSL (`pt list`, saved views)

Boolean expressions over field tokens.

### Field tokens

| Token | Predicate |
|---|---|
| `today` | `deadline = today` or due today |
| `overdue` | `deadline < today AND status != 'done'` |
| `no date` | `deadline IS NULL` |
| `recurring` | row has a `pt_recurrence` entry |
| `p1`..`p5` | exact priority match |
| `@label` | label in `pt_extensions.labels` |
| `#project` | project match |
| `due:`*phrase* | resolves a date phrase; exact-day match |
| `due before:`*phrase* | `deadline < parsed_date` |
| `created:`*phrase* | `created_at` on that day |
| `search:`*str* | `title LIKE %str%` (case-insensitive) |
| `status:STATE` | exact status; same lexicon as `-s` |
| `kind:`*scout\|ship* | investigation vs implementation (see `pt promote`) |

### Operators

| Op | Form |
|---|---|
| AND | `a & b` or whitespace `a b` |
| OR | `a | b` |
| NOT | `!a` |
| group | `(a | b) & c` |

### Examples

```
pt list 'today & p1'
pt list '(today | overdue) & #fleet'
pt list '@waiting & no date'
pt list 'due before: next friday & !recurring'
pt list 'search: ceph & @ops'
pt list 'kind: scout & p4'
```

## Quoting

Use single quotes around the whole DSL string in shells — `&`, `|`,
`!` are interpreted by the shell otherwise.
