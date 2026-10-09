#!/usr/bin/env bash
# Boot a throwaway sidecar over a FRESH DB where two actors opened and closed
# work, and assert in a real browser that the flux range picker lists them
# (largest net first, a positive net flagged) at desktop and phone width.
# Not in CI: needs a browser.
#
#   dashboard/tests/e2e/flux.sh [path-to-node_modules-with-@playwright/test]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dashboard="$(cd "$here/../.." && pwd)"
modules="${1:-$HOME/ptve/ptve-ui/node_modules}"
port="${PORT:-9615}"
workdir="$(mktemp -d)"
trap 'kill "${server_pid:-}" 2>/dev/null || true; rm -rf "$workdir"' EXIT

if [[ ! -d "$modules/@playwright/test" ]]; then
  echo "no @playwright/test under $modules — pass a node_modules path as \$1" >&2
  exit 2
fi
ln -sfn "$modules" "$here/node_modules"

PT="${PTASK_BIN:-$HOME/.cargo/bin/pt}"
export PTASK_DB="$workdir/tasks.db" PTASK_BIN="$PT"
for i in 1 2 3 4 5; do
  PTASK_ACTOR=hal "$PT" add --raw "finding $i from the sweep" >/dev/null
done
PTASK_ACTOR=hal "$PT" done PT-1 >/dev/null
PTASK_ACTOR=shell "$PT" done PT-2 >/dev/null
PTASK_ACTOR=shell "$PT" dismiss PT-3 >/dev/null

PTASK_ACTOR=dashboard PTASK_DASH_BIND="127.0.0.1:$port" \
  python3 "$dashboard/server.py" >"$workdir/server.log" 2>&1 &
server_pid=$!

for _ in $(seq 1 40); do
  curl -sf "http://localhost:$port/healthz" >/dev/null 2>&1 && break
  sleep 0.25
done
curl -sf "http://localhost:$port/healthz" >/dev/null || { cat "$workdir/server.log"; exit 1; }

BASE="http://localhost:$port" \
SHOT="${SHOT:-$workdir/flux}" \
  node "$here/flux-e2e.mjs"
