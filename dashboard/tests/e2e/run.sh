#!/usr/bin/env bash
# End-to-end verification of Face ID unlock against a REAL sidecar in a real Chromium with a CDP
# virtual platform authenticator (PRF enabled). Not in CI: it needs a browser download. Run it by
# hand after touching the auth gate, face-unlock.js or face-unlock-boot.js.
#
#   dashboard/tests/e2e/run.sh [path-to-node_modules-with-@playwright/test]
#
# Boots its own sidecar on a spare loopback port with a throwaway password and session store, so it
# never touches the live dashboard or ~/.local/state/ptask-dashboard/sessions.json. The DB is opened
# read-only by server.py, as always.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dashboard="$(cd "$here/../.." && pwd)"
modules="${1:-$HOME/ptve/ptve-ui/node_modules}"
port="${PORT:-9611}"
password='verify-face-unlock-local'
workdir="$(mktemp -d)"
trap 'kill "${server_pid:-}" 2>/dev/null || true; rm -rf "$workdir"' EXIT

if [[ ! -d "$modules/@playwright/test" ]]; then
  echo "no @playwright/test under $modules — pass a node_modules path as \$1" >&2
  exit 2
fi
ln -sfn "$modules" "$here/node_modules"

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

# WebAuthn needs a name, not an IP: rpId may not be a bare address, so browse "localhost".
BASE="http://localhost:$port" PTASK_DASH_PASS="$password" node "$here/face-unlock-e2e.mjs"
