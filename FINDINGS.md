# FINDINGS — pTask

Running register of review findings. One verb per item: **fixed** (version), **deleted**,
**held** (operator decision, with the reason), or **residue** (recorded, not changed, with
why). Companion ledger: `review-ledger.jsonl`; the Opus reader report this section was
triaged from is archived at `~/reports/cc/fable-pass-2026-09/reports/report-ptask.md`.

## 2026-09-02 — Fable 5.1 MAX pass (wave 10 of the ten-repo review)

Scope: `crates/ptask-server/src/*` (auth, lib router + MCP mount, every route, blocking,
dedup, webhooks), `ptask-core` (migrations, storage, tokens, config, event log, raw_items,
reap, scoring entry points, task mutations), `ptask-cli` (serve, mcp, export, backfill,
remote), `dashboard/server.py` + `session_auth.py`, `scripts/` (release, backup,
restore-verify, failure alert, the CI gates, ansible), the systemd units and both forges'
workflows — by an Opus reader; the roll-time delta and the backup/restore family by HAL.
Deployment truth first: the live `pt` 3.13.1 had been `cargo install`ed from a feature-branch
worktree (`pt-main-tmp`, since pruned) while `main` was 3.17.1; the dashboard already ran
3.17.1 from its production worktree; 19 worktrees → 6 (four dirty Cursor worktrees held);
gitleaks over 319 commits: clean; the nightly backup and the weekly restore drill run from the
live tree, whose 3.13.1+ script defaults are public-repo placeholders (`backup-host`,
`dr-host`) — the first run after the checkout moved to `main` would have failed and paged.

### Fixed (3.18.0)

| # | Finding (reader id) | Fix |
|---|---|---|
| T1 | A duplicate capture (unkeyed re-send, or the loser of two concurrent keyed sends) hit V014's unique index and returned 500 with raw SQLite text; `/email` the same for a repeated Message-ID (H1). | Both lanes use `insert_idempotent`; the repeat answers 200 with `duplicate: true`. Test fixture now carries the production unique index; pin added. |
| T2 | Token resolution wrote `last_used_at` on every request and any write failure — a litestream restore, a full disk, a long writer past the 30 s busy timeout — turned every authenticated call, pure reads and the MCP gate included, into a 401 (H2). | The touch is best-effort and logged. |
| T3 | `/sync` accepted an unbounded command array, each command taking the write lock in turn (M2). | 413 above 200 commands; pin added. |
| T4 | Git webhook close directives were gated on the literal `main`/`master` and silently skipped every other default branch (M3). | The payload's `repository.default_branch` decides, with the old pair as the fallback; tests updated. |
| T5 | The dashboard bound every interface while the API is deliberately pinned to the tailnet (M5); its Basic-auth compatibility path had no lockout (M6); task ids reached `pt` without a `--` separator (L1). | Loopback default with `PTASK_DASH_BIND` set to the tailnet address in the live env (mon1 probes it there); the Basic path shares the login throttle; `--` before every id. |
| T6 | The reaper units ran in production but existed only in the ignored `dist/` (M7). | `scripts/systemd/ptask-reaper.{service,timer}` from the live units; listed in the ansible playbook. |
| T7 | litestream's stated precondition (`wal_autocheckpoint=0`) was never applied (M8). | `PTASK_WAL_AUTOCHECKPOINT` env-gated pragma on every connection. |
| T8 | The restore drill would `ssh none` on hosts that opt out of the off-site leg (M11); the ansible restart handler could not fail (M13); the canonical release workflow's header claimed a glibc floor it does not enforce (M9); a per-request `reqwest::Client` on the voice proxy (L7). | Guarded; `failed_when: false` dropped; header corrected; one pooled client. |
| — | Live: the backup and restore-drill units get their fleet targets from a drop-in outside the public tree (`PTASK_BACKUP_REMOTE`, `PTASK_BACKUP_OFFSITE`); both units re-run green (nightly → mon1 + offsite; drill: litestream restore 1,823 tasks = live 1,823, nightly 0 h old, offsite 0 h old). | |

### Deleted

`.simplify/` — a contract gate whose listed files no longer exist, so its checksum step could only fail.

### Held (operator decision) / residue

- **M1** The MCP surface, the read routes and `/metrics` run blocking SQLite on the async executor (40+ handlers; the commit that claimed the conversion left three modules) — its own PR.
- **M4** The schema CI gate proves greenfield bootstrap only, never a migration of a live-shaped database.
- **M10** MCP captures never set `capture_key`, so `/capture/resolve` cannot close them.
- **M12** Two production units execute from the live working tree (the class fixed for the telegram-forwarder deploy); install from ansible into `~/.local/libexec/`.
- Four dirty Cursor worktrees under `/var/tmp/cursor-fleet/nv-ptask-*` with 5–13 unmerged commits each; the six one-commit `codex/*` branches (refs kept, worktrees removed).
- L4 (`/api/stats.version` means two things), L5 (`pt_webhook_log` has never had a row — the gauge cannot fire), L6 (fleet topology in a public repo's workflows — not a secret, not a rotation trigger), L8–L11.
- The `PTASK_API_TOKEN` in the user manager environment is readable by any process of the user; documented, not changed.

### Roll (3.13.1 → 3.18.0)

No schema delta between the live binary and `main` (14 refinery migrations applied on both sides; `user_version` unused). `LOCAL_LLM_URL` present in the live env (the 3.13.2 default moved to loopback). Rolled by `cargo install --path crates/ptask-cli --features native-ml --locked` from the merged `main`, previous binary kept as `~/.cargo/bin/pt.bak-pre-3.18.0`, `ptask-serve` restarted, dashboard worktree fast-forwarded and restarted, consumers verified (details in the pass report).
