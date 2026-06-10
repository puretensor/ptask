# pTask

> Sovereign single-binary Rust task manager for PureTensor. Replaces `puretensor-tasks` (Python). Single command `pt` for capture, find, finish, review — from terminal, TUI, Telegram, email, or HAL.

**Status:** production-active on the PureTensor fleet. The current version lives in [`Cargo.toml`](Cargo.toml) (`workspace.package.version`) — this line intentionally names no number, after the v1.1.1/v1.2.0 README drift. See [`docs/master-plan.md`](docs/master-plan.md) for the historical 12-phase build plan and current follow-ups.

## Quick Start (when shipped)

```bash
pt add "Buy bread tomorrow 10am @home p1 ~30m"   # inline-token quick-add
pt list "(today | overdue) & p1"                  # filter DSL
pt next                                           # DAG-ready tasks
pt done PT-42                                     # complete
pt                                                # opens TUI
pt serve                                          # axum sync API
pt bot                                            # Telegram bot
```

## Architecture

Single Cargo workspace, single binary `pt`. Domain logic in `ptask-core`. Surfaces in `ptask-cli`, `ptask-server`, `ptask-tui`, `ptask-bot`, `ptask-distill`. SQLite via `rusqlite` (bundled). Migrations via `refinery`. TUI via `ratatui`. HTTP via `axum`. Dates via `jiff` + `interim`. Filter DSL + recurrence via `winnow`.

## Design References

- [`docs/master-plan.md`](docs/master-plan.md) — the 12-phase build plan
- Linear's data model (cycles, projects, fixed status categories, `<TEAM>-<N>` IDs)
- Todoist's inline-token quick-add + filter DSL + recurrence (`every` vs `every!`)
- dstask's file-per-task git audit layer (deferred to v1.0.0)
- Existing Python implementation at `~/puretensor-tasks/` — preserved for reference until v0.9.0

## Workflow

This repo is built phase-by-phase by Claude on `main`. After each phase, Codex performs a separate review pass and opens PRs with improvements. The operator merges. See `docs/master-plan.md` § "Workflow Contract" for the contract.

## License

MIT — see [LICENSE](LICENSE).
