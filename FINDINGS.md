# FINDINGS — pTask

Running register of review findings. One verb per item: **fixed** (version), **deleted**,
**held** (operator decision, with the reason), or **residue** (recorded, not changed, with
why). Companion ledger: `review-ledger.jsonl`; the Opus reader report this section was
triaged from is archived at `~/reports/cc/fable-pass-2026-09/reports/report-ptask.md`.

## 2026-10-05 — adversarial pass (3.35.0)

Scope: the whole tree at `bd2c5cb` (3.34.0, after PT-2201 removed the cockpit login).
Eight adversarial readers (server, dashboard, MCP and approvals, core storage, parsers,
CLI/TUI/bot, distill and notify, CI and ops) reported about 115 candidate findings, with
overlaps. Each was re-checked against the code, and most were reproduced against the built
`pt` or a scratch sidecar before fixing. Fixes land as several PRs. Every fix carries a
regression test that fails on the old code. Each PR gets an independent second review
before merge.

### Fixed (3.35.0) — access gates

| Area | Finding | Fix |
|---|---|---|
| CI | The repository is public, and every CI job ran on the self-hosted `tensor-core` runner, which is the production host, including `pull_request` runs from forks. | Jobs run only for pushes and same-repository PRs; a GitHub-hosted job fails fork PRs so skipped CI never reads green. GitHub runs a fork's own workflow file, so the real control is the fork-approval setting: `docs/operations.md`, "CI runner". |
| Dashboard sidecar | After PT-2201 the sidecar had no credential and never checked Host. The CSRF check only compared Origin with the client-supplied Host. A DNS-rebinding page under the attacker's own name could read every task and pass that check on writes. | Every request's Host must be an IP literal, `localhost`, the machine's short hostname, `*.ts.net`, or in `PTASK_DASH_ALLOWED_HOSTS` (`.suffix` entries allowed), else 421. Other dotless names are refused: a hostile LAN can resolve them. The decide token is removed from the environment so no `pt`/`aws` child inherits it. |
| Dashboard sidecar / approvals | Any tailnet caller, with no credential and no Origin header, could approve or reject any AP-n. The sidecar runs `pt approve --via dashboard` with `CLAUDECODE` stripped, which defeats "only the operator decides". | Approve/reject requires `X-PTask-Decide-Token` = `PTASK_DASH_DECIDE_TOKEN` (16+ characters). With it unset, decisions are refused. The cockpit asks for the token once per browser. |
| Dashboard sidecar | A decision note starting with `-` reached clap as a flag, and the decision failed. A non-string `title` or `deadline` dropped the connection. `/api/stream` had no cap on threads. | `--note=…`. 400 on type errors. At most 32 streams (503 beyond). |
| `pt serve` dashboard | Basic-auth lockout bypasses: `GET /` checked the password with no throttle, and any non-401 response (the auth-exempt manifest, an extractor rejection) wiped the failure count. | The throttle judges Basic credentials itself (a bearer header is not a guess). `/` sits behind it. A password-less cockpit gets the same Host guard, and anonymous machine-API access answers only to the server's own names. |
| Auth | Named tokens never closed anonymous access. Finishing the rotation off `PTASK_API_TOKEN` and setting the override the bind error suggests would have given every unauthenticated caller Write. | An unrevoked named token closes anonymous access and counts as API auth for the bind check. A non-loopback listener never serves anonymous callers, so revoking the last named token can't open it. |
| Telegram | `/tg/callback` approve/reject worked with decide buttons switched off. `ptdone`/`ptdismiss`/`ptsnooze` accepted any write token and journaled the action as the operator's tap. | Approval taps need `PTASK_TG_APPROVAL_BUTTONS=1` and a `PTASK_TG_FORWARDERS` client. Task taps from any other client are journaled as `telegram via <client_id>`. |

### Fixed (3.36.0) — core task model

| Area | Finding | Fix |
|---|---|---|
| Undo | `pt undo` reversed the latest event whoever wrote it, so it could delete another actor's task; undoing a create left dependency links and children dangling. | Undo reverses only your own events from your own surface (CLI/TUI, not a local `pt mcp` sharing the actor), and a later event from anyone, a link, a child or an approval protects the task; a protected newest change makes undo refuse rather than reach further back. `pt undo` names the PT-N and title, and undoing a create needs a TTY confirmation or `--yes`. |
| Resolve | An empty or blank query matched every task (`pt done ""` completed the only open task). | Refused. |
| Completion | Completing a done task wrote a second `task.completed`. | Error "task is already done"; nothing written. |
| Recurrence | A duplicate or stale completion advanced a recurring task twice. | The advance is conditional on the deadline the caller read; `/sync`, MCP and the dashboard accept an optional `expected_deadline` so sequential duplicates are refused too. |
| Deadlines | Deadlines and snoozes were stored in forms SQLite's date functions misread; snoozes woke on the wrong day. | One stored form (bare date, or the operator-timezone instant with a colon offset); the snooze check parses in Rust, so old rows still wake on time. |
| Recurrence | A date-only recurring deadline gained a time on advance; quick-added monthly rules anchored on an already clamped day (Jan 31 → Feb 28 forever); fixed rules drifted an hour after spring-forward; an unrepresentable next occurrence made the task impossible to complete. | Date-only stays date-only; the anchor is the day the rule was set; fixed mode reapplies the rule's time; an unrepresentable next occurrence closes the task. |
| Reaper | The reaper dismissed tasks touched after its scan. | Each candidate is re-checked (status, reason, unchanged `updated_at`) before dismissing. |
| Search | `pt search` / MCP `task_search` passed raw FTS syntax, so ordinary text errored or matched wrongly. | Free text: each word quoted and ANDed; a trailing `*` is a prefix. |
| Idempotency | An idempotency key replayed whatever command first used it, on any task; `/sync` command uuids shared one namespace across clients and raced to a raw UNIQUE error. | Keyed events journal a command fingerprint (verb + SHA-256 of canonical args); a key replays only the same command, by the same actor, otherwise errors. `/sync` keys are stored as `sync:<len>:<client>:<uuid>` (older forms still replay for their sender); concurrent duplicates both answer with the same `temp_id` map. `--idempotency-key` is refused on verbs that can't replay. |

### Fixed (3.37.0) — server robustness and the filter DSL

| Area | Finding | Fix |
|---|---|---|
| Filter DSL | Unbounded recursion: a deeply nested or very long filter overflowed the stack and aborted `pt serve` (`GET /list`, read scope), `pt mcp` and the bot (which then crash-looped on the redelivered update). | `filter::parse` rejects input over 2048 bytes, nesting over 32 and more than 256 terms; every caller gets a normal error. |
| Filter DSL | `!A` dropped rows where `A` reads NULL (`!#fleet` lost tasks without a project, `!today` undated ones); empty `search:` matched everything; `@domain:mgmt`, `@v1.2`, `#team/ops` were unparseable; day atoms compared the stored offset's date, not London's, and `today` ignored quick-add `due:`. | `NOT COALESCE((A),0)`; empty `search:` is an error; names run to whitespace or an operator; day atoms compare instants against DST-correct London day bounds (legacy non-canonical rows fall back to their date prefix), and `today` also reads `due_at`. `docs/dsl.md` rewritten to match the parser. |
| `/email` | A nested `message/rfc822` chain (~32 bytes a level) overflowed the stack in mail-parser's drop and aborted `pt serve` after the row was inserted. | A structure pre-pass refuses nesting deeper than 32 (decoded base64/QP forwards included, at most two encoded layers, decoded with mail-parser's own MIME decoders and boundaries so the probe can't disagree with the parser, plus a lenient-decode backstop) before anything is stored; parses run on a dedicated large-stack thread; at most 4 at once, taken after authentication (503 + `Retry-After` beyond); 2 MiB body limit. |
| Capture | Concurrent keyed fast-lane captures opened duplicate P5 incidents; a semantic merge stamped the caller's capture key onto any unkeyed incident, so `/capture/resolve` could close a HAL- or operator-created task; the first `sevN` anywhere in the text set severity. | One incident-lane lock covers fast lane and resolve (semantic matching runs before it and is re-checked inside); keys are given to, and resolved on, capture-lane tasks only, identified by an all-digits `capture:` journal key that clients can no longer use (`/sync` uuids and CLI keys starting with `capture` are refused); a literal `[puresentinel sevN]` marker wins. |
| Git webhook | Close directives from any repository holding the secret, every commit, no cap; the skip path was quadratic. | At most 20 distinct PT-N per delivery (rest under `skipped`, with `skipped_count`); optional `PTASK_GIT_CLOSE_REPOS` allowlist; linear-time extraction. |

### Fixed (3.38.0) — terminal-facing surfaces (CLI, TUI, bot)

| Area | Finding | Fix |
|---|---|---|
| Terminal injection | Task, goal and approval text reached the terminal raw: OSC 52 clipboard writes, OSC 2 window titles, cursor moves, C1 controls, bidi overrides, newlines forging output lines (also through core error messages and the query echo), and this module's own palette SGR smuggled through raw-title slots. JSON output passed DEL, C1, bidi and invisible characters raw. | One shared `ptask_core::text` sanitiser, used by the CLI, the TUI and core errors. It covers every control and every Default_Ignorable or Cf character, shown as U+FFFD; line breaks are folded to ␤ in single-line slots. Untrusted text can't carry an ESC into a painted slot (`ui::Cell`), JSON escapes those characters as `\uXXXX`, and tracing fields are one-lined. Ordinary emoji (one VS15/VS16 after a pictograph, joining ZWJ) render intact. |
| Approval spoofing | The approval preview could be forged with CR, CSI, bidi or invisible characters (including variation-selector "emoji smuggling"), and `pt approve` decided anyway. | A strict preview shows every hazard and warns above and below it. `pt approve` refuses a flagged payload without `--force` (exit 7) and the dashboard returns 409 `payload_flagged`. More than 8 emoji selectors in one payload is flagged. An already-decided approval exits 4. |
| Delegate | The `claude -p` command was built from the raw title, so what was shown, copied and run could differ. | Built from the sanitised title, single-quoted. |
| TUI paste | No bracketed paste: a pasted CR acted as Enter and triggered single-key actions on the selected row. | Bracketed paste is enabled (and disabled on exit and on panic); pastes go only into an open prompt or filter. |
| CLI | `pt view show` included done tasks. `pt remote rm` had no confirmation and matched done tasks by substring. `pt remote edit` applied part of an edit, then failed. `--json` was ignored by many verbs. `pt reap` and `plan --write` exited 0 on failure. `pt review --stale-days` panicked on large values. | Views show open tasks (`-s all` restores the old behaviour). `remote rm` confirms (or needs `-y`) and matches open tasks by substring. `remote edit` validates locally and reports partial application. `--json` works everywhere. `reap` names its failures and exits non-zero. `plan --write` names the holds already created. Range errors replace panics. |
| Bot | Messages were split by code point, not Telegram's UTF-16 units, and the first failed chunk aborted the rest. | Split by UTF-16 units (never over 4096, no whitespace-only chunks); every chunk is sent and losses are reported. |

Residue: tag-sequence subdivision flags (England, Scotland, Wales) render as U+FFFD; space look-alikes (NBSP, U+2000–200A) pass the strict preview, since they have visible width and cannot hide text.

### Fixed (3.39.0) — distill, notify and accountability

| Area | Finding | Fix |
|---|---|---|
| Distill coverage | Consolidation was capped at 1–4 tasks (code kept 8), yet the whole chunk was marked processed, so commitments were silently lost. A blank title created a task. | Candidates name the captures they cover; a capture is consumed only when a created or deduped candidate covers it. The rest are walked again in smaller chunks. Claims of 3+ sources need the capture text to support the title; at most 8 sources per candidate. Blank titles are skipped. |
| Distill dedup | Title similarity ignored identifiers ("invoice 4411" deduped onto a done "4412"), and dates/times blocked genuine duplicates. | Distinct numbers and identifiers block a match. Dates, times, years and day numbers are compared as values when both titles carry them, and ignored when only one does. A lone capture whose unsupported match is a closed task is left unconsumed and audited; after 3 runs it gets its own task. |
| Distill provider failures | 429/5xx/timeouts were charged to captures and bisection amplified them; Retry-After was ignored. A capture that always 5xx'd later stalled the queue. | Failures are classed (the worst class across attempts wins; 504 is a timeout). Only a server-class failure confirmed by a passing canary on the failing stage, and followed by real success, is charged. Otherwise the capture is deferred with a `distill.deferred` event and the run fails closed. A row is charged after 6 deferrals since the last successful run, so an all-poison queue still moves. Retry-After is honoured up to 30 s. |
| Distill prompt fence | Untrusted items could forge or break the fence: lookalike dashes, zero-width splits, line separators, a forged "END UNTRUSTED" line. | Each request uses a fence with a random nonce, plus neutralisation of control, invisible and dash-like characters. Each item is capped at 4,000 characters. |
| Distill concurrency | Concurrent runs processed the same rows. | Every run flocks the database's (canonical) directory, never the DB file. A contended run records `distill.skipped`; the third consecutive skip exits non-zero. |
| Notify/accountability | SMTP send had no timeout; the SMTP login was used as From; a bad address aborted the run after Telegram had sent; Telegram limits were counted in chars; email-only levels went silent without email; escalation never reset on advance or reopen; concurrent runs overran the daily budget; nudges were sent before state was re-checked. | Whole-send timeout and a per-run email circuit breaker. `PTASK_SMTP_FROM` and up-front validation; a failing channel counts only against itself. UTF-16 limits. Fallback to the configured channel. Escalation resets on advance and reopen. Conditional budget reservation. A re-check and claim before sending. |

### Fixed (3.40.0) — approvals integrity

| Area | Finding | Fix |
|---|---|---|
| Payload release | `pt approval payload` returned the bytes with exit 0 for pending, rejected, expired and consumed rows, so an executor that skipped the status check acted on an unapproved payload. | Bytes are released only while the approval is in force (approved, unexpired, unconsumed); otherwise the command exits 3/4/6 and prints nothing. Every approval JSON carries `in_force`. The docs say plainly that this is a correctness guard for executors, not a confidentiality boundary: `preview` exposes UTF-8 payloads on every read surface, and the `--any-status` TTY guard is a speed bump. |
| Expiry | Approved rows stayed consumable after `expires_at`; an unswept pending row past expiry read as pending; an unparseable expiry never expired. | `expires_at` bounds execution too (exit 4); expiry fails closed. |
| Dedupe | Digest dedupe returned another requester's AP-n. | Dedupe is scoped per requester (V020, case-folded); a re-request keeps the existing row and says which requested fields were not applied (`deduplicated`, `notice`). |
| Canonical JSON | Duplicate keys were last-wins, big integers went through f64, floats were formatted unlike Python, and the "Python parity" claim was false. | Duplicate keys, out-of-range numbers, underflow and `-0` are refused. Literals of up to 17 digits are accepted. Floats are written exactly as Python's repr. 42,500 fuzzed numbers match Python's digest. |
| Telegram decide | The preview was cut at 800 chars with buttons still shown; limits were counted in chars, not UTF-16 units; the callback never re-checked. | Buttons appear only when the whole payload is shown and readable, in UTF-16 budgets, with no bidi, default-ignorable or control characters (CRLF allowed). `/tg/callback` re-checks with `tap_decidable` and refuses an Approve tap with 403. |
| Identity | The default actor `shell` was shared by an unconfigured `pt mcp` and the CLI; requester ≠ decider compared case-sensitively. | `pt mcp` uses `PTASK_MCP_ACTOR`, then `PTASK_ACTOR`, then `mcp`. Actor names compare trimmed and ASCII case-insensitive. |
| Bounds | Expiry loaded every pending payload blob; titles and notes were unbounded. | Expiry reads only id, seq and expires_at. Title ≤ 300, notes ≤ 16384, payload_name ≤ 255, refused on every surface. |

### Fixed (3.41.0) — quick-add, server lifecycle and residue

| Area | Finding | Fix |
|---|---|---|
| Quick-add | Parsing was quadratic per token: a 200k-token title took 78.7 s in debug, reachable over /sync and MCP. ISO dates were stored as midnight instants, so they were overdue all day. A recurrence phrase overwrote an explicit date. `!12` / `!2026` were taken as reminders. `every 9999999 days` failed the whole add. Tokens were loose: `p+1`, `p01`, `~-30m`, `#123`, `@bob,`. | Parsing is linear. ISO dates are stored date-only. An explicit date beats the rule's first occurrence and keeps the rule's `at` time. Reminders must be strict `HH:MM`. An overflowing rule stays in the title. Loose tokens stay title text, and punctuation is trimmed from labels and projects. |
| Outbound webhooks | Webhooks were delivered out of commit order, with `ts` set to delivery time. Queued events were dropped on shutdown. | One bounded worker per URL (10k backlog; overflow is dropped, logged and counted in `pt_webhook_dropped_total`). Enqueueing happens under a commit-order lock for /sync and git closes. `ts` is the commit time and envelopes gain `event_id`. Events are shared across lanes. Graceful shutdown drains for up to 15 s. |
| Server lifecycle | There was no header-read timeout (slowloris). SIGTERM never finished while an MCP SSE stream or a stalled body was open. | An own accept loop with a 30 s header-read timeout and backoff only on resource errors. Shutdown cancels MCP streams, gives in-flight requests 10 s, then drains. The worst case is about 30 s. |
| Core residue | An old binary failed on a newer schema with a bare refinery error. Concurrent first-open migration failed once. `backfill_all` and subtask promotion weren't atomic. History sorted by local `ts` text. MCP's `discovered_from` link was written after the create committed. The digest's "recently done" was keyed on `updated_at`. | An explicit "schema newer than this binary" error, plus a documented rollback. One migration retry. IMMEDIATE transactions. Ordering by id. The link is written in the create transaction. Completion uses the status-change time. |
| CLI and docs | Bearer tokens went over plain http to non-tailnet hosts silently. `pt plan` and `--stale-days` could panic. The bot's `/add` example used a past date. A blocked close in the sidecar returned 500. Dashboard `pt` processes didn't get `PTASK_WAL_AUTOCHECKPOINT=0`. The docs on `task_delete`, metrics and webhook fan-out were wrong, and docs claimed `start` was gated by blockers. | A warning is printed, and tailnet and MagicDNS names are recognised with a 1 s lookup bound. Range errors replace the panics. The example date is relative. The sidecar returns 409. The unit sets the variable. The docs match the code; a test checks the metrics table. |

### Operator actions (outside the repository)

- GitHub → Settings → Actions → General: require approval for all external contributors.
  Without it a fork's PR can still target the self-hosted runner.
- Set `PTASK_DASH_DECIDE_TOKEN` in `~/puretensor-tasks/.dashboard.env` (both instances)
  to decide from the cockpit.
- If any cloudflared tunnel still routes to the sidecar (README v0.20.1 documents one),
  remove it. The sidecar has no login. Unknown public Host names now get 421.
- If you reach the cockpit by a MagicDNS short alias other than the box's own hostname,
  add it to `PTASK_DASH_ALLOWED_HOSTS`; consider `PTASK_DASH_ALLOWED_HOSTS=.<tailnet>.ts.net`.
- Tokenless clients that reach `pt serve` by a DNS name other than `localhost`, the short
  hostname or `*.ts.net` (reverse proxy, LAN name) need that name in `PTASK_DASH_ALLOWED_HOSTS`.
- Check that nexus forwards taps with its `nexus` token (`pt token list` shows last use);
  on the legacy env token its taps are journaled as `telegram via legacy-env`.

## 2026-09-25 — residue follow-up (3.34.0)

Scope: the 3.33.0 residue below, plus what the follow-up read turned up. Every fix has a
regression test. The cycle-check, recurrence-anchor, distill-budget, SSE and webhook tests
were also run against the old code, and each fails there.

### Fixed (3.34.0)

| Area | Finding | Fix |
|---|---|---|
| Dashboard (Rust) | M1 residue: `api_tasks/critical/stats/timeline/heatmap/events`, the SSE start cursor and its 2 s poll ran pooled SQLite on the async workers. The poll swallowed read errors. | All on the blocking pool. The poll logs a failed read. |
| Recurrence (V019) | `every month` from Jan 31 chained from the clamped Feb 28 and stayed on the 28th. | `pt_recurrence.anchor` holds the operator-set deadline (at creation or on an edit). A fixed plain-monthly rule counts whole intervals from it. Pre-V019 rows keep the old rule until their next deadline edit. |
| Core | The dependency cycle walk re-prepared per node. Past 10,000 visited nodes it skipped the rest of the graph, so a cycle behind a deep chain was accepted. | One recursive CTE, no cap. |
| Distill | Worst-case provider time (~97 min) exceeded the unit's 30-min kill, which lands before any row is marked processed, so finished chunks were redone. | No new provider call after 20 min of chunk work. The run closes out normally. |
| Webhooks | Outbound delivery was awaited per event inside `/sync` and the git close webhook. At 10 s per hung URL, a 200-command batch could hold the request for over half an hour. | Each request streams its events to one background task that delivers them in order. The request never waits on a subscriber. Events already queued still go out if the client drops. |
| Metrics | `MAX(ts)` compared ISO strings. With mixed offsets, the distill and notification age gauges read stale (`10:30+01:00` > `09:45Z`). | Compared as epoch seconds. |
| Schema (V019) | Four indexes no query reads (`idx_pt_recurrence_next`, `idx_pt_webhook_log_source`, `idx_tasks_kind`, `idx_goals_status_seq`). | Dropped. |
| CLI | `pt scoring run --diff` called `why` per task, and each call rescored every task (O(N²)). It labelled the stored ordering "v1". | One scoring pass (`scoring::composites_v2`), labelled as the stored ordering. |
| TUI | The list title said "pending" on every saved view. | Shows the active view. |

### Deleted

- The quick-add → `NewTask` + `Extensions` builder, six copies. Replaced by
  `QuickAdd::task_parts`.
- Scout → report defaulting in the CLI and MCP. Replaced by
  `tasks::kind_and_deliverable`.
- The approvals wire-payload parser in the HTTP route and the MCP tool. Replaced by
  `approvals::payload_from_wire`.
- Duplicated code: two inline event-uuid lookups in `tg.rs`, the dashboard's cursor
  query, `main()`'s second error branch and a double `--note` check.
- Dead or unused code: `App::selected`, the `ptask_bot::{PtCommand, BotConfig}` and
  `ptask_tui::App` re-exports, and a stale `dead_code` allow in `remote.rs`.
- The dashboard sidecar's second deadline scan in `q_stats`.

### Residue (recorded, not changed)

- `ptask-notify` still builds a `reqwest::Client` per send. The reason from 3.33.0 stands.
- Webhook signature rejects still write a fixed-size audit stub, as in 3.33.0.
- `/api/critical`, `/api/timeline` and `/api/heatmap` are documented API
  (`docs/sync-api.md`, `dashboard/README.md`) that the cockpit no longer calls. Whether
  to remove them needs a check for external consumers on the host.
- `due_within_7d` counts overdue tasks in the Python sidecar but not in `pt serve`. No
  UI reads the key.

## 2026-09-25 — review pass (3.33.0)

Scope: the whole tree (all seven crates, dashboard sidecar, scripts, units, workflows,
docs). Five parallel readers produced ~60 candidate findings. Each was re-verified
against the code, and most were reproduced against the built `pt`, before fixing. Every
fix carries a regression test. The concurrency, migration, capture and episode tests were
also run against the old code to confirm they fail there. `cargo test --workspace` went
from 443 to 473 tests, with source-grep and dead-code tests removed and behavioural ones
added. Clippy (both feature sets), fmt, actionlint, the schema check and the
approvals/goals contracts are all green.

### Fixed (3.33.0)

| Area | Finding | Fix |
|---|---|---|
| Accountability | 3→4 and 4→5 gated on `last_reminded` age, which level 3–4 email restamps every 4 h, so level 4 was unreachable while email worked. Escalation was committed before delivery, so a dead channel walked tasks up the ladder unseen, and a failed level-5 email was never retried. Escalating bumped `updated_at` (neglect score, reaper). | Time-at-level for 3/4. The new level persists only after a successful send. `updated_at` is untouched. |
| Approvals | `expires_at` was never enforced, and nothing ever ran `expire()`. `parse_expires_in` panicked on `5日` / `99999999d`. `accountability run --dry-run` sent real approval pings. | `decide` expires stale rows. `request` sweeps expired rows before its dedupe. The 15-min timer sweeps. Parser is checked. One shared config. |
| Approvals (V018) | FK `task_uuid REFERENCES tasks(id)` made any task named by an approval undeletable, with no way out past the lock trigger. | Table rebuilt with a soft reference. Triggers and indexes recreated verbatim. Upgrade test added. |
| Quick-add / recurrence | `https://` split the description. `every monday at 9` stored a rule that `mark_done` could never re-parse. `every 9999999 days` panicked. `Every` (capital E) was ignored. `--deadline "next friday"` was stored raw. `edit --deadline ''` errored. `-s in_progress` / `-s todo` matched nothing. `overdue` included dismissed tasks. `!re:invoice` was eaten as a reminder. | See commit `b2eaede` and `f2721a2`. Help text, bot and docs no longer promise natural-language dates. |
| Capture | The sev-marker parse was fooled by "several". The trimmed `client_key` was used inconsistently (an empty key merged unrelated incidents). A semantic-merged key re-opened an episode on every re-send. A reopened incident could not be resolved again. `/resolve` returned 500 for an unknown uuid. | One normalised key. Absorbed keys count as open. The resolve event uuid includes `updated_at`. |
| Server | Approvals, read and metrics routes ran blocking SQLite on async workers (M1 residue, and a v3.31 regression). `/sync` rescored after every command. Token resolve wrote on every request. `pt serve`'s Basic auth had no lockout. | `db_response` / `db_value`. One rescore per batch. `last_used_at` written at most every 5 min. Per-peer lockout layer. |
| Telegram | Raw titles in HTML parse mode (400 errors, circuit breaker). No 4096-char split, so a long morning digest was dropped. Approval ping title/note were uncapped. | Escaped. Split on line boundaries. Bounded. |
| CLI | A shared `--idempotency-key` failed the second task of a multi-task command, and a local retry hit a raw UNIQUE error. `done` / `bulk` / `review` stopped at the first failure. `rm` on a non-TTY printed "aborted" and exited 0. Six verbs ignored `--json`. `pt delegate` told agents to run `pt capture`, which doesn't exist. Remote mutations full-synced the table. `remote list -f -p` dropped `-p`. | Per-task keys plus replay. Every task is attempted and failures reported. `rm` refuses. `emit()`. No-delta cursor. Server-side `/list`. |
| Core | DEFERRED read-then-write transactions failed instantly under a concurrent writer. v2 scoring computed v1's graph, centrality and N interaction queries, then discarded them. Goal relink under a key collided on the event uuid. The `set_parent` cycle check failed open past depth 16 and ran outside the write transaction. | IMMEDIATE. v1-only work gated. `K:unlink`. Fail closed, in-transaction. |
| Dashboard (py) | The login throttle keyed on the tunnel connector, so anyone could lock the operator out. Non-object or non-UTF-8 bodies got no response. A due-today task showed "0d over". SSE fired a full reload every 15 s. | `Cf-Connecting-Ip` from `PTASK_DASH_TRUSTED_PROXIES` only. 400. Calendar days. Change-driven. |
| Distill | Re-embedded the ~1–2k-title universe per candidate. The downloading model loader had no timeout. Captured text could close the prompt fence. The prompts invited the empty answer the pipeline charges. | Universe embedded once per run. Cache-only loader. Fence sealed. 1–4 tasks requested. |
| Ops / CI | Backup unit had no start timeout and no ssh timeouts, so a hang never alerted. Scoring, reaper and export lacked `.env` (T7 pragma) and `OnFailure`. Export was missing from the playbook. Release notes' commit list was always empty (shallow checkout). The schema check built a second LTO release. | See commit `c340a50` and `16ad527`. |

### Deleted

`tests/test_01..10` + `tests/source.py` (never run by CI; regexes over source, plus
"negative controls" that asserted on their own Python copies). Replaced by behavioural
Rust and unittest tests where coverage was missing; `docs/pt2121/ptask.md` records the
mapping. Also deleted: `ptask-distill` `clustering.rs`, `types.rs` and
`examples/cluster_probe.rs`, and `semantic_dedup::find_duplicates` (no callers). From core:
`event_log::exists`, `tasks::priority_label`, `dag::pending_with_missing_deps`,
`pt_id::mint_for`, `SortKey::sql_bare`, `renumber_placeholders` (always a no-op) and
`QuickAdd::warnings` (never written; the always-empty `warnings` key left `pt add --json`).
Also deleted: `RemoteClient::list` and `PtaskMcp::rescore`, plus the duplicate
`escape_like`, `combine_date_with_time` and `ct_eq`.

### Residue (recorded, not changed)

- **M1** The Rust dashboard's `api_*` handlers and its SSE poll still run SQLite on the
  async workers. The approvals, read and metrics routes are now offloaded.
- `ptask-notify` builds a `reqwest::Client` per send. The volume is a few per 15 min, and
  a process-wide client shared across per-command and per-test runtimes risks "dispatch
  task is gone".
- Webhook signature rejects still write a fixed-size audit stub. This was a deliberate
  earlier choice and is tailnet-only.
- `every month` from the 31st drifts to the 28th (it chains from the clamped deadline). A
  fix needs a stored day anchor.
- `pt distill`'s worst-case provider time (~97 min) exceeds the unit's 30-min timeout.
  Progress is recorded only at the end of the run.
- Unused indexes (`idx_pt_recurrence_next`, `idx_pt_webhook_log_source`, `idx_tasks_kind`,
  `idx_goals_status_seq`) are left in place.
- The quick-add → `NewTask` builder is duplicated across six surfaces.
- The dashboard's unused `/api/critical`, `/api/timeline` and `/api/heatmap` routes are
  kept, pending an external-consumer check.

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
