# FINDINGS — reconciled adversarial review (a ∪ b ∪ c)

Workspace version **3.8.9** (patch bump in the first fix commit only).

Reviewer provenance is tagged **a** / **b** / **c**. Overlapping fixes keep the
strongest implementation; unique fixes are imported after a re-check against
the source. Incorrect or incomplete implementations are dropped with the
failure scenario that disqualified them.

## Suite status

```bash
bash scripts/ci-version-check.sh
cargo test --workspace --locked --offline --lib
cargo test --manifest-path tests/Cargo.toml --locked --offline
python3 -m pytest tests/ -q
python3 -m pytest scripts/tests/ -q
```

- `python3 -m pytest dashboard/tests/` from the repo root **fails at collection**
  (`ModuleNotFoundError: No module named 'server'`). CI runs
  `python -m unittest discover -s tests -v` with `working-directory: dashboard`.
  Pre-existing; not chased.

## Overlapping findings

### 1. HTTP `/capture` UNIQUE collision, keyed race, and crash-window skip

- **Severity:** high
- **Provenance:** a#1, b#1, c#F1
- **Location:** `crates/ptask-server/src/routes/capture.rs:225-275`
- **Failure scenario:** After V014 `UNIQUE(source_file, text)`, unkeyed posts
  all used `source_file = "http://capture"`. A second independent POST of the
  same text 500'd. Two concurrent keyed posts both missed the SELECT and the
  loser 500'd. A keyed replay after the raw_item landed but before the
  fast-lane create returned `duplicate: true` with no `task_uuid`, so a
  sev≥3 incident never materialised until distill.
- **Kept:** A's trim + minted `http://capture/{uuid}` for unkeyed identity;
  C/A `insert_idempotent` (closes the check-then-insert race); B's crash
  window (`duplicate && !processed` falls through to the fast lane) and
  fail-closed open-incident list / exact-key refresh; A's lookup of the
  original PT-N on a *processed* replay so heartbeats do not bump
  `capture_count`.
- **Dropped:** C still used the shared `"http://capture"` breadcrumb, so
  unkeyed identical text still 500'd. A's early-return on every duplicate
  skipped the crash window. B continued the fast lane on every duplicate,
  so a heartbeat replay bumped `capture_count` (the re-nag loop
  `client_key` exists to kill).
- **Status:** FIXED `d1b0214`

### 2. `/capture/resolve` treated a list error as “nothing open”

- **Severity:** high
- **Provenance:** a#2, b#2, c#F3
- **Location:** `crates/ptask-server/src/routes/capture.rs:79-96`
- **Failure scenario:** `with_conn(...).unwrap_or_default()` turned a SQLite
  error into HTTP 200 `{closed:0}`. Recovery callers stop retrying while the
  incident stays open.
- **Kept:** A/B's 500 with the error text (C used a generic string).
- **Status:** FIXED `2cfe6cb`

### 3. `/sync` advanced the cursor past a task the delta failed to load

- **Severity:** high
- **Provenance:** a#3, b#4
- **Location:** `crates/ptask-server/src/routes/sync.rs:176-186`
- **Failure scenario:** `if let Ok(t) = task_by_uuid(...)` dropped any fetch
  error (not just a deleted row) while the cursor still advanced. The client
  never saw the update and would not retry it.
- **Kept:** A's `task_by_uuid_opt` (preserves the original `task_by_uuid`
  signature for command lookup). B's Option-return on `task_by_uuid` is
  equivalent but forced every caller to unwrap.
- **Status:** FIXED `752932a`

### 4. Inbound `/email` UNIQUE collision on Message-ID replay and missing Message-ID

- **Severity:** high
- **Provenance:** a#4, b#3, c#F2
- **Location:** `crates/ptask-server/src/routes/email.rs:54-85`
- **Failure scenario:** A retried RFC822 with the same Message-ID hit
  `UNIQUE(source_file, text)` and 500'd. Two messages without a Message-ID
  shared `email:none` and collided the same way, so the second inbound mail
  was lost.
- **Kept:** A's trim-empty Message-ID + `email:anon:{uuid}` (B only special-
  cased the literal `"none"` after `unwrap_or`; C used `insert_idempotent`
  but still shared `email:none`).
- **Status:** FIXED `57e92ce`

## Unique findings (imported)

### 5. `mark_done` on an already-done task wrote a second completion

- **Severity:** medium
- **Provenance:** a#5
- **Location:** `crates/ptask-core/src/tasks.rs:598-617`
- **Failure scenario:** A dashboard double-submit (or two agents) ran
  `UPDATE ... WHERE id=?` with no status guard, inserted another
  `task.completed` event, and bumped `updated_at`. Undo/sync treated a no-op
  as a fresh completion.
- **Dropped from a:** rejecting `mark_done` on a dismissed task. No concrete
  failure that dismissed→done is wrong; a git `Closes PT-N` on a dismissed
  task would then 500. Already-done is the proven no-op bug.
- **Status:** FIXED `9ae97aa`

### 6. `/metrics` fail-open to zero gauges on HTTP 200

- **Severity:** high
- **Provenance:** a#6 (fail-closed gauges); b#6 (off the async worker, same file)
- **Location:** `crates/ptask-server/src/routes/metrics.rs:21-51`
- **Failure scenario:** `current_cursor` / inbox counts used `unwrap_or(0)`,
  and `render()` errors were served as HTTP 200 with a comment. Prometheus
  treated a broken store as `pt_event_log_cursor 0` (caught up) and did not
  page. Concurrent scrapes also parked tokio workers behind the r2d2 pool.
- **Status:** FIXED `ce2500e`

### 7. MCP `task_capture` without `client_key` collapsed identical text

- **Severity:** high
- **Provenance:** a#7; b's `!row.processed` crash-window (bundled with b#1 / 5ced905)
- **Location:** `crates/ptask-server/src/mcp.rs:391-410`
- **Failure scenario:** Unkeyed captures used `source_file = mcp://{actor}`.
  Two independent “buy milk” notes from the same agent UNIQUE-collapsed into
  one inbox row. A crash after the raw_item insert and before the fast-lane
  create then skipped the lane on `!duplicate`, so the incident never
  materialised. `!row.processed` still satisfies the PT-1687 frozen test
  (processed rows do not mint a second task).
- **Status:** FIXED `455f38b`

### 8. Garbage `/sync` cursor became a full sync with empty tombstones

- **Severity:** high
- **Provenance:** a#8
- **Location:** `crates/ptask-server/src/routes/sync.rs:230-247`
- **Failure scenario:** `sync_token: "12x"` parsed as `0`, which is a full
  snapshot and `deleted_task_uuids: []`. A delta client would merge every
  live task and never drop server-side deletes. The token is now parsed
  before commands and rejected with 400.
- **Status:** FIXED `752932a`

### 9. Git webhook close faults returned HTTP 200 so the provider would not retry

- **Severity:** high
- **Provenance:** a#9
- **Location:** `crates/ptask-server/src/routes/webhook_git.rs:178-199`, `252-270`
- **Failure scenario:** A failed idempotency lookup or blocking-pool abort
  was appended to `errors` and the handler still returned 200. Gitea/GitHub
  treat 2xx as delivered; the `Closes PT-N` directive was lost. Permanent
  misses (unknown PT-N) still 200.
- **Status:** FIXED `205bf61`

### 10. `/capture/resolve` skipped a matched task that would not load

- **Severity:** high
- **Provenance:** a#10
- **Location:** `crates/ptask-server/src/routes/capture.rs:101-148`
- **Failure scenario:** `capture_key` matched an open incident,
  `resolve_for_lookup` (or `mark_done`) failed, and the handler continued
  then returned HTTP 200 `{closed:0}` — the same body as an unknown key.
  Recovery stopped; the incident stayed open.
- **Status:** FIXED `a115924`

### 11. Capture fast-lane incident list fail-open (create rather than drop)

- **Severity:** low (a deferred) / high as a duplicate-PT-N (b#1)
- **Provenance:** a#11 (DEFERRED), b#1 (fixed)
- **Location:** `crates/ptask-server/src/routes/capture.rs:297-317`
- **Failure scenario:** If listing open incidents errors, A's fail-open still
  creates a new sev≥3 task (duplicate PT-N). B 500s so the client retries
  when the store is healthy. A 500 is not a drop if the caller retries 5xx
  (puresentinel does).
- **Kept:** B's fail-closed. Semantic-match refresh still falls through to
  create (fuzzy). Exact-key refresh failure also 500s so a second PT-N is
  not minted for the same live incident.
- **Status:** FIXED `d1b0214`

### 12. `/mcp` rejected valid REST credentials

- **Severity:** medium
- **Provenance:** b#5
- **Location:** `crates/ptask-server/src/lib.rs:56-92`
- **Failure scenario:** HAL sends `Authorization: bearer pt_…` (lowercase
  scheme, accepted on `/sync` via `presented_token`). `/mcp` 401'd. Token
  lookup also ran on the async worker, so a stalled pool parked `/healthz`.
- **Dropped from b:** mapping a blocking-pool abort to 401 (`Err(_) => false`).
  That tells the client the credential is wrong. Abort is now 500.
- **Status:** FIXED `05beeb4`

### 13. Read-route SQLite on async workers

- **Severity:** medium
- **Provenance:** b#6
- **Location:** `crates/ptask-server/src/routes/read.rs`
- **Failure scenario:** Same class as the existing `/sync` starvation bug:
  concurrent `/list` holds the pool; tokio workers park for up to the 30s
  timeout and `/healthz` stalls even though liveness does not touch SQLite.
- **Status:** FIXED `da3e8c4`

## Deferred (concrete but out of scope / already pinned)

| Item | Provenance | Why deferred |
|------|------------|--------------|
| `format_pt_id` emits `PT--1` for negative counters | a suspected, b, c#F4 | Requires a corrupt `pt_counters` row; contract tests pin current behaviour (`tests/contract_pt_id.rs`). |
| `split_time_suffix` keeps a dangling `at` | a suspected, b, c#F5 | Operator typo; contract tests pin it (`tests/contract_recurrence.rs`). |
| Dashboard `act_edit` applies fields in separate transactions | b | Title can commit then deadline reject (422). Fix needs a single transaction API on `tasks::*`. |
| Telegram bot mutations have no `update_id` idempotency | b | Advancing offset always avoids duplicate `/add` on redelivery; not advancing without keys would duplicate. Needs `event_uuid=tg:{update_id}` plus replay. |
| Semantic incident dedup fail-open | b | Intentional (`dedup.rs`): sev≥3 must not be dropped if the embedder is missing. |
| Unauthenticated write when no env token | b | Documented loopback / `PTASK_ALLOW_UNAUTHENTICATED` back-compat. |

No NEEDS-HUMAN items: every overlapping implementation disagreement was
resolvable from the failure scenario and the surrounding contract (PT-1687,
V014 UNIQUE, heartbeat `client_key` docs).
