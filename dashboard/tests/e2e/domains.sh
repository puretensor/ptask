#!/usr/bin/env bash
# Boot a throwaway sidecar over a FRESH DB with a five-hat PTASK_DASH_DOMAINS list
# and assert in a real browser that the switch, chips, composer picker and brand all
# follow the config. Not in CI: needs a browser.
#
#   dashboard/tests/e2e/domains.sh [path-to-node_modules-with-@playwright/test]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dashboard="$(cd "$here/../.." && pwd)"
modules="${1:-$HOME/ptve/ptve-ui/node_modules}"
port="${PORT:-9613}"
password='verify-domains-local'
workdir="$(mktemp -d)"
trap 'kill "${server_pid:-}" 2>/dev/null || true; rm -rf "$workdir"' EXIT

if [[ ! -d "$modules/@playwright/test" ]]; then
  echo "no @playwright/test under $modules — pass a node_modules path as \$1" >&2
  exit 2
fi
ln -sfn "$modules" "$here/node_modules"

PT="${PTASK_BIN:-$HOME/.cargo/bin/pt}"
export PTASK_DB="$workdir/tasks.db"
PTASK_ACTOR=e2e "$PT" add "Bretalon article approval" >/dev/null
PTASK_ACTOR=e2e "$PT" edit PT-1 --label domain:bretalon >/dev/null
PTASK_ACTOR=e2e "$PT" add "Renew the Windsor lease" >/dev/null   # untagged -> default domain

PTASK_DASH_BIND="127.0.0.1:$port" \
PTASK_DASH_PASS="$password" \
PTASK_DASH_SECURE_COOKIE=0 \
PTASK_DASH_SESSION_STORE="$workdir/sessions.json" \
PTASK_DASH_TITLE="ALAN" \
PTASK_DASH_DOMAINS="puretensor:PureTensor:PT,bretalon:Bretalon:BRET,eaglestone:Eaglestone:EAGLE,diloretio:Diloretio:DILO,personal:Personal:ME" \
PTASK_DASH_DEFAULT_DOMAIN="personal" \
  python3 "$dashboard/server.py" >"$workdir/server.log" 2>&1 &
server_pid=$!

for _ in $(seq 1 40); do
  curl -sf "http://localhost:$port/healthz" >/dev/null 2>&1 && break
  sleep 0.25
done
curl -sf "http://localhost:$port/healthz" >/dev/null || { cat "$workdir/server.log"; exit 1; }

BASE="http://localhost:$port" PTASK_DASH_PASS="$password" \
SHOT="${SHOT:-$workdir/domains.png}" \
  node "$here/domains-e2e.mjs"
