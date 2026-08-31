# Contract-test findings (pTask)

## Test commands

```bash
# Rust contract tests (standalone package under tests/)
cargo test --manifest-path tests/Cargo.toml

# Python contract tests (failure-alert env helpers; network mocked)
python3 -m pytest tests/ -q

# Pre-existing Python tests (still pass)
python3 -m pytest scripts/tests/ -q
```

## Behaviors now covered

| Area | Source (file:line) | Contract test file |
|------|--------------------|--------------------|
| PT-N string formatting (`format_pt_id`) | `crates/ptask-core/src/pt_id.rs:14-16` | `tests/contract_pt_id.rs` |
| API token SHA-256 hashing + scope label parsing | `crates/ptask-core/src/tokens.rs:26-34`, `62-66` | `tests/contract_tokens.rs` |
| v2 status parse/legacy/terminal mapping | `crates/ptask-core/src/status.rs:24-71` | `tests/contract_status_dates.rs` |
| Legacy timestamp normalisation (`parse_iso_to_utc`) | `crates/ptask-core/src/dates.rs:66-90` | `tests/contract_status_dates.rs` |
| Recurrence `at <time>` suffix splitting | `crates/ptask-core/src/recurrence.rs:54-65` | `tests/contract_recurrence.rs` |
| Failure-alert env precedence (`first_nonempty`, digest chat list) | `scripts/ptask_failure_alert.py:12-26` | `tests/test_failure_alert_contract.py` |

### Edge cases pinned

- **PT-ID**: zero, large positives, negative counters, `i64::MIN`/`MAX` (formatter does not validate).
- **Tokens**: empty input SHA-256 vector, UTF-8 NFC vs NFD hashing difference, unknown scope strings, emoji scope.
- **Status**: `in-progress`/`doing` aliases, empty/unknown parse errors, terminal vs active states, dual-column `columns()` mapping.
- **Dates**: empty/whitespace/garbage/impossible dates return `None`; offset ISO, SQLite `datetime`, and date-only inputs normalise to UTC hours.
- **Recurrence**: bare phrases, last-`at` wins, casing preserved in rule half, trailing `at` without time token.
- **Failure alert**: whitespace-only env vars skipped, first digest chat id from comma list, no network when unconfigured.

## Pre-existing test status

- `cargo test -p ptask-core --lib`: **242 passed** (unchanged).
- `python3 -m pytest scripts/tests/`: **5 passed** (unchanged).
- `python3 -m pytest dashboard/tests/`: **fails at collection** (`ModuleNotFoundError: No module named 'server'` when not run from `dashboard/`). Left as-is per instructions.

## Suspected bugs

### 1. `format_pt_id` accepts negative counters

**Location:** `crates/ptask-core/src/pt_id.rs:14-16`

**Scenario:** If a bug or manual SQL update produced a negative `pt_counters` value, `format_pt_id(-1)` returns `"PT--1"`, which is not a valid operator-facing PT-N and would break resolve/display assumptions.

**Contract test:** `tests/contract_pt_id.rs::format_pt_id_negative_and_max_i64` asserts current behavior (`"PT--1"`).

### 2. `split_time_suffix` leaves dangling `at` when no time follows

**Location:** `crates/ptask-core/src/recurrence.rs:54-65`

**Scenario:** Input `"every monday at "` (trimmed to `"every monday at"`) does not match the `" at "` separator (no trailing space after `at`), so the function returns `("every monday at", None)` instead of treating it as a bare weekly rule. Downstream `parse("every monday at ")` may then fail with “empty rule after 'every'” rather than parsing as `every monday`.

**Contract test:** `tests/contract_recurrence.rs::split_time_suffix_trailing_at_without_time_keeps_at_in_rule` pins current behavior.
