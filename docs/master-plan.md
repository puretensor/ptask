# pTask — Master Plan

> Sovereign, single-binary, Rust-native task manager for PureTensor. Replaces the existing Python `puretensor-tasks` system. Built phase-by-phase by Claude on `main`, reviewed by Codex on a separate pass, merged by operator.

---

## Context

The existing `~/puretensor-tasks/` Python system is in production: 204 active tasks, 11,209 raw items, 6-stage distillation pipeline (SBERT + BERTopic + Gemini), composite priority scoring (NetworkX), 6-level accountability state machine, FastAPI dashboard on `:9500`, 4 systemd timers. It works, but:

- **Browser-only surface.** Operator wants terminal/Telegram/bot-first, not a web dashboard.
- **No natural-language input.** Deadlines must be ISO. No recurrence, no filter DSL, no inline-token quick-add.
- **No shareable IDs.** UUIDs everywhere; nothing like `PT-123` for cross-referencing.
- **Slow surface.** Python + FastAPI + HTMX is fine, but for an operator who lives in CLI, the round-trip is friction.
- **Flat status model.** Just `pending|done|delayed|dismissed|blocked` — no Linear-style fixed categories with free labels.
- **No git integration.** "Fixes PT-123" in a commit should auto-transition; doesn't today.

Goal: build a Rust single-binary `pt` that absorbs every Python behavior, then extends it with Linear-grade IDs/status/views, Todoist-grade quick-add/DSL/recurrence, dstask-grade git audit, and HAL-grade triage. Pre-paid, pre-owned, sovereign — no subscriptions.

---

## North Star

> **`pt` is the single command an operator uses to capture, find, finish, and review work — from terminal, Telegram, email, or HAL. Every other surface is plumbing.**

Concrete one-liner success criteria at v1.0.0:
- `pt add "Buy bread tomorrow 10am @home p1 ~30m"` parses every token, mints `PT-N`, returns in <50ms.
- `pt next` returns the DAG-ready task list (no unsatisfied predecessors) ordered by critical-path pressure.
- `pt review` opens a Friday Telegram conversation with HAL that triages the inbox, flags stale items, and produces a weekly summary.
- Python system is retired. Single static binary deploys to any fleet node via `cargo dist`.

---

## Architecture (high-level)

```
┌──────────────────────────────────────────────────────────────────┐
│  Surfaces                                                         │
│  ─────────                                                        │
│  CLI (pt add/list/done/next/edit/show/rm/review)                 │
│  TUI (pt with no args → ratatui)                                  │
│  Telegram bot (teloxide, inline-token quick-add)                  │
│  Email forward (axum /capture, mail-parser)                       │
│  HAL via HTTP (sync API, HMAC webhooks)                           │
│  Git webhooks (Fixes PT-N from Gitea + GitHub)                    │
└────────────────┬─────────────────────────────────────────────────┘
                 │
┌────────────────▼─────────────────────────────────────────────────┐
│  ptask-core (lib): domain logic                                   │
│  ────────────────                                                 │
│  Quick-add parser (winnow)        Filter DSL (winnow)             │
│  Date parser (interim + jiff)     Recurrence (rrule + every/!)    │
│  DAG / next-query (petgraph)      Scoring (urgency/dep/neglect)   │
│  Accountability state machine     Distill orchestrator            │
└────────────────┬─────────────────────────────────────────────────┘
                 │
┌────────────────▼─────────────────────────────────────────────────┐
│  Storage                                                          │
│  ───────                                                          │
│  SQLite (rusqlite, bundled, WAL)                                  │
│  Existing tables: tasks / interactions / notifications /          │
│                   raw_items / canonical_tasks / ingested_files /  │
│                   daily_budget                                    │
│  New side tables: pt_extensions / pt_views / pt_event_log /       │
│                   pt_webhook_log / pt_recurrence                  │
└──────────────────────────────────────────────────────────────────┘
```

Cargo workspace at `~/code/ptask/`:

```
ptask/
├── Cargo.toml                    # workspace, edition 2024, MIT
├── crates/
│   ├── ptask-core/               # domain logic + storage
│   ├── ptask-cli/                # bin: pt
│   ├── ptask-server/             # axum HTTP + sync API
│   ├── ptask-tui/                # ratatui frontend (lib used by pt)
│   ├── ptask-bot/                # teloxide Telegram bot
│   └── ptask-distill/            # 6-stage pipeline (shim, then native)
├── migrations/                   # refinery V###__*.sql
├── docs/
│   └── master-plan.md            # mirror of this file
├── scripts/
├── .github/workflows/ci.yml      # cargo test/clippy/deny + golden-DB diff
└── README.md
```

Single binary `pt` at v1.0.0 (subcommands: `pt serve`, `pt bot`, `pt tui`, plus all task verbs).

---

## Workflow Contract

Per the operator's instruction:

```
1. Claude designs the phase against this master plan.
2. Claude implements directly on main branch:
   - One or more commits, each with version bump (SemVer)
   - cargo test green locally before each commit
   - git push to GitHub + Gitea after each commit
3. Claude signals "ready for review" when the phase is feature-complete.
4. Operator triggers Codex separately:
   - Codex reviews the latest commits since last review point
   - Codex creates one or more PRs targeting main with improvements
5. Claude reviews Codex's PRs (responds to feedback, may push fixup commits to the PR branch).
6. Operator merges Codex's PRs when satisfied.
7. Loop: next phase begins from updated main.
```

**Branch model:** Claude works on `main`. Codex's improvement PRs land on `codex/<phase-tag>` branches. No long-lived feature branches.

**Tag policy:** Each phase end tag `v0.N.0` on `main` after Codex's PRs are merged. Patch versions `v0.N.M` for fixup commits inside a phase.

**Push targets:** `origin` (GitHub `puretensor/ptask`) and `gitea` (Gitea `puretensor/ptask` on `100.92.245.5:2222`). Mirror-by-push, not Gitea's mirror feature, so commits land instantly on both.

---

## Versioning Rules

- **v0.N.0** = phase boundary. The list below.
- **v0.N.M** = feature commit inside a phase. Bump on every commit that ships code.
- **v0.N.M-rcK** = pre-merge fixup commits responding to Codex review.
- **v1.0.0** = fleet rollout, Python retired, docs complete.

Pre-1.0 means breaking changes can land in MINOR bumps (per CLAUDE.md). Every commit includes the version bump in the same commit (per CLAUDE.md). Bump in: `Cargo.toml` workspace version + crate `Cargo.toml` files + `pt --version` string.

---

## Migration Strategy (locked)

**Side-table approach on the existing `~/puretensor-tasks/tasks.db`.** The Rust system reads the existing tables, writes its own metadata to new side tables. Per-domain cutover one phase at a time.

New side tables added by Rust migrations:

```sql
CREATE TABLE pt_extensions (
    task_uuid       TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    pt_id           TEXT UNIQUE NOT NULL,          -- 'PT-1', 'PT-2', ...
    status_category TEXT NOT NULL DEFAULT 'todo',  -- triage|backlog|todo|in_progress|done|cancelled
    status_label    TEXT,                          -- free-form per-team status name
    energy          TEXT,                          -- deep|admin|phone|null
    duration_min    INTEGER,                       -- estimated minutes
    planned_at      TEXT,                          -- ISO datetime
    actual_min      INTEGER,                       -- actual time spent
    labels          TEXT DEFAULT '[]',             -- JSON array of strings
    created_by_pt   INTEGER DEFAULT 0              -- 1 if Rust created this row
);

CREATE TABLE pt_views (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    name            TEXT NOT NULL UNIQUE,
    filter_dsl      TEXT NOT NULL,                 -- raw DSL string
    grouping        TEXT,                          -- status|priority|project|none
    sort_by         TEXT,                          -- created_at|priority_score|deadline
    created_at      TEXT NOT NULL
);

CREATE TABLE pt_recurrence (
    task_uuid       TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    rrule           TEXT NOT NULL,                 -- RFC 5545 RRULE
    mode            TEXT NOT NULL,                 -- 'fixed' (every) | 'completion' (every!)
    original_input  TEXT NOT NULL,                 -- e.g. 'every monday at 9am'
    next_occurrence TEXT NOT NULL                  -- ISO datetime
);

CREATE TABLE pt_event_log (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid            TEXT UNIQUE NOT NULL,          -- idempotency key
    task_uuid       TEXT,
    event_type      TEXT NOT NULL,
    payload         TEXT NOT NULL,                 -- JSON
    ts              TEXT NOT NULL
);

CREATE TABLE pt_webhook_log (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    direction       TEXT NOT NULL,                 -- in|out
    source          TEXT NOT NULL,                 -- gitea|github|hal|telegram|email
    payload         TEXT NOT NULL,
    signature_ok    INTEGER NOT NULL,
    ts              TEXT NOT NULL
);
```

PT-N is minted in `pt_extensions` for every existing task on v0.1.0 first run. Counter persisted in `sqlite_sequence` or a dedicated `pt_counters` table.

**Per-domain write authority transfer:**

| Domain | Owner v0.1–v0.6 | Owner v0.7+ | Owner v0.8+ | Owner v0.9+ |
|---|---|---|---|---|
| tasks (CRUD) | Python | Python | Python | **Rust** |
| escalation_level, next_reminder, notifications | Python | **Rust** | Rust | Rust |
| priority_score, score_* | Python | Python | **Rust** | Rust |
| raw_items → canonical_tasks → tasks | Python | Python | Python | **Rust** |
| pt_extensions, pt_views, pt_recurrence, pt_event_log, pt_webhook_log | Rust | Rust | Rust | Rust |

**Safety**: Before v0.1.0 first run, `cp ~/puretensor-tasks/tasks.db ~/puretensor-tasks/tasks.db.pre-ptask-backup`. Backup automated nightly to Ceph from v0.1.0 onward.

---

## Phases

Each phase below is a separable deliverable. End state: shippable binary, Python system still functioning, tests green, version tagged.

### v0.1.0 — Foundation ✅ shipped

**Goal:** Rust workspace exists, reads the existing DB, mints PT-N for every task, ships a CLI with parity for the three current verbs (`add`, `list`, `done`). Python remains authoritative on all writes.

**Sub-sections:**
- ✅ **0.1.1 — Workspace scaffold** (v0.0.1). `Cargo.toml` workspace, 6 crates (ptask-core, ptask-cli, ptask-server, ptask-tui, ptask-bot, ptask-distill), edition 2024, MIT. Mirrors `ptve` layout conventions (shared `[workspace.package]` block).
- ✅ **0.1.2 — Database connection layer** (v0.0.2). `ptask-core::storage` with `rusqlite` + `r2d2` + `r2d2_sqlite`. WAL mode, `busy_timeout=30s`, `foreign_keys=ON`. `PTASK_DB` env defaults to `~/puretensor-tasks/tasks.db`.
- ✅ **0.1.3 — Refinery migrations** (v0.0.2). `V001__pt_counters.sql`, `V002__pt_extensions.sql`, `V003__pt_views.sql`, `V004__pt_recurrence.sql`, `V005__pt_event_log.sql`, `V006__pt_webhook_log.sql`. Embedded in binary via `refinery::embed_migrations!("migrations")`.
- ✅ **0.1.4 — PT-N minting** (v0.0.2). One-shot backfill iterates `tasks` in `created_at` order, mints sequential `PT-1..PT-N` into `pt_extensions`. Idempotent. Live backfill landed: PT-1..PT-204.
- ✅ **0.1.5 — CLI `pt add`** (v0.0.4). Argument parity with Python `cli.py add` (`-p`, `-d`, `--deadline`, `--reason`). Direct SQL into `tasks` preserving byte-for-byte Python defaults; mints PT-N + logs `interactions` row in the same transaction.
- ✅ **0.1.6 — CLI `pt list`** (v0.0.4). Parity with Python (`-s`, `-p`, `-n`, `-v`). Display includes PT-N from `pt_extensions`.
- ✅ **0.1.7 — CLI `pt done <PT-N | substring>`** (v0.0.4). Accepts `PT-N`, bare integer, or title substring. Multi-match prints choices and exits non-zero. Logs `status_change` interaction.
- ✅ **0.1.8 — Skill update.** `~/.claude/skills/ptask/SKILL.md` rewritten to call `pt` with `python3 ~/puretensor-tasks/cli.py` as fallback.
- ✅ **0.1.9 — CI** (v0.0.5). `.github/workflows/ci.yml`: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `scripts/ci-schema-check.sh` (asserts all six pt_* tables exist after migration). cargo-deny deferred — not blocking.
- ✅ **0.1.10 — Nightly backup** (v0.0.6). `scripts/ptask-backup.sh` (Python `sqlite3.backup()` → scp to `mon1:/mnt/cephfs/ptask-backups/`, 30-day retention). User-mode systemd unit at `scripts/systemd/ptask-backup.{service,timer}`, OnCalendar=`*-*-* 03:17`, RandomizedDelaySec=300. Installed + enabled + linger on. First snapshot verified at `mon1:/mnt/cephfs/ptask-backups/ptask-tasks-2026-05-13.db` (5.35 MB).

**Exit criteria — all met:** Binary `pt` deployed to `~/.cargo/bin/pt`. Backfill minted PT-1..PT-204 against the live DB. `pt add` / `pt list` / `pt done` round-trip works on live data (test row PT-205 created + completed during smoke). 15/15 unit tests green; rustfmt / clippy clean. First backup landed in Ceph.

**Rollback:** Delete `pt_extensions` etc., restore `tasks.db.pre-ptask-backup`. Python untouched.

---

### v0.2.0 — DSL, Dates, Recurrence ✅ shipped (with one carryover)

**Goal:** Inline-token quick-add, filter DSL, natural-language dates, RFC 5545 recurrence with `every` (fixed) vs `every!` (completion-relative) semantics.

**Sub-sections:**
- ✅ **0.2.1 — Inline-token grammar** (v0.1.3, v0.1.4 lint fix, v0.1.5 CLI wire). Hand-rolled token scanner (winnow was considered but the grammar is small enough that combinators added noise). Tokens: `@label`, `#project`, `p1..p4`, `~Nm`/`~Nh`/`~Nd` duration, `//description`, `!HH:MM` reminder. Free text is title. Date phrases extracted via the greedy try-longer-then-shorter scanner against `interim`. Persisted into `pt_extensions` (labels JSON, project, duration_min, energy, planned_at) via `tasks::create_with_extensions`. Migration V007 added the `project` column. `pt add` parses by default; `--raw` disables for literal titles.
- ✅ **0.2.2 — Date parser** (v0.1.2). `interim` 0.2 with the `jiff_0_2` feature, `Dialect::Uk`. Operator tz constant Europe/London. `dates::format_iso` emits `+HH:MM` offset with 6-digit microseconds — matches Python `datetime.isoformat()` byte-for-byte.
- ✅ **0.2.3 — Recurrence parser** (v0.1.8 + v0.1.9 lint fix). Patterns: `every day`, `every weekday`, `every N {days,weeks,months}`, `every <weekday[, weekday]+>`, `every <day-of-month[, day-of-month]+>`. `every!` toggles `Mode::Completion`. Serialised as RFC 5545-style `rrule_str` for forward compat. `next_after(&Recurrence, &Zoned)` computes the next instance.
- 🚧 **0.2.4 — Recurrence advancement carryover.** Library function (`recurrence::next_after`) is shipped and tested, but the quick-add → `pt_recurrence` row write and the `pt done` → clone-with-next-deadline flow are not yet wired. Targeted for v0.2.1 (a phase-2 patch) before v0.3.0 begins.
- ✅ **0.2.5 — Filter DSL** (v0.1.6). Hand-rolled recursive-descent parser → SQL WHERE-fragment compiler. Operators `&` `|` `!` `(` `)`; terms `today` `tomorrow` `yesterday` `overdue` `no date` `recurring` `p1..p4` `@label` `#project` `due:` `due before:` `due after:` `search:`. Date phrases inside `due:*` clauses go through the dates module. Search and label LIKE patterns reuse the `\` escape from `tasks::resolve`. Wired into `pt list <filter>` and intersected with `-s`/`-p` flags.
- ✅ **0.2.6 — Saved views** (v0.1.10). `pt_views` CRUD; CLI verbs `pt view save/list/show/rm`. `save` validates the DSL via `filter::parse` before writing. `show` re-parses and runs via `tasks::list_with_filter`.
- ✅ **0.2.7 — `pt next`** (v0.1.7). DAG-ready query: pending tasks whose every `depends_on` UUID resolves to `status='done'` (or doesn't resolve at all, treated as satisfied). Order matches `list_with_filter`. Diagnostic `pending_with_missing_deps` available for a future `pt next --explain`. `petgraph` stays in workspace deps for future cycle-detection / critical-path work.

**Exit criteria — met (with 0.2.4 wiring carrying over):** `pt add "gym monday 8am @health p2 ~45m"` parses every token. `pt list "(today | overdue) & p1"` returns expected set. `pt next` returns DAG-ready list. `pt view save NAME '<dsl>'` + `pt view show NAME` round-trip. Recurrence advancement library works (88 tests green) but the `pt done`-time clone is a v0.2.1 patch.

---

### v0.3.0 — TUI

**Goal:** `pt` with no args opens ratatui. Single-key edits, peek, fuzzy search.

**Sub-sections:**
- **0.3.1 — App skeleton.** `ratatui` + `crossterm`. `App` struct, event loop, layout: list pane + detail pane + filter bar.
- **0.3.2 — List view.** Renders filtered tasks. Cursor navigation `j/k`, page `Ctrl-d/u`, top/bottom `gg/G`.
- **0.3.3 — Single-key edits.** `s` status, `p` priority, `a` assign-self, `l` label, `r` rename, `d` set deadline, `Space` peek detail, `Enter` open edit mode, `c` create, `Del` delete, `/` filter.
- **0.3.4 — Fuzzy search.** `nucleo` matcher. Type to filter visible list incrementally; threaded snapshot matcher.
- **0.3.5 — View switching.** `gv` cycle saved views. `gt` triage queue. `gi` inbox.

**Exit criteria:** TUI usable as primary surface. Operator can capture, edit, complete, and browse without leaving keyboard. <16ms frame on 204-task DB.

---

### v0.4.0 — HTTP Server + Observability

**Goal:** `pt serve` exposes Todoist-style sync API. HMAC webhooks out. Tracing + Prometheus metrics.

**Sub-sections:**
- **0.4.1 — `pt serve` daemon.** `axum` server on configurable port (default `:9501`, leaves `:9500` free for the existing FastAPI). Graceful shutdown via tokio signal.
- **0.4.2 — `POST /sync`.** `{ sync_token, resource_types, commands }`. Returns deltas. `sync_token` = monotonic event-log offset. Commands keyed by UUID for idempotency. `temp_id` resolution for batched creates.
- **0.4.3 — `POST /capture`.** Single-field capture endpoint (`{text: "...", source: "..."}`). Drops into `raw_items` for Python distill (until v0.9).
- **0.4.4 — Outbound webhooks.** Configurable `webhook_endpoints` in config; HMAC-SHA256 signed POSTs on task events. Logged to `pt_webhook_log`.
- **0.4.5 — Tracing.** `tracing` + `tracing-subscriber`. JSON logs by default. Span every HTTP request, every DB write, every webhook dispatch.
- **0.4.6 — Prometheus.** `axum-prometheus` or hand-rolled. Metrics: `pt_tasks_total{status}`, `pt_capture_total{source}`, `pt_dsl_parse_duration_seconds`, `pt_webhook_send_total{result}`. Scraped by existing mon1 Prometheus.

**Exit criteria:** External system (HAL) can POST `/sync` and `/capture`. Webhooks fire on task transitions. Grafana dashboard `pTask Overview` shows live metrics.

---

### v0.5.0 — Telegram Bot

**Goal:** `pt bot` is the Telegram entry point. Inline-token quick-add via DM, morning digest, evening recap, snooze commands.

**Sub-sections:**
- **0.5.1 — teloxide skeleton.** Long-poll. Dialogue state in SQLite via `teloxide::dispatching::dialogue::SqliteStorage`.
- **0.5.2 — `/add` handler.** Free text → inline-token parser → `tasks` write → reply with PT-N echo.
- **0.5.3 — `/list` handler.** Optional filter DSL inline. Reply with formatted task list.
- **0.5.4 — `/done`, `/snooze`, `/defer` handlers.** PT-N or fuzzy.
- **0.5.5 — Morning digest (07:00 UK).** Today + overdue, grouped by status category. Replaces Python accountability morning poke for ptask flow.
- **0.5.6 — Evening recap (18:00 UK).** What got done, what slipped, blocked items.

**Exit criteria:** Operator's existing Telegram → ptask flow now lands via `pt bot`, not Python. Python bot's ptask routes disabled. (Other Telegram flows — HAL, alerts — untouched.)

---

### v0.6.0 — Email + Git Magic Words

**Goal:** Email-to-inbox endpoint. Gitea + GitHub webhooks parse `Fixes PT-N` from commits/branches/PRs and auto-transition.

**Sub-sections:**
- **0.6.1 — Email forward landing zone.** `inbox@ops.puretensor.ai` MX → forwarder → `POST /capture`. `mail-parser` on the JSON wrapper.
- **0.6.2 — Gitea webhook handler.** `POST /webhook/gitea` HMAC-verified. Parse commit messages, branch names, PR titles for `Fixes PT-N`, `Closes PT-N`, `Ref PT-N`, `Skip PT-N`.
- **0.6.3 — GitHub webhook handler.** Same shape, GitHub HMAC.
- **0.6.4 — Magic-word state transitions.** Configurable: branch created → `in_progress`, PR opened → `in_progress` (no separate "in review" — single op, no team), PR merged to `main` → `done`. `Skip` opts out.
- **0.6.5 — Branch name helper.** `pt branch PT-123` prints `feature/PT-123-buy-bread-tomorrow-10am` (Linear-style). Copy via shell pipe.

**Exit criteria:** Pushing a commit `git commit -m "Fixes PT-42: buy bread"` transitions PT-42 to `done` automatically once the push lands.

---

### v0.6.5 — Distillation Shim

**Goal:** Rust takes ownership of the distill **timer and DB writes**; Python remains the ML library invoked via subprocess. Reduces parallel-ops surface.

**Sub-sections:**
- **0.6.5.1 — `pt distill` subcommand.** Runs the existing Python `ingest.distill.main()` as a subprocess. Captures stdout/stderr to `pt_event_log`. Returns canonical task count.
- **0.6.5.2 — Systemd unit swap.** `puretensor-tasks-distill.timer` → `ptask-distill.timer` (calls `pt distill` instead of `python3 -m ingest.distill`). Same cadence: `*-*-* 00,06,12,18:00:00` with 300s jitter.
- **0.6.5.3 — Failure handling.** Non-zero exit → log to pt_event_log, send Telegram alert, leave Python timer dormant (not re-enabled).

**Exit criteria:** Python distill no longer scheduled by its own systemd unit; `pt distill` is the entry point. ML still runs in Python under the hood. Reverting = swap the systemd unit back.

---

### v0.7.0 — Accountability Cutover

**Goal:** Rust owns the escalation state machine + notification dispatch. Python accountability retired.

**Sub-sections:**
- **0.7.1 — Port escalation logic.** Translate `accountability/engine.py` → `ptask-core::accountability`. Six levels, exact transition rules (age ≥2d, dismissal_count ≥1/≥3, last_reminded >48h / >7d).
- **0.7.2 — Notification budget.** `DAILY_BUDGET_MAX=3`, `MIN_HOURS_BETWEEN_TASK_REMINDERS=4`, quiet hours 22:00–08:00 UTC. Reuse `daily_budget` table.
- **0.7.3 — Telegram + email dispatch.** Reuse env credentials (`SMTP_HOST`, `TELEGRAM_BOT_TOKEN`). `lettre` for SMTP, `reqwest` for Telegram. CC `ops@puretensor.ai` on every email (per CLAUDE.md).
- **0.7.4 — Gemini-generated nudge text.** Call HAL via HTTP for message generation; HAL routes to Gemini. Keeps pTask vendor-clean.
- **0.7.5 — `pt accountability run`.** Subcommand for cron. Or built-in scheduler via `tokio_cron_scheduler`.
- **0.7.6 — Systemd unit swap.** `puretensor-tasks-accountability.timer` → `ptask-accountability.timer`. Cadence: every 15 minutes.

**Exit criteria:** Python accountability disabled. Rust runs the escalation cycle. Operator manually verifies one escalation cycle in production. Rollback = swap timer back.

---

### v0.8.0 — Scoring Cutover

**Goal:** Rust owns the composite priority scoring. Python scoring retired.

**Sub-sections:**
- **0.8.1 — Port scoring formulas.** `ptask-core::scoring`. Urgency sigmoid (deadline-driven, 7-day horizon, age decay 21d for undated), dependency centrality via `petgraph::algo::betweenness_centrality`, neglect (view/reopen ratio 14d), manual ((priority-1)/4).
- **0.8.2 — Weighted composite.** 30% urgency + 20% dependency + 20% neglect + 30% manual = `priority_score`. Update all 4 `score_*` columns + `priority_score`.
- **0.8.3 — `pt scoring run`.** Subcommand for hourly cron.
- **0.8.4 — Systemd unit swap.** `puretensor-tasks-scoring.timer` → `ptask-scoring.timer`. Hourly.

**Exit criteria:** Python scoring disabled. Rust recomputes scores hourly. Manual spot-check shows reasonable rankings. Rollback = swap timer back.

---

### v0.9.0 — Native ML Port

**Goal:** Retire Python distill module. SBERT + clustering + Gemini calls all in Rust. Heaviest phase.

**Sub-sections:**
- **0.9.1 — SBERT in Rust.** `candle-rs` + `candle-transformers` loading `sentence-transformers/all-MiniLM-L6-v2`. Or `hf-hub` + ONNX runtime. Benchmark vs Python; if >2x slower, keep Python as a microservice for embeddings only.
- **0.9.2 — Speech-act classifier.** Port to HAL HTTP call (HAL routes to Gemini/Claude). Drop direct Gemini SDK dependency.
- **0.9.3 — Semantic dedup.** Cosine threshold 0.82, reuse SBERT.
- **0.9.4 — Temporal dedup.** 7-day window, same raw_item source.
- **0.9.5 — Clustering replacement.** BERTopic-equivalent: `linfa-clustering` (HDBSCAN or k-means) on SBERT embeddings. Match Python output topic-by-topic on a frozen test set.
- **0.9.6 — Consolidation.** HAL HTTP call, prompt preserved verbatim from `ingest/consolidate.py`.
- **0.9.7 — Source collectors in Rust.** Port `gmail_client.py`, `gdrive_client.py`, `collect_telegram.py`. Reuse OAuth tokens from existing `.env`.
- **0.9.8 — Cutover.** `pt distill` no longer shells to Python. Python retired. Systemd unit unchanged (still `ptask-distill.timer`).

**Exit criteria:** Python `~/puretensor-tasks/` archived to `puretensor-tasks-legacy` (read-only). pTask is fully self-sufficient. One distillation cycle produces canonical tasks matching Python output ≥90% by semantic similarity on a 100-item test set.

---

### v0.10.0 — Multi-Node Fleet Rollout

**Goal:** `pt` runs as a service on multiple fleet nodes. Sync API exercised. Reproducible deploy.

**Sub-sections:**
- **0.10.1 — `cargo dist` release pipeline.** Static musl build via `x86_64-unknown-linux-musl`. Single binary. Checksums. Homebrew tap (cosmetic, for Mac dev).
- **0.10.2 — Ansible playbook in `tensor-scripts/playbooks/ptask.yml`.** Install binary, systemd units, env file, healthcheck.
- **0.10.3 — Canonical-store node election.** SQLite lives on `mon1` (primary) or `arx2` (high-write). Other nodes hit `pt serve` over Tailscale.
- **0.10.4 — Litestream replication.** Continuous backup of `tasks.db` to S3/B2 or Ceph object. RPO < 1 minute.
- **0.10.5 — Fleet read-only clients.** Other nodes install `pt` configured against `https://ptask.ts.puretensor.local/sync`.

**Exit criteria:** Operator can `pt add "..."` from `fox-n0`, `mon2`, or any fleet node, and the write lands on the canonical store. Failure of any non-canonical node loses no data.

---

### v1.0.0 — Polish

**Goal:** Documentation complete, performance pass, release announcement.

**Sub-sections:**
- **1.0.1 — Performance pass.** Profile common verbs (`add`, `list`, `next`). p99 < 50ms on 10k-task DB.
- **1.0.2 — Documentation.** `docs/master-plan.md` (this file, kept current), `docs/cli-reference.md`, `docs/dsl.md`, `docs/recurrence.md`, `docs/sync-api.md`, `docs/migration.md`, `docs/operations.md`.
- **1.0.3 — Manpage.** `pt(1)` via `clap_mangen`.
- **1.0.4 — Shell completions.** `clap_complete` for bash/zsh/fish.
- **1.0.5 — Bretalon post.** Operator-facing announcement (via `/bretalon-post`). "Why we built our own task manager."
- **1.0.6 — Tag and release.** `v1.0.0` tag, GitHub Release with binary, Gitea release mirror.

**Exit criteria:** Anyone with `pt --help` can use the system without reading source. Performance budget met. Docs cover every verb, every config flag, every webhook event.

---

## Critical Files to Reference

Existing Python implementation (do not modify; reference only):
- `~/puretensor-tasks/api/db.py` — CRUD signatures the Rust schema must preserve
- `~/puretensor-tasks/api/models.py` — `PRIORITY_LABEL`, `PRIORITY_MAP` vocabulary
- `~/puretensor-tasks/api/scoring.py` — scoring formula reference (v0.8)
- `~/puretensor-tasks/accountability/engine.py` — escalation state machine reference (v0.7)
- `~/puretensor-tasks/ingest/distill.py` — pipeline orchestration reference (v0.6.5 shim + v0.9 port)
- `~/puretensor-tasks/ingest/classifier.py` — speech-act prompt + class set
- `~/puretensor-tasks/ingest/dedup.py` — SBERT cosine threshold 0.82
- `~/puretensor-tasks/ingest/cluster.py` — BERTopic config
- `~/puretensor-tasks/ingest/consolidate.py` — Gemini consolidation prompt
- `~/puretensor-tasks/cli.py` — CLI parity surface for v0.1
- `~/.claude/skills/ptask/SKILL.md` — to be rewritten in v0.1.8

Existing Rust workspace conventions (mirror layout):
- `~/ptve/Cargo.toml` — workspace package block, dependency style
- `~/ptve/Cargo.lock` — kept in repo
- `~/ptve/.github/workflows/ci.yml` — CI patterns

Operator policy (must adhere):
- `~/CLAUDE.md` — version bump in same commit, dual-remote push, MANDATORY CC on emails, etc.
- `~/.claude/projects/-home-puretensorai/memory/MEMORY.md` — operational context

---

## Verification Approach

**Per-commit (local):**
- `cargo test` green for the workspace.
- `cargo clippy -D warnings` green.
- `cargo deny check` green (no banned licenses, no advisories).

**Per-phase (CI on push):**
- All of the above in GitHub Actions.
- Golden-DB diff: import `tasks.db.fixture` (frozen snapshot), run any migrations, compare schema + row counts against expected via `sqldiff`. Drift = red.
- Integration test: `pt add` then `pt list` round-trip produces matching row in `tasks` + `pt_extensions`.

**Per-cutover (manual, no shadow mode per operator decision):**
- v0.7: spot-check next escalation cycle in production; confirm Telegram + email dispatch identical to Python's last cycle.
- v0.8: spot-check 10 task priority scores after recompute; confirm rankings match Python's last recompute within noise.
- v0.9: 50-task sample of distill output; semantic agreement with Python's last run ≥ 90% via SBERT similarity.

**End-to-end (v1.0):**
- Fresh node bootstrap: install binary via Ansible, point at empty DB, run full distill cycle, verify task creation through Telegram + email + git webhooks.

---

## Repo + Master-Plan Sync

- Repo: `puretensor/ptask` on GitHub + `puretensor/ptask` on Gitea (`100.92.245.5:2222`).
- Master plan lives at: `docs/master-plan.md` in repo, mirrored from `~/.claude/plans/so-we-will-build-curious-pillow.md`.
- At end of every phase: update `docs/master-plan.md` with **status** (`✅ shipped vX.Y.Z`, `🚧 in progress`, `⏳ pending`) and any deltas from the original plan. Commit with the phase's version bump.
- The local plan file and the repo file are kept in lockstep — operator sees the same artifact regardless of surface.

---

## Open Items (none blocking)

- Choice of canonical-store node (mon1 vs arx2) deferred to v0.10.0 design pass.
- Whether to keep `:9500` Python dashboard alive read-only post-v0.9, or shut it cold, deferred to v0.9 retirement decision.
- Whether HAL triage operates synchronously on `/capture` or async via batch — answer locked: async batch every 5 minutes (per earlier research), but revisit if latency feels wrong in v0.5.

---

## What "Done" Looks Like at v1.0.0

- Single binary `pt` deployed to mon1, mon2, mon3, arx1–4, fox-n0, fox-n1, tensor-core.
- `~/puretensor-tasks/` exists only as `puretensor-tasks-legacy/` for historical reference.
- The operator captures, finds, finishes, and reviews tasks exclusively through `pt` (CLI, TUI, Telegram, or HAL).
- The `/ptask` skill calls `pt`; no fallback path remains.
- One canonical source of truth for tasks, replicated by litestream, backed up to Ceph nightly.
- Every commit since v0.1.0 in `git log` shows the version-bump-in-same-commit pattern; tags `v0.1.0` through `v1.0.0` mark each phase boundary.
