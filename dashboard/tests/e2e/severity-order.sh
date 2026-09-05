#!/usr/bin/env bash
# Boot a throwaway sidecar over a COPY of the task DB and assert in a real
# browser that the board renders severity-ordered. Not in CI: needs a browser.
#
#   dashboard/tests/e2e/severity-order.sh [path-to-node_modules-with-@playwright/test]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dashboard="$(cd "$here/../.." && pwd)"
modules="${1:-$HOME/ptve/ptve-ui/node_modules}"
port="${PORT:-9612}"
password='verify-severity-order-local'
workdir="$(mktemp -d)"
trap 'kill "${server_pid:-}" 2>/dev/null || true; rm -rf "$workdir"' EXIT

if [[ ! -d "$modules/@playwright/test" ]]; then
  echo "no @playwright/test under $modules — pass a node_modules path as \$1" >&2
  exit 2
fi
ln -sfn "$modules" "$here/node_modules"

# Serve a copy: the live DB is opened read-only anyway, but a copy keeps a
# concurrent writer from changing the board mid-assertion.
src="${PTASK_DB:-$HOME/puretensor-tasks/tasks.db}"
cp "$src" "$workdir/tasks.db"

PTASK_DB="$workdir/tasks.db" \
PTASK_DASH_BIND="127.0.0.1:$port" \
PTASK_DASH_PASS="$password" \
PTASK_DASH_SECURE_COOKIE=0 \
PTASK_DASH_SESSION_STORE="$workdir/sessions.json" \
  python3 "$dashboard/server.py" >"$workdir/server.log" 2>&1 &
server_pid=$!

for _ in $(seq 1 40); do
  curl -sf "http://localhost:$port/healthz" >/dev/null 2>&1 && break
  sleep 0.25
done
curl -sf "http://localhost:$port/healthz" >/dev/null || { cat "$workdir/server.log"; exit 1; }

BASE="http://localhost:$port" PTASK_DASH_PASS="$password" \
SHOT="${SHOT:-$workdir/severity-order.png}" \
  node "$here/severity-order-e2e.mjs"
