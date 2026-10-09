#!/usr/bin/env bash
# Boot a throwaway sidecar over a FRESH DB and assert in a real browser that
# closure evidence and notes work end to end: the mark-done dialog's Evidence
# field reaches `pt done --note`, and the drawer shows the trail and adds a
# note through `pt note`. Not in CI: needs a browser.
#
#   dashboard/tests/e2e/notes.sh [path-to-node_modules-with-@playwright/test]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dashboard="$(cd "$here/../.." && pwd)"
modules="${1:-$HOME/ptve/ptve-ui/node_modules}"
port="${PORT:-9614}"
workdir="$(mktemp -d)"
trap 'kill "${server_pid:-}" 2>/dev/null || true; rm -rf "$workdir"' EXIT

if [[ ! -d "$modules/@playwright/test" ]]; then
  echo "no @playwright/test under $modules — pass a node_modules path as \$1" >&2
  exit 2
fi
ln -sfn "$modules" "$here/node_modules"

PT="${PTASK_BIN:-$HOME/.cargo/bin/pt}"
export PTASK_DB="$workdir/tasks.db" PTASK_BIN="$PT"
PTASK_ACTOR=e2e "$PT" add --raw -p critical "Rotate the backup key" >/dev/null   # PT-1
PTASK_ACTOR=e2e "$PT" add --raw -p urgent "Rack fox-n2" >/dev/null              # PT-2
PTASK_ACTOR=hal "$PT" note PT-2 "rails ordered; ETA Thursday" >/dev/null

# The production unit's actor (README: PTASK_ACTOR=dashboard).
PTASK_ACTOR=dashboard PTASK_DASH_BIND="127.0.0.1:$port" \
  python3 "$dashboard/server.py" >"$workdir/server.log" 2>&1 &
server_pid=$!

for _ in $(seq 1 40); do
  curl -sf "http://localhost:$port/healthz" >/dev/null 2>&1 && break
  sleep 0.25
done
curl -sf "http://localhost:$port/healthz" >/dev/null || { cat "$workdir/server.log"; exit 1; }

BASE="http://localhost:$port" \
SHOT_DIR="${SHOT_DIR:-$workdir}" \
  node "$here/notes-e2e.mjs"

# The writes went through pt: the journal carries them, attributed.
"$PT" --json show PT-1 | python3 -c '
import json, sys
notes = json.load(sys.stdin)["notes"]
assert [(n["kind"], n["text"], n["actor"]) for n in notes] == [
    ("done", "restic check: 0 errors", "dashboard")], notes
'
"$PT" --json show PT-2 | python3 -c '
import json, sys
notes = json.load(sys.stdin)["notes"]
assert [(n["text"], n["actor"]) for n in notes] == [
    ("rails ordered; ETA Thursday", "hal"),
    ("racked; IPMI answers on the tailnet", "dashboard")], notes
'
echo "notes e2e: ok"
