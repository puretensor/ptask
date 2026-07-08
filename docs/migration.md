# Migration — Python `puretensor-tasks` → Rust pTask

The migration was designed as a **per-domain handoff** rather than a
cold cutover. The same `~/puretensor-tasks/tasks.db` file kept its
existing tables; Rust added a small set of side tables (`pt_extensions`,
`pt_views`, `pt_recurrence`, `pt_event_log`, `pt_webhook_log`) and took
ownership of one column-set per phase.

## Phase ladder

| Phase | What flipped | Tag |
|---|---|---|
| 1 | Workspace, migrations, PT-N minting | v0.1.1 |
| 2 | DSL, dates, recurrence | v0.2.3 |
| 3 | ratatui TUI | v0.3.1 |
| 4 | HTTP `/sync` + Prometheus | v0.4.1 |
| 5 | Telegram bot | v0.5.2 |
| 6 | Email + git magic-words | v0.6.2 |
| 6.5 | Distill shim — Rust owns the timer | v0.6.6 |
| 7 | Accountability — Rust owns escalation + dispatch | v0.7.1 |
| 8 | Scoring — Rust owns composite priority recompute | v0.8.1 |
| 9 | Native ML modules in tree (architectural close) | v0.9.0 |
| 10 | Fleet deploy kit (ansible, litestream, remote client) | v0.10.0 |
| 1.0 | Polish + Python archive | v1.0.0 |

## Per-domain write authority

| Domain | v0.1–v0.6 | v0.7+ | v0.8+ | v0.9+ |
|---|---|---|---|---|
| `tasks` CRUD | Python | Python | Python | **Rust** |
| `escalation_level`, `next_reminder`, `notifications` | Python | **Rust** | Rust | Rust |
| `priority_score`, `score_*` | Python | Python | **Rust** | Rust |
| `raw_items → canonical_tasks → tasks` | Python | Python | Python | **Rust** (gated on HAL) |
| `pt_extensions`, `pt_views`, `pt_recurrence`, `pt_event_log`, `pt_webhook_log` | Rust | Rust | Rust | Rust |

`v0.9` cutover was architectural. As of v3.0.0, `pt distill` is the native
Rust path and no longer shells out to Python. `PTASK_HAL_CLASSIFY_URL`,
`PTASK_HAL_CONSOLIDATE_URL`, and `PTASK_DISTILL_PY_ROOT` are not part of
the active distill runtime.

## Safety nets

- Nightly backup to Ceph (mon1, 30-day retention) via
  `ptask-backup.timer` — runs from v0.1.0 onward.
- One-time pre-v0.1 backup at `~/puretensor-tasks/tasks.db.pre-ptask-backup`.
- Per-phase rollback: revert the systemd-unit swap (Python timer comes
  back, Rust one goes dormant). DB state is unaffected because each
  phase touched only the columns it owned.

## Final archive (v1.0.0)

```
mv ~/puretensor-tasks ~/puretensor-tasks-legacy
chmod -R a-w ~/puretensor-tasks-legacy
cat > ~/puretensor-tasks-legacy/LEGACY.md <<EOF
# Archived

Active code lives at https://github.com/puretensor/ptask.
This tree is read-only as of pTask v1.0.0 (2026-MM-DD).
Re-enable a Python timer at your own risk — they conflict
with the live Rust ones on \`ptask-*.timer\`.
EOF
```

The `/ptask` Claude Code skill drops its Python fallback at the same
point — only the Rust `pt` binary remains in the call path.

## Re-enabling Python (emergency only)

Each Rust phase ships a documented rollback. The pattern is the same:

```
systemctl --user disable --now ptask-<DOMAIN>.timer
sudo systemctl enable --now puretensor-tasks-<DOMAIN>.timer
```

Specifically:

- accountability → `puretensor-tasks-accountability.timer`
- scoring → `puretensor-tasks-scoring.timer`
- distill → `puretensor-tasks-distill.timer`

The legacy units stay installed (but disabled) on mon1 until v1.0.0
archive. Post-archive, the rollback path requires un-archiving the
Python tree first.
