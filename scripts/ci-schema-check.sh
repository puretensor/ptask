#!/usr/bin/env bash
# Runs the embedded refinery migrations against a brand-new SQLite file
# (greenfield bootstrap: V008 creates the legacy base schema, V001-V007 the
# pt_* side tables), then asserts every expected table is present. Used by
# .github/workflows/ci.yml.
set -euo pipefail

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

DB="$TMPDIR/test.db"

# Build the CLI in release mode and run backfill against a fresh file —
# no pre-seeded schema; migrations must bootstrap everything.
cargo build --release -p ptask-cli --locked
PTASK_DB="$DB" ./target/release/pt backfill

python3 - "$DB" <<'PY'
import sqlite3, sys
db = sys.argv[1]
c = sqlite3.connect(db)
expected = {
    "pt_counters",
    "pt_views",
    "pt_recurrence",
    "pt_event_log",
    "pt_webhook_log",
    "pt_api_tokens",
    "pt_extensions_legacy",
}
present = {
    row[0]
    for row in c.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'pt_%'"
    )
}
views = {
    row[0]
    for row in c.execute(
        "SELECT name FROM sqlite_master WHERE type='view' AND name LIKE 'pt_%'"
    )
}
assert "pt_extensions" in views, f"pt_extensions compat view missing: {views}"
for t in ("task_links", "task_labels", "tasks_fts"):
    n = c.execute(
        "SELECT COUNT(*) FROM sqlite_master WHERE name=?", (t,)
    ).fetchone()[0]
    assert n == 1, f"missing v2 object {t}"
missing = expected - present
extra = present - expected
if missing or extra:
    print(f"FAIL: missing={missing}, unexpected={extra}", file=sys.stderr)
    sys.exit(1)
base_expected = {
    "tasks", "interactions", "notifications", "raw_items",
    "canonical_tasks", "ingested_files", "daily_budget",
}
base_present = {
    row[0]
    for row in c.execute("SELECT name FROM sqlite_master WHERE type='table'")
}
base_missing = base_expected - base_present
if base_missing:
    print(f"FAIL: V008 base schema missing tables: {base_missing}", file=sys.stderr)
    sys.exit(1)
counter = c.execute("SELECT value FROM pt_counters WHERE name='pt_id'").fetchone()[0]
if counter != 0:
    print(f"FAIL: pt_counter seed was {counter}, expected 0", file=sys.stderr)
    sys.exit(1)
print(f"OK: all pt_* tables present ({len(present)}); base schema bootstrapped; counter at 0.")
PY
