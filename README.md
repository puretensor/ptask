# pTask

> **PureTensor's sovereign, single-binary task manager.** One command — `pt` — to capture, find, finish, and review work, from terminal, TUI, Telegram, HTTP API, MCP-native agents, or the web cockpit. Rust, SQLite, no subscriptions, no cloud dependency.

**Status:** production-active on the PureTensor fleet since the v1.0 activation (2026-05-14); the v2 agent-native program completed 2026-07-02. The current version lives in [`Cargo.toml`](Cargo.toml) (`workspace.package.version`) — this line intentionally names no number, after the v1.1.1/v1.2.0 README drift.

## What it does

- **Capture fast** — inline-token quick-add (`tomorrow 10am @home p1 ~30m`), natural-language dates, idempotent `capture` lane that fast-tracks fleet incidents (severity ≥ 3) into tasks.
- **Find fast** — Linear-style `PT-N` IDs, filter DSL (`pt list "(today | overdue) & p1"`), FTS5 full-text search, saved views.
- **Work in order** — DAG dependencies (`pt next` shows only unblocked tasks), composite priority scoring with explainability (`pt why PT-42`), recurrence (`every` vs `every!`), snooze.
- **Stay honest** — attributed event log (`pt log`: who did what, via which surface), `pt undo`, accountability escalation state machine with Telegram/SMTP/HAL notifications, staleness reaper for machine-generated tasks.
- **Feed the agents** — MCP server (11 tools over streamable-HTTP and stdio), atomic `task_claim` so parallel agents can't collide, `discovered_from` provenance links, deterministic `task_digest` session priming, scoped named API tokens.
- **Distill the noise** — native Rust distillation turns raw fleet signals into deduplicated tasks (Gemini structured-output classify/consolidate, semantic + temporal dedup, close-on-recovery). Chunked with per-chunk failure isolation, so one unprocessable capture is quarantined instead of wedging the queue behind it (`pt_distill_quarantined_captures`). `pt distill` is canonical; the legacy Python pipeline is archived for reference only.

## Quick start

```bash
pt add "Buy bread tomorrow 10am @home p1 ~30m"   # inline-token quick-add
pt list "(today | overdue) & p1"                  # filter DSL
pt next                                           # DAG-ready tasks
pt done PT-42                                     # complete
pt why PT-42                                      # explain a task's priority score
pt                                                # opens the TUI
pt serve                                          # axum HTTP server: sync API, capture, webhooks, metrics
pt bot                                            # Telegram bot (long-poll)
pt mcp                                            # MCP server over stdio
```

`pt --help` lists all ~40 subcommands; `pt gen-manpage` / `pt gen-completions` generate the manpage and shell completions. Full reference: [`docs/cli-reference.md`](docs/cli-reference.md).

## Surfaces

| Surface | Entry point | Notes |
|---|---|---|
| CLI | `pt <verb>` | `--json` for machine output, `--idempotency-key` for safe retries |
| TUI | `pt` / `pt tui` | ratatui |
| Sync API | `pt serve` | axum; canonical store on one host, clients use `pt remote` |
| Telegram | `pt bot` | Bot API long-poll |
| MCP (agents) | `pt mcp` (stdio) or `/mcp` mount on the server | 11 tools; bearer-gated HTTP for HAL, scoped REST tokens for other agents — [`docs/agent-surface.md`](docs/agent-surface.md) |
| Web | [`dashboard/`](dashboard/) | **PTASK Triage Cockpit** — read-only Python sidecar over the same DB; writes delegate to the `pt` binary |

## Architecture

Single Cargo workspace, single binary `pt`. SQLite via `rusqlite` (bundled), migrations via `refinery`, TUI via `ratatui`, HTTP via `axum`, dates via `jiff` + `interim`. Filter DSL and recurrence use hand-written parsers (no parser-combinator dependency).

| Crate | Role |
|---|---|
| `ptask-core` | Domain logic: storage, parsing, scoring, accountability |
| `ptask-cli` | The `pt` binary and all subcommands |
| `ptask-server` | Sync API, capture, webhooks, MCP HTTP mount, metrics |
| `ptask-tui` | Terminal UI |
| `ptask-bot` | Telegram bot |
| `ptask-distill` | Native Rust distillation orchestrator + ML stages (`native-ml` feature) |
| `ptask-notify` | Notification adapters (Telegram/SMTP/HAL) behind ptask-core's `Dispatch` trait |

Fleet topology (canonical store, Litestream WAL replication to CephFS, per-node roles, ansible rollout): [`docs/architecture.md`](docs/architecture.md). Backups, restore drills, and timers: [`docs/operations.md`](docs/operations.md).

## Documentation

- [`docs/cli-reference.md`](docs/cli-reference.md) — every subcommand
- [`docs/dsl.md`](docs/dsl.md) — the filter DSL
- [`docs/recurrence.md`](docs/recurrence.md) — recurrence semantics (`every` vs `every!`)
- [`docs/sync-api.md`](docs/sync-api.md) — HTTP API
- [`docs/agent-surface.md`](docs/agent-surface.md) — MCP tools, claim/lease mechanics, provenance
- [`docs/architecture.md`](docs/architecture.md) — self-hosted topology and canonical-store election
- [`docs/operations.md`](docs/operations.md) — backups, timers, runbooks
- [`docs/master-plan.md`](docs/master-plan.md) — the historical 12-phase build plan (complete) and design lineage

## Design lineage

Linear's data model (fixed status categories, `<TEAM>-<N>` IDs, cycles), Todoist's quick-add + filter DSL + recurrence, dstask's git-diffable export. pTask replaced an earlier Python task store (retired at v1.0). See `docs/master-plan.md` for the design lineage.

## License

Proprietary. Copyright (c) 2026 PureTensor, Inc. All rights reserved.
See [LICENSE](LICENSE). Source is published for review; use, copying, and
distribution require a written commercial license from PureTensor.
