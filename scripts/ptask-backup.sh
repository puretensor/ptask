#!/usr/bin/env bash
# ptask-backup.sh — nightly hot-backup of the pTask SQLite store.
#
# Runs on the canonical DB host (the workstation that owns
# ~/puretensor-tasks/tasks.db). Produces a consistent snapshot via
# `sqlite3 .backup`, copies it to a Ceph-mounted directory on mon1 over
# SSH, then prunes backups older than RETAIN_DAYS.
#
# Env overrides:
#   PTASK_DB              — source DB (default: ~/puretensor-tasks/tasks.db)
#   PTASK_BACKUP_REMOTE   — scp target (default: mon1:/mnt/cephfs/ptask-backups)
#   PTASK_BACKUP_RETAIN   — retain N days (default: 30)
set -euo pipefail

DB="${PTASK_DB:-$HOME/puretensor-tasks/tasks.db}"
REMOTE="${PTASK_BACKUP_REMOTE:-mon1:/mnt/cephfs/ptask-backups}"
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

remote_host="${REMOTE%%:*}"
remote_dir="${REMOTE#*:}"

ssh -o BatchMode=yes "$remote_host" "mkdir -p '$remote_dir'"
scp -q "$TMP" "$REMOTE/ptask-tasks-$DATE.db"

# Retention prune. `-mtime +N` means strictly older than N days.
ssh -o BatchMode=yes "$remote_host" \
    "find '$remote_dir' -maxdepth 1 -type f -name 'ptask-tasks-*.db' \
     -mtime +$((RETAIN_DAYS - 1)) -delete"

REMAINING=$(ssh -o BatchMode=yes "$remote_host" \
    "find '$remote_dir' -maxdepth 1 -type f -name 'ptask-tasks-*.db' | wc -l")

echo "ptask-backup: ok ${REMOTE}/ptask-tasks-${DATE}.db (${SIZE} bytes, ${REMAINING} backups retained)"
