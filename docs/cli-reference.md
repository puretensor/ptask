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

### `pt edit <query> [--deadline ISO | --clear-deadline]` (alias `pt update`)

Edit task fields. Currently supported: set the deadline to an ISO
date/datetime, or clear it. Recurring tasks reject deadline clearing;
setting a deadline updates their next occurrence. Deadline feeds
`score_urgency`, so a rescore runs immediately after the change.

### `pt next [-n LIMIT]`

DAG-ready tasks: every `depends_on` predecessor is `done` (or missing).
Ordered by `priority_score DESC, priority DESC, created_at DESC`.

### `pt branch <query>`

Print a Linear-style branch name for the matched task, e.g.
`feature/PT-42-buy-bread-tomorrow-10am`. Pipe into `git checkout -b`.

## Saved views

```
pt view save <name> '<filter-dsl>'   # store
pt view list                          # list
pt view show <name>                   # run
pt view rm <name>                     # delete
```

## Long-running daemons

- `pt tui` — ratatui frontend with `j/k`, single-key edits, fuzzy search.
- `pt serve [--bind 0.0.0.0:9501]` — HTTP API: `/sync`, `/capture`,
  `/webhook/{gitea,github}`, `/metrics`. See [sync-api.md](sync-api.md).
- `pt bot` — Telegram long-poll handler.

## Pipeline orchestrators

| Verb | Cadence | Description |
|---|---|---|
| `pt distill [--days 60]` | `*-*-* 00,06,12,18:00:00` | Distillation pipeline. v0.6.5 Python shim by default. |
| `pt accountability run [--dry-run]` | `*:0/15` | Escalation state machine + dispatch. |
| `pt scoring run [--dry-run]` | `hourly` | Composite priority recompute. |
| `pt backfill` | one-shot | Mint PT-N for any task lacking one. |

## Remote (`pt remote`)

Talks to a canonical `pt serve` over Tailscale; no local DB.

| Verb | Use |
|---|---|
| `pt remote add "..." [--url ...]` | quick-add on the remote canonical |
| `pt remote list [-s STATUS -p P -n N]` | full-sync + client-side filter |
| `pt remote done <query>` | resolve + `task_done` |

`--url` defaults to `$PTASK_SYNC_URL` then `https://ptask.ts.puretensor.local`.

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
