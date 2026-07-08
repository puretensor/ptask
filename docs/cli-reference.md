# pt(1) — Command Reference

Pre-rendered manpage at [docs/gen/pt.1](gen/pt.1); regenerate with
`pt gen-manpage > docs/gen/pt.1`.

## Globals

| Flag | Env | Default |
|---|---|---|
| `--db <PATH>` | `PTASK_DB` | `~/puretensor-tasks/tasks.db` |
| `--help`, `-h` | — | — |
| `--version`, `-V` | — | — |

## Tasks

### `pt add <text> [...]`

Create a task. The free-text title runs through the quick-add parser
(see [dsl.md](dsl.md)) unless `--raw` is set.

| Flag | Use |
|---|---|
| `-p`, `--priority` | `low | normal | high | urgent | critical | 1..=5` |
| `-d`, `--description` | description body |
| `--deadline <ISO>` | `2026-05-21` or `2026-05-21T10:00:00+01:00` |
| `--reason` | persisted as `ai_reasoning` |
| `--raw` | skip quick-add parsing |

### `pt list [filter] [...]` (alias `pt ls`)

| Flag | Use |
|---|---|
| `-s`, `--status` | `pending` (default), `done`, `delayed`, `dismissed`, `blocked`, `all` |
| `-p`, `--priority` | `1..=5` or label |
| `-n`, `--limit` | rows; default 20 |
| `-v`, `--verbose` | show description + UUID |
| `[filter]` positional | DSL — see [dsl.md](dsl.md) |

### `pt done <query>`

Mark done by `PT-N`, bare integer `42`, or title substring.

### `pt priority <query> <level>` (alias `pt pri`)

Promote/demote a task's priority: `critical | urgent | high | normal | low`
or `1..=5`. Rescores immediately so `pt next` ordering reflects the change.

### `pt edit <query> [--deadline ISO | --clear-deadline] [--title T] [--desc D]` (alias `pt update`)

Edit task fields: set/clear the deadline and/or replace the title/description
(any combination; at least one required). Recurring tasks reject deadline
clearing; setting a deadline updates their next occurrence. A deadline change
feeds `score_urgency` and triggers an immediate rescore; a text-only edit does
not rescore.

### `pt reopen <query>`

Flip a completed or dismissed task back to `pending` (resolve by PT-N for a
done task — substring resolution only matches active tasks). Logs a
`status_change` interaction the neglect score reads as a reopen, and rescores
immediately so the task re-enters `pt next` ordering.

### `pt show <query>`

Print one task's full row plus side-table detail: labels, project, duration,
dependencies (`deps on` / `blocks`), and recurrence.

### `pt dismiss <query>`

Soft-close a task (`status → dismissed`). Reversible with `pt reopen`. Distinct
from `pt rm`: the row and its history survive.

### `pt rm <query> [-y | --yes]`

Permanently delete a task (hard `DELETE` + a `task.deleted` tombstone for delta
sync). Prompts for confirmation unless `--yes`. The `interactions` history is
lost with the row — prefer `pt dismiss` unless you truly want it gone.

### `pt next [-n LIMIT]`

DAG-ready tasks: every `depends_on` predecessor is `done` (or missing).
Ordered by `priority_score DESC, priority DESC, created_at DESC`.

### `pt branch <query>`

Print a Linear-style branch name for the matched task, e.g.
`feature/PT-42-buy-bread-tomorrow-10am`. Pipe into `git checkout -b`.

## Scoring & why (v2.2.0)

```
pt scoring run            # composite v2 (growth urgency, real neglect, link deps, effort)
pt scoring run --v1       # legacy v1 formula
pt scoring run --diff     # rank diff v1 vs v2 (top movers) without writing
pt scoring run --dry-run  # compute, print, don't write
pt why PT-42              # component breakdown: urgency/neglect/dependency/effort/llm + rank
```

v2 composite = 0.35·urgency + 0.20·neglect + 0.15·dependency + 0.30·(priority/5·effort_factor) + clamp(score_llm, ±0.15).
No-deadline urgency GROWS with age (aged p5 can never rank below fresh p3). `score_llm` is written by the Phase-8 triage pass; zero until then.

## Saved views

```
pt view save <name> '<filter-dsl>'   # store
pt view list                          # list
pt view show <name>                   # run
pt view rm <name>                     # delete
```

## Agent surface (v2.4.0)

```
pt mcp                    # MCP server over stdio (tools: task_next/list/add/…)
pt digest [--days 7]      # session-priming JSON: recent done/dismissed + ready queue
pt export [--git] [--out DIR]   # JSONL projection of the spine (nightly timer)
pt delegate PT-42         # prints the operator-gated claude -p command (never spawns)
```

HTTP MCP mounts at /mcp in `pt serve` (hal token only) — docs/agent-surface.md.

## Long-running daemons

- `pt tui` — ratatui frontend with `j/k`, single-key edits, fuzzy search.
- `pt serve [--bind 127.0.0.1:9501]` — HTTP API: `/sync`, `/capture`,
  `/webhook/{gitea,github}`, `/metrics`. See [sync-api.md](sync-api.md).
- `pt bot` — Telegram long-poll handler.

## Pipeline orchestrators

| Verb | Cadence | Description |
|---|---|---|
| `pt distill [--batch 200]` | hourly (`*:15`) | Native fail-closed distillation: consumes new `raw_items` only, Gemini structured-output classify+consolidate with `thinkingBudget=0`, transient retry, and token/semantic/temporal dedup. Exit 3 = missing GOOGLE_API_KEY before consumption. |
| `pt accountability run [--dry-run]` | `*:0/15` | Escalation state machine + dispatch. |
| `pt scoring run [--dry-run]` | `hourly` | Composite priority recompute. |
| `pt backfill` | one-shot | Mint PT-N for any task lacking one. |

## Workflow (v2.0.0)

| Verb | Use |
|---|---|
| `pt start <query>` | mark in progress (status_v2 `in_progress`) |
| `pt snooze <query> <until…>` | park until a date (natural language ok); auto-wakes to todo via the hourly scoring run |
| `pt depend <query> --on <target> [--clear]` | dependency edges in `task_links`; `pt next` hides tasks with unmet deps; no `--on` shows current edges |
| `pt review [--stale-days N]` | interactive sweep of stale tasks (TTY: k/d/x/s/q; non-TTY prints the list) |
| `pt search <query…> [-n N]` | FTS5 full-text over titles + descriptions |
| `pt bulk '<filter>' --set-priority P \| --done \| --dismiss [--dry-run]` | one action across every DSL match |
| `pt done <q1> <q2> …` | done now accepts multiple tasks |

Globals (v2.0.0): `--json` on task-facing verbs emits machine-readable
output; `--idempotency-key <k>` keys the mutation's event so retries are
safe. Quick-add gains `due:<date>` (scheduled) alongside hard deadlines.
Statuses are the 8-state v2 model: triage/backlog/todo/in_progress/
snoozed/done/dismissed/blocked (legacy column maintained for
not-yet-retired consumers).

## Journal & tokens (v1.17.0)

| Verb | Use |
|---|---|
| `pt log <query> [-n N]` | attributed event history for a task: when, who (actor), via which surface, what |
| `pt undo` | reverse the most recent undoable mutation (done/dismiss → reopen, create → delete); the reversal is itself an attributed event |
| `pt token create <client_id> [--scope read\|capture\|write\|admin]` | mint a named scoped API token (plain value shown ONCE; only the sha256 is stored) |
| `pt token list` | client, scope, active/revoked, created/last-used |
| `pt token revoke <client_id>` | revoke all active tokens for a client |

Server auth resolves, in order: legacy env `PTASK_API_TOKEN` → env metrics
token → `pt_api_tokens` lookup. Named-token requests are journaled under
their client_id; local mutations under `$PTASK_ACTOR` (default `shell`).

## Remote (`pt remote`)

Talks to a canonical `pt serve` over Tailscale; no local DB.

| Verb | Use |
|---|---|
| `pt remote add "..." [--url ...]` | quick-add on the remote canonical |
| `pt remote list [-s STATUS -p P -n N]` | full-sync + client-side filter |
| `pt remote done <query>` | server-side `/resolve` + `task_done` |
| `pt remote priority <query> <level>` (alias `pri`) | server-side `/resolve` + `task_priority` (+ server rescore) |
| `pt remote edit <query> [--deadline ISO \| --clear-deadline] [--title T] [--desc D]` (alias `update`) | server-side `/resolve` + `task_edit` (deadline) and/or `task_retext` (title/desc) |
| `pt remote reopen <query>` | server-side `/resolve` (incl. done) + `task_reopen` |
| `pt remote show <query>` | base row + side-table detail via `GET /detail/{uuid}` (read-only) |
| `pt remote next [-n N]` | DAG-ready tasks via `GET /next` (server resolves `depends_on`) |
| `pt remote dismiss <query>` | server-side `/resolve` + `task_dismiss` (soft close; reversible via reopen) |
| `pt remote start <query>` | server-side `task_start` |
| `pt remote snooze <query> <until…>` | server-side `task_snooze` (date parsed locally) |
| `pt remote depend <query> --on <t> [--clear]` | server-side `task_depend` |
| `pt remote rm <query>` | server-side `task_delete` (tombstoned) |
| `pt remote list --filter '<DSL>'` | SERVER-side filtered list via `GET /list` |
| `pt remote version` | compare client vs server `GET /version`; exits non-zero on skew |

`--url` defaults to `$PTASK_SYNC_URL` then `http://100.121.42.54:9501`.

Every remote error also runs the version handshake: a 401/404 from a
mismatched deploy appends `version skew: client vX vs server vY` to the
error instead of masquerading as an auth/routing failure.

## Codegen

```
pt gen-manpage > docs/gen/pt.1
pt gen-completions bash > docs/gen/pt.bash
pt gen-completions zsh  > docs/gen/_pt
pt gen-completions fish > docs/gen/pt.fish
```

## Exit codes

| Code | Meaning |
|---|---|
| `0` | success |
| `1` | runtime error (DB, network, parse) |
| `2` | argparse error (clap) |
| `64` | usage error from `scripts/release.sh` and similar helpers |
