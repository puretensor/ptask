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
# Both legs are load-bearing: failure of either exits non-zero so the
# OnFailure Telegram alert fires.
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

# ---- Off-site leg (different failure domain) ----------------------------
if [ "$OFFSITE" != "none" ]; then
    offsite_host="${OFFSITE%%:*}"
    offsite_dir="${OFFSITE#*:}"
    ssh -o BatchMode=yes -o ConnectTimeout=15 "$offsite_host" "mkdir -p '$offsite_dir'"
    scp -q "$TMP" "$OFFSITE/ptask-tasks-$DATE.db"
    ssh -o BatchMode=yes "$offsite_host" \
        "find '$offsite_dir' -maxdepth 1 -type f -name 'ptask-tasks-*.db' \
         -mtime +$((RETAIN_DAYS - 1)) -delete"
    OFF_REMAINING=$(ssh -o BatchMode=yes "$offsite_host" \
        "find '$offsite_dir' -maxdepth 1 -type f -name 'ptask-tasks-*.db' | wc -l")
    echo "ptask-backup: offsite ok ${OFFSITE}/ptask-tasks-${DATE}.db (${OFF_REMAINING} retained)"
fi
