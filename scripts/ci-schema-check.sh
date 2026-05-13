#!/usr/bin/env bash
# Runs the embedded refinery migrations against a fresh SQLite file with a
# minimal Python-schema stub for `tasks`, then asserts every pt_* side table
# is present. Used by .github/workflows/ci.yml.
set -euo pipefail

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

DB="$TMPDIR/test.db"

python3 - "$DB" <<'PY'
import sqlite3, sys
db = sys.argv[1]
c = sqlite3.connect(db)
c.executescript("""
CREATE TABLE tasks (
    id         TEXT PRIMARY KEY,
    title      TEXT NOT NULL,
    created_at TEXT NOT NULL
);
""")
c.commit()
PY

# Build the CLI in release mode and run backfill against the empty stub.
cargo build --release -p ptask-cli --locked
PTASK_DB="$DB" ./target/release/pt backfill

python3 - "$DB" <<'PY'
import sqlite3, sys
db = sys.argv[1]
c = sqlite3.connect(db)
expected = {
    "pt_counters",
    "pt_extensions",
    "pt_views",
    "pt_recurrence",
    "pt_event_log",
    "pt_webhook_log",
}
present = {
    row[0]
    for row in c.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'pt_%'"
    )
}
missing = expected - present
extra = present - expected
if missing or extra:
    print(f"FAIL: missing={missing}, unexpected={extra}", file=sys.stderr)
    sys.exit(1)
counter = c.execute("SELECT value FROM pt_counters WHERE name='pt_id'").fetchone()[0]
if counter != 0:
    print(f"FAIL: pt_counter seed was {counter}, expected 0", file=sys.stderr)
    sys.exit(1)
print(f"OK: all pt_* tables present ({len(present)}); counter at 0.")
PY
