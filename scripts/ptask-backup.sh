#!/usr/bin/env bash
# ptask-backup.sh — nightly hot-backup of the pTask SQLite store.
#
# Runs on the canonical DB host (the workstation that owns
# ~/puretensor-tasks/tasks.db). Produces a consistent snapshot via
# `sqlite3 .backup`, copies it to a nearby replica host over SSH and
# optionally to an off-site DR receiver, then prunes both targets to
# RETAIN_DAYS.
#
# Put the two legs in different failure domains. A replica on the same
# storage cluster as the primary is not a disaster-recovery copy.
#
# Both legs are load-bearing and independent: each runs even when the other
# fails (a dead nearby host must not cost the off-site copy), and failure of
# either exits non-zero so the OnFailure Telegram alert fires.
#
# Env overrides:
#   PTASK_DB               — source DB (default: ~/puretensor-tasks/tasks.db)
#   PTASK_BACKUP_REMOTE    — scp target (default: backup-host:/var/backups/ptask)
#   PTASK_BACKUP_OFFSITE   — off-site scp target
#                            (default: dr-host:dr-backup/ptask)
#                            set to "none" to skip (e.g. non-canonical hosts)
#   PTASK_BACKUP_RETAIN    — retain N days (default: 30)
set -euo pipefail

DB="${PTASK_DB:-$HOME/puretensor-tasks/tasks.db}"
REMOTE="${PTASK_BACKUP_REMOTE:-backup-host:/var/backups/ptask}"
OFFSITE="${PTASK_BACKUP_OFFSITE:-dr-host:dr-backup/ptask}"
RETAIN_DAYS="${PTASK_BACKUP_RETAIN:-30}"

if [ ! -f "$DB" ]; then
    echo "ptask-backup: source DB not found: $DB" >&2
    exit 1
fi

DATE=$(date -u +%Y-%m-%d)
TMP=$(mktemp -t ptask-backup-XXXXXX.db)
cleanup() { rm -f "$TMP" "$TMP-journal" "$TMP-shm" "$TMP-wal"; }
trap cleanup EXIT

python3 - "$DB" "$TMP" <<'PY'
import sqlite3, sys
src = sqlite3.connect(sys.argv[1])
dst = sqlite3.connect(sys.argv[2])
try:
    src.backup(dst)
finally:
    dst.close()
    src.close()
PY
SIZE=$(stat -c%s "$TMP")

# Every ssh/scp gets a connect timeout and keepalives: a hung CephFS mount or
# a dead peer otherwise blocks forever, the oneshot never fails, and
# OnFailure never alerts (the unit also carries TimeoutStartSec).
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=4)

# Upload the snapshot to one target, prune it to RETAIN_DAYS, count what is
# retained. Called from `||`, where bash suspends errexit (also inside the
# function), so every step carries its own `|| return 1`.
upload_leg() {
    local label=$1 target=$2
    local host="${target%%:*}" dir="${target#*:}" retained
    ssh "${SSH_OPTS[@]}" "$host" "mkdir -p '$dir'" || return 1
    scp -q "${SSH_OPTS[@]}" "$TMP" "$target/ptask-tasks-$DATE.db" || return 1
    # Retention prune. `-mtime +N` means strictly older than N days.
    ssh "${SSH_OPTS[@]}" "$host" \
        "find '$dir' -maxdepth 1 -type f -name 'ptask-tasks-*.db' \
         -mtime +$((RETAIN_DAYS - 1)) -delete" || return 1
    retained=$(ssh "${SSH_OPTS[@]}" "$host" \
        "find '$dir' -maxdepth 1 -type f -name 'ptask-tasks-*.db' | wc -l") || return 1
    echo "ptask-backup: $label ok ${target}/ptask-tasks-${DATE}.db (${SIZE} bytes, ${retained} backups retained)"
}

failed=()
upload_leg nearby "$REMOTE" || failed+=("nearby ($REMOTE)")
# ---- Off-site leg (different failure domain) ----------------------------
if [ "$OFFSITE" != "none" ]; then
    upload_leg offsite "$OFFSITE" || failed+=("off-site ($OFFSITE)")
fi

if [ "${#failed[@]}" -gt 0 ]; then
    echo "ptask-backup: FAILED leg(s): ${failed[*]}" >&2
    exit 1
fi
