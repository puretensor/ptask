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
| `!HH:MM` | reminder time of day (a valid `HH:MM` only). Echoed by `pt add`; not persisted. |
| `//description` | everything from `//` to end-of-text → description. |
| `every monday`, `every weekday`, `every! 5 days`, etc. | recurrence — see [recurrence.md](recurrence.md). |
| `YYYY-MM-DD` | deadline, when it is a standalone date in the future. |
| `due:`*date* | scheduled date (`due_at`): when you plan to do it, distinct from the deadline. One word: `due:2026-10-02`, `due:tomorrow`. |

### Dates

Body-text deadline inference is deliberately narrow: only a standalone
future ISO date (`2026-10-02`) sets the deadline. Other date prose
(`tomorrow`, `next friday`, `4/5`, a past date) stays title text instead of
silently setting a wrong deadline; pass `--deadline` for anything else.
Recurrence phrases (`every monday at 9am`) set the first occurrence.

Operator timezone: `Europe/London` (DST-correct via jiff).

### Examples

```
pt add 'gym @health every! monday at 8am p2 ~45m'
pt add 'buy bread 2026-10-02 @home p1 ~30m //sourdough from baker'
pt add 'investigate ceph mon quorum @ops p4 #fleet'
pt add 'review PR #42 //sync via gh pr view 42'
```

## Filter DSL (`pt list`, saved views, `GET /list`, MCP `task_list`, bot `/list`)

Boolean expressions over field tokens. Status is not a token: use
`pt list -s/--status` (or `status=` on `GET /list`).

### Field tokens

A "day" is a calendar day in the operator timezone (`Europe/London`). A
date-only deadline (`2026-10-02`) compares as a date; a datetime deadline
compares as an instant against that day's local start and end, whatever
offset it was stored with (`2026-10-20T23:30:00Z` is 21 Oct in BST).

| Token | Predicate |
|---|---|
| `today` | deadline on today, or scheduled (`due_at`, quick-add `due:`) for today |
| `tomorrow`, `yesterday` | deadline on that day |
| `overdue` | not done or dismissed, and a date-only deadline before today, a datetime deadline before now, or a deadline that can't be read as a date |
| `no date` (alias `no deadline`) | no deadline |
| `recurring` | row has a `pt_recurrence` entry |
| `p1`..`p5` | exact priority match |
| `@label` | label in `pt_extensions.labels`; the name runs to the next space or operator (`@domain:mgmt`, `@v1.2`) |
| `#project` | exact project match; same name rule (`#infra/core`) |
| `due:`*phrase* | deadline on the day the phrase resolves to |
| `due before:`*phrase* | deadline before that day |
| `due after:`*phrase* | deadline after that day |
| `search:`*str* | `str` in the title or description (case-insensitive); an empty `str` is an error |
| `kind:`*scout\|ship* | investigation vs implementation (see `pt promote`) |

### Operators

| Op | Form |
|---|---|
| AND | `a & b` (binds tighter than OR; juxtaposition `a b` is an error) |
| OR | `a | b` |
| NOT | `!a` — true whenever `a` is not (a task with no project matches `!#fleet`) |
| group | `(a | b) & c` |

Limits: at most 2048 bytes, 32 levels of `(` / `!` nesting and 256 terms.

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
