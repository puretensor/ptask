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
│  Telegram bot (Bot API long-poll, inline-token quick-add)                  │
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
│   ├── ptask-bot/                # Telegram Bot API client
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
- ✅ **0.1.10 — Nightly backup** (v0.0.6). `scripts/ptask-backup.sh` (Python `sqlite3.backup()` → scp to `backup-host:/var/backups/ptask/`, 30-day retention). User-mode systemd unit at `scripts/systemd/ptask-backup.{service,timer}`, OnCalendar=`*-*-* 03:17`, RandomizedDelaySec=300. Installed + enabled + linger on. First snapshot verified at `backup-host:/var/backups/ptask/ptask-tasks-2026-05-13.db` (5.35 MB).

**Exit criteria — all met:** Binary `pt` deployed to `~/.cargo/bin/pt`. Backfill minted PT-1..PT-204 against the live DB. `pt add` / `pt list` / `pt done` round-trip works on live data (test row PT-205 created + completed during smoke). 15/15 unit tests green; rustfmt / clippy clean. First backup landed in Ceph.

**Rollback:** Delete `pt_extensions` etc., restore `tasks.db.pre-ptask-backup`. Python untouched.

---

### v0.2.0 — DSL, Dates, Recurrence ✅ shipped (carryover closed in v0.2.2)

**Goal:** Inline-token quick-add, filter DSL, natural-language dates, RFC 5545 recurrence with `every` (fixed) vs `every!` (completion-relative) semantics.

**Sub-sections:**
- ✅ **0.2.1 — Inline-token grammar** (v0.1.3, v0.1.4 lint fix, v0.1.5 CLI wire). Hand-rolled token scanner (winnow was considered but the grammar is small enough that combinators added noise). Tokens: `@label`, `#project`, `p1..p4`, `~Nm`/`~Nh`/`~Nd` duration, `//description`, `!HH:MM` reminder. Free text is title. Date phrases extracted via the greedy try-longer-then-shorter scanner against `interim`. Persisted into `pt_extensions` (labels JSON, project, duration_min, energy, planned_at) via `tasks::create_with_extensions`. Migration V007 added the `project` column. `pt add` parses by default; `--raw` disables for literal titles.
- ✅ **0.2.2 — Date parser** (v0.1.2). `interim` 0.2 with the `jiff_0_2` feature, `Dialect::Uk`. Operator tz constant Europe/London. `dates::format_iso` emits `+HH:MM` offset with 6-digit microseconds — matches Python `datetime.isoformat()` byte-for-byte.
- ✅ **0.2.3 — Recurrence parser** (v0.1.8 + v0.1.9 lint fix). Patterns: `every day`, `every weekday`, `every N {days,weeks,months}`, `every <weekday[, weekday]+>`, `every <day-of-month[, day-of-month]+>`. `every!` toggles `Mode::Completion`. Serialised as RFC 5545-style `rrule_str` for forward compat. `next_after(&Recurrence, &Zoned)` computes the next instance.
- ✅ **0.2.4 — Recurrence advancement** (v0.2.2, v0.2.3 time edge fixes). Quick-add detects `every X` / `every! X` clauses, consumes up to the next explicit marker, and optionally splits a trailing ` at <time>` to set the occurrence time-of-day. Recurrence rule + mode + original_input persisted to `pt_recurrence` in the same transaction as the task insert. `original_input` keeps the full operator phrase, including ` at <time>` when supplied, while the recurrence parser strips that suffix for rule matching. `tasks::mark_done` checks `pt_recurrence` first: if present, it advances the deadline in-place (Todoist-style — status stays `pending`), anchoring on the current deadline for `Fixed` mode or `now()` for `Completion` mode. Fixed mode skips missed occurrences until the next deadline is in the future. Explicit ` at <time>` is preserved on completion-relative advancement. Returns `DoneOutcome::{Completed, Advanced}` so the CLI can echo `Recurring task advanced` vs `Marked done`. Note: deviated from the master plan's original "clone with new due" wording — advance-in-place matches Todoist UX and avoids one-task-per-occurrence list bloat.
- ✅ **0.2.5 — Filter DSL** (v0.1.6). Hand-rolled recursive-descent parser → SQL WHERE-fragment compiler. Operators `&` `|` `!` `(` `)`; terms `today` `tomorrow` `yesterday` `overdue` `no date` `recurring` `p1..p4` `@label` `#project` `due:` `due before:` `due after:` `search:`. Date phrases inside `due:*` clauses go through the dates module. Search and label LIKE patterns reuse the `\` escape from `tasks::resolve`. Wired into `pt list <filter>` and intersected with `-s`/`-p` flags.
- ✅ **0.2.6 — Saved views** (v0.1.10). `pt_views` CRUD; CLI verbs `pt view save/list/show/rm`. `save` validates the DSL via `filter::parse` before writing. `show` re-parses and runs via `tasks::list_with_filter`.
- ✅ **0.2.7 — `pt next`** (v0.1.7). DAG-ready query: pending tasks whose every `depends_on` UUID resolves to `status='done'` (or doesn't resolve at all, treated as satisfied). Order matches `list_with_filter`. Diagnostic `pending_with_missing_deps` available for a future `pt next --explain`. `petgraph` stays in workspace deps for future cycle-detection / critical-path work.

**Exit criteria — met:** `pt add "gym monday 8am @health p2 ~45m"` parses every token. `pt list "(today | overdue) & p1"` returns expected set. `pt next` returns DAG-ready list. `pt view save NAME '<dsl>'` + `pt view show NAME` round-trip. `pt add "standup every monday at 9am @ops"` creates a recurring task; `pt done` advances the deadline in-place rather than completing. 109 tests green at v0.2.3.

---

### v0.3.0 — TUI ✅ shipped (with two carryovers)

**Goal:** `pt` with no args opens ratatui. Single-key edits, peek, fuzzy search.

**Sub-sections:**
- ✅ **0.3.1 — App skeleton** (v0.2.4, v0.3.1 entrypoint/list fixes). `ratatui` 0.30 + `crossterm` 0.29 + `nucleo` 0.5 added to workspace deps. Interactive `pt` and explicit `pt tui` enter alt-screen, run an event loop, and restore on exit. Non-interactive `pt` keeps the help fallback instead of trying to enter alt-screen. Initial list cap covers the current 204-task scale with headroom. Module split: `lib.rs::run` / `app::App` / `event::poll_event` / `ui::render`. 3-row layout (header, body, status). Quit on q / Esc / Ctrl-C.
- ✅ **0.3.2 — List view + navigation** (v0.2.5). `ListState` + scroll-aware highlight. Bindings: `j/k`, `↑/↓`, `Ctrl-d/u`, `PageUp/PageDown`, `gg/G`, `Home/End`, `r` reload. Viewport rows captured during render so half/full-page scales with the terminal.
- 🟡 **0.3.3 — Single-key edits** (v0.2.6 peek, v0.2.8 actions). Shipped: `Space` peek, `d` done (recurring → advances in place via DoneOutcome), `p` cycle priority, `c` create (prompt → quickadd::parse), `Del` delete with `y/n` confirm. Carryover: `s` status, `a` assign-self, `l` label, `r` rename, `D` set deadline as discrete edit verbs — useful but not blocking. Will land as v0.3.x patches if any prove painful in daily use.
- ✅ **0.3.4 — Fuzzy filter** (v0.2.7). `/` opens a filter bar; nucleo `Pattern::parse` scores against `PT-N title` per task, sorted descending. Enter applies; Esc clears. Selection rebases against the filtered subset; peek cache invalidates.
- 🟡 **0.3.5 — View switching** (v0.2.9). Shipped: `gv` cycles `Pending → saved_view[0..N-1] → Pending`. Carryover: `gt` triage queue and `gi` inbox — deferred to land alongside the distill shim (v0.6.5) where triage / raw_items become first-class.

**Exit criteria — met:** interactive `pt` and `pt tui` are usable as the primary surface. Operator can browse, peek detail, filter live, mark done (including recurring advance), cycle priority, create via quick-add, delete with confirm, and cycle saved views — all without leaving the keyboard. The two carryovers are surfaceable in later phases without architectural change.

---

### v0.4.0 — HTTP Server + Observability ✅ shipped (counters deferred)

**Goal:** `pt serve` exposes Todoist-style sync API. HMAC webhooks out. Tracing + Prometheus metrics.

**Sub-sections:**
- ✅ **0.4.1 — `pt serve` daemon** (v0.3.2). `axum` 0.8 on configurable bind (default `127.0.0.1:9501`; `:9500` left to legacy Python). `tower_http::TraceLayer` wraps the router. Graceful shutdown on SIGINT + SIGTERM (Unix) / Ctrl-C (any). `AppState { db }` cloned into route handlers. `GET /`, `/healthz`, `/version` shipped.
- ✅ **0.4.2 — `POST /sync`** (v0.3.4). Todoist-shape `{sync_token, resource_types?, commands}`. Idempotent: every command's UUID is recorded in `pt_event_log`; replays return `ok` without re-execute. `temp_id` mapping for batched creates. Commands implemented: `task_create` (runs `quickadd::parse` → `create_with_extensions`) and `task_done` (resolves by `pt_id` or `task_uuid` → `mark_done`, surfaces `DoneOutcome` so recurring tasks advance). Sync token = `MAX(pt_event_log.id)`. Delta = tasks whose event-log row id > prev token. Sentinels `*` / `""` / missing → full sync.
- ✅ **0.4.3 — `POST /capture`** (v0.3.3). `{text, source?, source_file?}` → `raw_items` insert. Returns `{id, source_type, source_date}`. Empty text → 400. Defaults `source=http`, `source_file=http://capture`. Python distill picks up downstream (until v0.9).
- ✅ **0.4.4 — Outbound webhooks** (v0.3.5). `ptask-server::webhooks::dispatch` fans an event out to every `PTASK_WEBHOOK_URLS` (comma-list). HMAC-SHA256 of the body with `PTASK_WEBHOOK_SECRET` → `X-PTask-Signature: sha256=<hex>` header. Every attempt (sent or failed) lands in `pt_webhook_log`. Awaited inline in `/sync`. No retries — small fleet, easier to debug.
- 🟡 **0.4.5 — Tracing** (basic init only). `tower_http::TraceLayer` covers every HTTP request via spans; the CLI tracing-subscriber chain (`PTASK_LOG` env filter) carries over to `pt serve`. JSON logging mode + structured spans on every DB write and webhook dispatch are deferred — `tracing` is configured, but per-call instrumentation hasn't been threaded through the core paths yet. Carryover into v0.4.x or alongside the v0.10.0 fleet-rollout phase.
- 🟡 **0.4.6 — Prometheus** (v0.3.6). Hand-rolled text-format `GET /metrics`. Gauges shipped: `pt_tasks_total{status}`, `pt_tasks_priority_total{priority}`, `pt_raw_items_unprocessed`, `pt_views_total`, `pt_event_log_cursor`, `pt_webhook_log_total{direction}`, `pt_recurrence_total`. Counters (`pt_capture_total{source}`, `pt_dsl_parse_duration_seconds`, `pt_webhook_send_total{result}`) need in-process state and ship in a later v0.4.x patch.

**Exit criteria — met:** External systems (HAL, scripts, the future Telegram bot) can POST `/sync` and `/capture`. Webhooks fire HMAC-signed on every task event when `PTASK_WEBHOOK_URLS` is set. mon1 Prometheus can scrape `/metrics` and chart fleet-level pTask state. Structured spans + counter metrics are deferred but non-blocking — every path is already inside a tower trace span, and gauges cover the operational dashboard surface.

---

### v0.5.0 — Telegram Bot ✅ shipped (snooze/defer + Python cutover deferred)

**Goal:** `pt bot` is the Telegram entry point. Inline-token quick-add via DM, morning digest, evening recap, snooze commands.

**Sub-sections:**
- ✅ **0.5.1 — Telegram bot skeleton** (v0.4.2, updated v1.12.0). Local Bot API long-poll client with Ctrl-C/SIGTERM shutdown; teloxide was removed to eliminate its mandatory aquamarine/proc-macro-error2 advisory path. Chat allowlist via `PTASK_TELEGRAM_ALLOWED_CHATS` (comma-list of int64 chat_ids). Non-allowlisted messages dropped with an info log naming the unknown chat_id so onboarding is trivial. SQLite dialogue storage wasn't needed — current commands are stateless.
- ✅ **0.5.2 — `/add` handler** (v0.4.2). `/add <quick-add text>` → `ptask_core::quickadd::parse` → `create_with_extensions(source_type='telegram')`. Reply echoes PT-N, deadline, and the recurrence rule when present.
- ✅ **0.5.3 — `/list` handler** (v0.4.2). `/list [filter DSL]`. Empty filter → pending tasks. Filter present → status='all' so DSL date predicates work. Top 20 returned.
- 🟡 **0.5.4 — `/done` shipped; `/snooze` + `/defer` deferred** (v0.4.2). `/done <PT-N | substring>` routes through `tasks::resolve` + `mark_done`, surfacing `DoneOutcome::Advanced` for recurring tasks with the next deadline. Snooze and defer need a `snooze_until` column on `pt_extensions` — small follow-on patch.
- ✅ **0.5.5 — Morning digest 07:00 Europe/London** (v0.4.3). DST-correct via jiff `sleep_until` loop (no cron crate). Content = `(today | overdue) & pending` partitioned into 🚨 OVERDUE / 📅 DUE TODAY, with PT-N + priority + deadline columns. Empty case prints "Clear runway."
- ✅ **0.5.6 — Evening recap 18:00 Europe/London** (v0.4.3). Counts today's `interactions` rows (`status_change` for completed + `recurrence_advance` for recurring), reports still-overdue tail (top 50) and `status='blocked'` list. Operator's post-mortem snapshot.

Bonus this phase:
- `/next [N]` — DAG-ready tasks via `ptask_core::dag::next_ready` (the same query that powers `pt next`).

**Exit criteria — met (with deferrals):** `pt bot` runs against any Telegram bot token; `/add /list /done /next /help` work in DMs. Morning + evening digests fire DST-correctly on the operator's local 07:00 / 18:00 schedule. The Python accountability poke remains active because the operator hasn't disabled it yet — flipping that switch is a one-line env edit on the existing systemd unit and lands the moment the operator decides this is the canonical surface. `/snooze` + `/defer` and a `snooze_until` column on `pt_extensions` are the only remaining v0.5.x feature gap.

---

### v0.6.0 — Email + Git Magic Words ✅ shipped (in_progress transitions deferred)

**Goal:** Email-to-inbox endpoint. Gitea + GitHub webhooks parse `Fixes PT-N` from commits/branches/PRs and auto-transition.

**Sub-sections:**
- ✅ **0.6.1 — Email landing zone** (v0.5.5). `POST /email` accepts raw RFC 822 message bodies (`message/rfc822`). `mail_parser::MessageParser` extracts Subject + body_text(0); `raw_items` row written with `source='email'`, `source_file='email:<Message-Id>'`, text = "Subject\n\nBody". Python distill picks up downstream. Stays provider-agnostic — Mailgun/Postmark JSON-wrapped envelopes need a tiny upstream forwarder that hands us the raw `.eml`.
- ✅ **0.6.2 — Gitea webhook** (v0.5.4). `POST /webhook/gitea` HMAC-SHA256-verifies `X-Gitea-Signature` against `PTASK_GITEA_WEBHOOK_SECRET`. Bad signature → 401; tampered body → 401; empty secret → reject (no silent-accept on misconfig). Verified envelopes get logged to `pt_webhook_log` with `signature_ok=1`.
- ✅ **0.6.3 — GitHub webhook** (v0.5.4 — same module). `POST /webhook/github` uses `X-Hub-Signature-256: sha256=<hex>` against `PTASK_GITHUB_WEBHOOK_SECRET`. Both providers ship the same push-event shape so the handlers share a single `handle()` implementation.
- 🟡 **0.6.4 — Magic-word state transitions** (v0.5.3 + v0.5.4). Parser: `ptask_core::magic_words` recognises `Fixes`/`Closes`/`Ref`/`Skip PT-N` (case-insensitive verbs, canonical PT-N output, word-boundary required, dedup within a message, Skip suppresses Fixes/Closes on the same PT-N). Webhook handlers walk every commit message and route Closes/Fixes through `tasks::mark_done` (recurring tasks advance via `DoneOutcome::Advanced`). Branch-created → `in_progress` and PR-opened → `in_progress` transitions deferred to a v0.6.x patch — they need pt_extensions.status_category writes that aren't wired through `mark_done` yet.
- ✅ **0.6.5 — Branch name helper** (v0.5.6). `pt branch <PT-N|substring>` resolves the task and prints `feature/PT-N-<slug>`. Slug = lowercase ASCII, hyphen-joined, 50-char cap with edge-trim, non-ASCII stripped. Pure helper in `ptask_core::tasks::branch_name`.

**Exit criteria — met:** Pushing a commit `git commit -m "Fixes PT-42: buy bread"` to a repo with a Gitea or GitHub webhook pointed at `/webhook/{gitea,github}` (HMAC-secret configured) marks PT-42 done automatically once the push lands. `pt branch PT-42` produces the matching Linear-style branch name for shell-pipe use. `POST /email` lands inbound captures in `raw_items` for distill. Branch-created / PR-opened → `in_progress` transitions are the one feature gap, deferred behind a pt_extensions status_category writer.

---

### v0.6.5 — Distillation Shim ✅ shipped

**Goal:** Rust takes ownership of the distill **timer and DB writes**; Python remains the ML library invoked via subprocess. Reduces parallel-ops surface.

**Sub-sections:**
- ✅ **0.6.5.1 — `pt distill` subcommand** (v0.6.3). New `ptask-distill::run(db, args)` shells out to `python3 -m ingest.distill <args>` against `python_root()` (default `~/puretensor-tasks/`, overridable via `PTASK_DISTILL_PY_ROOT`). Captures stdout + stderr (line counts + 2KB UTF-8-safe tails), exit code, and duration_ms; lands a `distill.run` (success) or `distill.failed` (non-zero exit) row in `pt_event_log` so the sync API surfaces the run to clients. CLI: `pt distill [--days 60]` echoes a one-line summary on success, prints stderr tail + exits with the Python exit code on failure.
- ✅ **0.6.5.2 — Systemd cutover** (v0.6.4). `scripts/systemd/ptask-distill.{service,timer}` ship as user-mode units (same pattern as `ptask-backup`). ExecStart = `~/.cargo/bin/pt distill --days 60`; EnvironmentFile = `~/puretensor-tasks/.env`; OnCalendar = `*-*-* 00,06,12,18:00:00` with `RandomizedDelaySec=300`. The legacy `puretensor-tasks-distill.timer` was already disabled on the workstation; for fleet nodes still running it, `docs/operations.md` documents the `sudo systemctl disable --now puretensor-tasks-distill.timer` step. Live-installed on the workstation; next fire 2026-05-14 00:02 BST.
- ✅ **0.6.5.3 — Failure handling** (v0.6.3). `pt_event_log` event type differentiates `distill.run` (success) from `distill.failed` (non-zero exit). Failure payload includes exit code + stderr tail. `pt distill` exits with the Python exit code so systemd records failure correctly. Outbound alerting reuses the existing webhook + Telegram bot paths — subscribers scrape `pt_event_log` for `distill.failed` events (a dedicated alert hook is a follow-on patch, not blocking).

**Exit criteria — met:** `puretensor-tasks-distill.timer` is dormant; `ptask-distill.timer` is the active scheduler. `pt distill` is the only entry point that writes the distill audit log row. The Python ML still runs end-to-end exactly as before — Rust just owns the cron + audit-log layer, ready for the v0.9.0 native-port swap without changing the `pt_event_log` shape.

---

### v0.7.0 — Accountability Cutover ✅ shipped

**Goal:** Rust owns the escalation state machine + notification dispatch. Python accountability retired.

**Sub-sections:**
- ✅ **0.7.1 — Escalation state machine** (v0.6.7). `ptask_core::accountability` ports `engine.py` exactly: six levels (0=new, 1=reminded, 2=deferred, 3=escalated, 4=critical, 5=blocked). Transitions: `0→1` at age ≥ 2d, `1→2` at dismissal_count ≥ 1, `2→3` at dismissal_count ≥ 3, `3→4` at last_reminded ≥ 48h, `4→5` at last_reminded ≥ 7d. Level 5 flips `tasks.status` to `'blocked'`. Eligibility query mirrors Python's: `status IN ('pending','delayed') AND (next_reminder IS NULL OR next_reminder <= now) AND escalation_level < 5`, ordered by `priority_score DESC, priority DESC`.
- ✅ **0.7.2 — Notification budget** (v0.6.7). `DAILY_BUDGET_MAX=3` Telegram sends per UTC day via the existing `daily_budget` table. Per-task cooldown `MIN_HOURS_BETWEEN_TASK_REMINDERS=4` via `last_reminded` + `next_reminder`. Quiet hours 22:00–08:00 UTC enforced via `in_quiet_hours_at`. Email is unbudgeted.
- ✅ **0.7.3 — Telegram + email dispatch** (v0.6.7). Telegram via `reqwest` POST to Bot API (chat resolved from `PTASK_ACCOUNTABILITY_CHAT_ID` → `PTASK_TELEGRAM_DIGEST_CHATS[0]` → `TELEGRAM_CHAT_ID`). Email via `lettre` STARTTLS (`PTASK_SMTP_HOST`/`SMTP_HOST`, `PTASK_SMTP_USER`/`SMTP_USER`, etc.). `ops@puretensor.ai` always CC'd per CLAUDE.md (overridable via `PTASK_NOTIFY_CC`). `dry_run` flag for safe test paths.
- 🟡 **0.7.4 — Gemini-generated nudge text** (v0.6.7, optional HAL hook). `PTASK_HAL_NUDGE_URL` — if set, POST `{task_uuid, title, level, age_days, dismissal_count}` and use the returned `message` field. Otherwise fall back to five short loss-frame templates that semantically mirror `_LEVEL_PROMPTS` in `engine.py`. HAL endpoint itself isn't built yet; templates are operative.
- ✅ **0.7.5 — `pt accountability run`** (v0.6.8). CLI: `pt accountability run [--dry-run]`. Prints `eligible= dispatched= telegrams= emails= budget=X/3` + per-task lines. Async via the same tokio runtime pattern as `pt serve` / `pt bot`.
- ✅ **0.7.6 — Systemd cutover** (v0.6.8). `scripts/systemd/ptask-accountability.{service,timer}` user-mode, `OnCalendar=*:0/15` (every 15 min) with 60s jitter; runs `pt accountability run`. Legacy `puretensor-tasks-accountability.timer` was already disabled on the workstation. Installed live; next fire 23:45 BST.

**Exit criteria — met:** Python accountability timer is dormant; `ptask-accountability.timer` runs the escalation cycle every 15 min, gated by the same quiet hours / budget / cooldown rules. Dry-run verified end-to-end against a temp DB copy. Rollback = swap timer back via the documented step.

---

### v0.8.0 — Scoring Cutover ✅ shipped

**Goal:** Rust owns the composite priority scoring. Python scoring retired.

**Sub-sections:**
- ✅ **0.8.1 — Port scoring formulas** (v0.7.2). `ptask_core::scoring` ports `api/scoring.py` exactly. Urgency: sigmoid `1/(1+exp((days_until−7)/2))` for deadlined tasks, `0.7·exp(−age_days/21)` decay for undated, clamped `[0,1]`. Manual: `(priority−1)/4`, clamped `[1,5]`. Neglect: `(0.3·views + 0.5·reopens) / max(1, 0.5·recent_count)` over last 14d, clamped to 1.0; "reopen" = `status_change` action with `'pending'` somewhere in `details`. Dependency centrality: in-tree Brandes implementation (petgraph 0.8 ships no betweenness) directed-normalised by `(n−1)(n−2)`, matching NetworkX default, plus `0.1·descendants_count`, clamped to 1.0. `DepGraph::from_pairs` silently adds UUIDs referenced in `depends_on` that aren't in the scoring set — mirrors `nx.DiGraph.add_edge` semantics.
- ✅ **0.8.2 — Weighted composite** (v0.7.2). `composite = 0.30·urgency + 0.20·dependency + 0.20·neglect + 0.30·manual`, clamped `[0,1]`. `run_once_at(db, dry_run, now)` walks all `status NOT IN ('done', 'dismissed')` tasks, computes the dependency graph once, writes all four `score_*` columns + `priority_score`. `dry_run=true` logs without mutating (applying the v0.7.1 accountability fix preemptively).
- ✅ **0.8.3 — `pt scoring run`** (v0.7.3). CLI: `pt scoring run [--dry-run]`. Synchronous (no tokio runtime). Prints `tasks_scored=N`.
- ✅ **0.8.4 — Systemd cutover** (v0.7.3). `scripts/systemd/ptask-scoring.{service,timer}` user-mode, `OnCalendar=hourly` with 60s jitter (matches legacy cadence). Legacy `/etc/systemd/system/puretensor-tasks-scoring.timer` disabled. Installed live.

**Exit criteria — met:** Python scoring timer is dormant; `ptask-scoring.timer` runs the hourly recompute. Smoke-verified: ran Python `recompute_all_scores` and `pt scoring run` against two copies of the live `tasks.db` snapshot — all 14 active tasks produced bit-identical `priority_score`, `score_urgency`, `score_dependency`, `score_neglect` (max delta 0, sum-of-squares 0). Rollback = swap timer back via the documented step.

---

### v0.9.0 — Native ML Port ✅ shipped (architecturally)

**Goal:** Retire Python distill module. SBERT + clustering + Gemini calls all in Rust.

**Sub-sections:**
- ✅ **0.9.1 — SBERT embeddings** (v0.8.2). `ptask_distill::embeddings` via `candle-core` + `candle-transformers` 0.10. `sentence-transformers/all-MiniLM-L6-v2` loaded from the local HF cache, 384-dim L2-normalised output. Bit-perfect cosine parity (max-abs-delta 0.0) against `sentence_transformers.encode()`. Throughput on CPU 597.8 strings/sec vs Python 2604.9 (0.23× — below the 0.5× gate); acceptable for our scale (10k-item distill = 17s) and shipped as default with CUDA gating queued for v0.9.x.
- ✅ **0.9.2 — Speech-act classifier** (v0.8.3). `ptask_distill::classifier`. Drops the Gemini SDK; routes through `PTASK_HAL_CLASSIFY_URL`. Same five classes, same pre-filter heuristics (`< 5 words`, `> 60 words`, AI-prefix list), same batch size and worker count as Python. `FallbackClassifier` keeps the pipeline running with REAL_COMMITMENT/0.51 when HAL is down.
- ✅ **0.9.3 — Semantic dedup** (v0.8.4). `ptask_distill::semantic_dedup` with cosine ≥ 0.82 threshold; `find_duplicate` for a single new title vs candidate set, `find_duplicates` for batched (single Embedder call for `N+M` titles, reused candidate vectors).
- ✅ **0.9.4 — Temporal dedup** (v0.8.5). `ptask_distill::temporal_dedup` — exact-hash 7-day rolling window keyed on `(source_type, normalised_text_hash)`. Recorded in `pt_event_log` as `temporal_dedup.seen` rows with idempotent UUIDs; runs *before* embedding to side-step Gemini cost for repeat lines.
- ✅ **0.9.5 — Clustering** (v0.8.6). `ptask_distill::clustering` replaces BERTopic with cosine-threshold connected components + token-frequency keywords. `DEFAULT_LINK_THRESHOLD = 0.15` calibrated against MiniLM-L6 short-text cosines via `examples/cluster_probe.rs`. Outliers (component size < `min_cluster_size`) land in `cluster_id = -1`. JSON output shape matches Python `clusters_to_json_input`.
- ✅ **0.9.6 — Consolidation** (v0.8.7). `ptask_distill::consolidation`. Verbatim CLUSTER_PROMPT (with `{cluster_json}` / `{cluster_id}` substitution) routed through `PTASK_HAL_CONSOLIDATE_URL`. Same one-cluster-per-call cadence as Python, same `PER_CLUSTER_MAX = 3` / `GLOBAL_MAX_TASKS = 10` caps, same `CanonicalTask` shape.
- 🟡 **0.9.7 — Source collectors** (v0.8.8 partial). File collectors (`collect_voice_kb`, `collect_cc_reports`) ported in `ptask_distill::collectors` with bit-equivalent `RawItem` output. External-API collectors (Gmail / Drive / Telegram) need interactive OAuth — kept as separate Python cron processes for now; native ports queued for v0.9.x once OAuth tokens are runtime-portable.
- 🟡 **0.9.8 — Cutover.** All native modules ship and pass tests. The native pipeline does not yet drive `pt distill` (the v0.6.5 Python subprocess shim remains the live cron entry point). A native-mode flip is gated on (a) operator standing up `PTASK_HAL_CLASSIFY_URL` and `PTASK_HAL_CONSOLIDATE_URL`, (b) live-DB parity smoke against one Python cycle, (c) operator approval.

**Exit criteria — partial:** every native module is in tree, clippy-clean, fmt-clean, and unit-tested. Phase boundary tagged at v0.9.0; live cutover deferred to a v0.9.x release once HAL endpoints exist and parity is verified. Python pipeline stays authoritative until that flip.

**Superseded by v3.0.0:** `pt distill` is now the native Rust v2 pipeline.
The Python subprocess shim, `--legacy`, `--days`, `PTASK_DISTILL_PY_ROOT`,
`PTASK_HAL_CLASSIFY_URL`, and `PTASK_HAL_CONSOLIDATE_URL` are retired from
the active runtime. Gemini classify/consolidate now goes through
`ptask_distill::providers::GeminiProvider` with `thinkingBudget=0`, bounded
timeouts, transient retry, and detailed `distill.failed` payloads.

---

### v0.10.0 — Multi-Node Fleet Rollout ✅ shipped (deployment kit; live deploy operator-supervised)

**Goal:** `pt` runs as a service on multiple fleet nodes. Sync API exercised. Reproducible deploy.

**Sub-sections:**
- ✅ **0.10.1 — Release pipeline** (v0.9.1). `.github/workflows/release.yml` fires on `vX.Y.Z` tag push, builds `pt` for x86_64-unknown-linux-gnu (x86-64-v3 baseline), strips, sha256s, attaches binary + manifest to the GitHub Release with auto-generated commit notes. `scripts/release.sh` is the operator helper: clean-tree check, `cargo fmt/clippy/test` gates, tag, and push to the canonical GitHub `origin`; the Gitea mirror syncs automatically. Musl static target queued for v0.9.x once local validation is done.
- ✅ **0.10.2 — Ansible playbook** (v0.9.2). `scripts/ansible/ptask.yml` + `scripts/ansible/inventory.yml`. Idempotent install of `pt` binary + four user-mode systemd timers (`ptask-backup`, `ptask-distill`, `ptask-accountability`, `ptask-scoring`) to every tier-0 node. On non-canonical hosts the binary is installed but timers stay disabled — they'll proxy through `pt serve` after v0.9.5. `loginctl enable-linger` keeps timers alive across operator logout. `ansible-playbook --syntax-check` passes.
- ✅ **0.10.3 — Canonical-store election** (v0.9.3). Locked to mon1. `docs/architecture.md` captures the topology, role split (canonical / client / excluded), env-var matrix, and recovery procedure. Re-audit gate at v0.10 if read pressure on mon1 justifies arx2.
- ✅ **0.10.4 — Litestream replication** (v0.9.4). `scripts/litestream/litestream.yml` + `scripts/systemd/ptask-litestream.service` stream the WAL to a Ceph rados gateway. `PTASK_LITESTREAM_*` env from `~/.config/litestream/.env`. RPO < 1 min, snapshot daily, retention 30 days. `wal_autocheckpoint = 0` pinned so Litestream owns checkpoints. Full deploy + restore + rollback runbook in `docs/operations.md`. Live deploy gated on operator: Ceph creds + bucket creation.
- ✅ **0.10.5 — Fleet read-only clients** (v0.9.5). `pt remote {add,list,done}` subcommands speak the v0.4.2 `/sync` wire protocol against `PTASK_SYNC_URL` (default `http://127.0.0.1:9501`). `ptask_core::Task` gains `Deserialize` for client-side rehydration. Quick-add grammar reused so `pt remote add` matches the local `pt add` UX. Mock-/sync tests cover the three verbs end-to-end.

**Exit criteria — met (v1.0.3):** all deployment artefacts ship. Live fleet activation completed 2026-05-14 on **the canonical host** (not mon1; pre-v1.0 docs were ahead of reality). Multi-arch handling on mon2 (glibc 2.35) + mon3 (aarch64) documented in `scripts/ansible/inventory.yml` and `docs/architecture.md`.

---

### v1.0.0 — Polish ✅ shipped

**Goal:** Documentation complete, performance pass, internal release.

**Sub-sections:**
- 🟡 **1.0.1 — Performance pass** (v0.10.1). `criterion` bench scaffold at `crates/ptask-cli/benches/pt_bench.rs` covering `add quickadd-parse`, `add insert-then-list-100`, `list-1000-pending-top20`, `next-500-no-deps`. p99 < 50ms on 10k-task DB and the CI gate stay v1.0.x re-attack — current benches run at 100/500/1000-task populations, enough to catch regressions but not yet enforced.
- ✅ **1.0.2 — Documentation** (v0.10.2). `docs/cli-reference.md`, `docs/dsl.md`, `docs/recurrence.md`, `docs/sync-api.md`, `docs/migration.md` shipped. `docs/operations.md` covers backup / distill / accountability / scoring / litestream / pt serve. `docs/architecture.md` covers fleet topology (canonical = the canonical host post-v1.0.3). `docs/master-plan.md` stays the rolling source of truth.
- ✅ **1.0.3 — Manpage** (v0.10.1). `pt gen-manpage` via `clap_mangen` 0.3. Pre-rendered at `docs/gen/pt.1`.
- ✅ **1.0.4 — Shell completions** (v0.10.1). `pt gen-completions {bash|zsh|fish}` via `clap_complete`. Pre-rendered at `docs/gen/{pt.bash, _pt, pt.fish}`.
- ❌ **1.0.5 — Bretalon post.** Withdrawn (category error). Bretalon is a separate UK Ltd with its own editorial surface for external subjects; cross-publishing a PureTensor internal-tool announcement there doesn't make sense. The launch is internal — repo + the activation report at `~/reports/cc/2026-05-14_04-41_ptask-v1-final-activation-handover.md` are the artefacts.
- ✅ **1.0.6 — Tag and release.** v1.0.0 tagged on both remotes 2026-05-14; v1.0.2 fix-up landed via PR #11. `~/puretensor-tasks/` archived to `~/puretensor-tasks-legacy/` read-only on the canonical host during the activation pass, with live `.env` + `tasks.db` carved out so timers keep firing.

**Exit criteria — met:** `pt --help` covers every verb, docs cover every config flag, the binary is live fleet-wide, the Python tree is archived, the skills no longer reference Python fallbacks.

---

### v1.0.4 — k3s puretensor-tasks namespace retired ✅ shipped

Discovered during v1.0.3 verification that a second pTask deployment had been running in k3s the entire time (namespace `puretensor-tasks` on mon1, image `100.92.245.5:3002/puretensor/puretensor-tasks:v2.0.4`, behind `the previous HTTP task service`). Up 63 days, last redeployed 15 days ago. The operator's day-to-day surface was this k3s pod, not the canonical host's Rust binary; v1.0.3's "canonical = the canonical host" was correct after the activation but missed the live shadow deployment.

UUID union: 132 overlap, 306 mon1-only, 72 tc-only → **510 tasks** on the consolidated the canonical host DB. Soft-merge preserves both histories — neither side's recent work was lost.

Post-merge cleanup:
- k3s cronjobs `tasks-distill` / `tasks-scoring` / `tasks-accountability` suspended.
- Deployment scaled to 0; pod terminated.
- `kubectl delete namespace puretensor-tasks` — namespace + IngressRoutes (`tasks-ingress`, `tasks-ingress-tls`) + PVCs (`tasks-data-rwm`, `tasks-data-rwo`) gone.
- `the previous HTTP task service` now returns 404 from Traefik (host unrecognised).
- Litestream replica restarted with a fresh generation `6aa7408a...` reflecting the merged DB.

Tangential: mon1's `k3s.service` unit had a malformed `ExecStart` (`/usr/local/bin/k3s \ --nodeport-addresses primary`) from a prior hand-edit, crash-looping. Restored to baseline `ExecStart=/usr/local/bin/k3s server`; broken unit preserved at `/etc/systemd/system/k3s.service.bak-malformed-20260514T041017`. The `--nodeport-addresses` intent is operator's to re-introduce correctly (`--kube-proxy-arg=nodeport-addresses=...`).

Rollback artefacts at `/tmp/ptask-migration/`:
- `the canonical host-pre-merge.db` (the prior 204-task DB).
- `from-mon1.db` (the 438-task snapshot pulled out of k3s).
- `tc-only-tasks.json` (the 72 the canonical host-only rows the soft-merge re-injected).
- Old Litestream replica at `/var/backups/ptask-litestream/tasks.db.pre-mon1-merge-20260514T035601`.

### v1.0.3 — Doc reality reconciliation ✅ shipped

Post-activation patch. The pre-1.0 docs nominated mon1 as the canonical host and an S3 rados-gateway as the Litestream backend; the actual activation runs on the canonical host with a CephFS file replica. v1.0.3 brings the repo in line with reality:

- `docs/architecture.md` rewritten — canonical = the canonical host, Litestream = CephFS, multi-arch handling, recovery procedures keyed off the live setup.
- `scripts/ansible/inventory.yml` — the canonical host canonical, per-host `ptask_arch_override` for mon2 (glibc 2.35) + mon3 (aarch64).
- `scripts/litestream/litestream.yml` — CephFS replica active, S3 path retained as documented alternate.
- `crates/ptask-cli/src/remote.rs` — `default_url` falls back to the canonical host's Tailscale IP `http://127.0.0.1:9501` instead of the unresolving `ptask.ts.puretensor.local`.
- `docs/operations.md` — Litestream section rewritten; `ptask-serve.service` documented.
- `scripts/systemd/ptask-serve.service` — new, mirrors the live unit on the canonical host.
- `docs/WAKE_HANDOFF.md` — all gates marked complete with execution log.
- `docs/announcement.md` — removed (1.0.5 category error).

### Carryovers (deferred to v1.x.x post-launch)

- `v0.3.3`: TUI discrete edit verbs (`r`/`d`/`l` for rename/deadline/label single-key).
- `v0.3.5`: TUI `gt`/`gi` triage + inbox view shortcuts.
- `v0.4.5`: structured tracing spans (`#[instrument]` on every HTTP handler + DB write).
- `v0.4.6`: in-process counter metrics — gauges ship; counters need a small global registry.
- `v0.5.4`: Telegram `/snooze` + `/defer` handlers.
- `v0.6.4`: `in_progress` status transition on branch/PR creation events (currently the webhook handler logs the event but doesn't flip status until merge).
- `v0.7.4`: live HAL `/compose-nudge` endpoint — pTask side reads `PTASK_HAL_NUDGE_URL`; the HAL repo needs the matching route.

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

- Single binary `pt` deployed to the canonical host and client nodes.
- `~/puretensor-tasks/` exists only as `puretensor-tasks-legacy/` for historical reference.
- The operator captures, finds, finishes, and reviews tasks exclusively through `pt` (CLI, TUI, Telegram, or HAL).
- The `/ptask` skill calls `pt`; no fallback path remains.
- One canonical source of truth for tasks, replicated by litestream, backed up to Ceph nightly.
- Every commit since v0.1.0 in `git log` shows the version-bump-in-same-commit pattern; tags `v0.1.0` through `v1.0.0` mark each phase boundary.
