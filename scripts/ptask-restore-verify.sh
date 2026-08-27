#!/usr/bin/env bash
# ptask-restore-verify.sh — weekly proof that the pTask backups actually
# restore. A backup that has never been restored is a hope, not a backup:
# the Litestream replica was verified exactly once (at activation) and
# never re-drilled until this script existed.
#
# Three checks, all must pass (exit non-zero otherwise → OnFailure alert):
#   1. Litestream replica restores to a scratch path, passes
#      PRAGMA integrity_check, and its task count is sane vs live.
#   2. The newest nearby nightly is <48h old and passes integrity_check.
#   3. The newest off-site nightly is <48h old (existence+age only —
#      pulling it back over the WAN weekly is unnecessary).
#
# Env overrides:
#   PTASK_DB                — live DB (default ~/puretensor-tasks/tasks.db)
#   PTASK_LITESTREAM_CONFIG — (default ~/.config/litestream/litestream.yml)
#   PTASK_BACKUP_REMOTE     — nearby backup target (default backup-host:/var/backups/ptask)
#   PTASK_BACKUP_OFFSITE    — off-site target (default dr-host:dr-backup/ptask)
set -euo pipefail

DB="${PTASK_DB:-$HOME/puretensor-tasks/tasks.db}"
LS_CONFIG="${PTASK_LITESTREAM_CONFIG:-$HOME/.config/litestream/litestream.yml}"
REMOTE="${PTASK_BACKUP_REMOTE:-backup-host:/var/backups/ptask}"
OFFSITE="${PTASK_BACKUP_OFFSITE:-dr-host:dr-backup/ptask}"

SCRATCH=$(mktemp -d -t ptask-restore-verify-XXXXXX)
cleanup() { rm -rf "$SCRATCH"; }
trap cleanup EXIT

fail() { echo "ptask-restore-verify: FAIL — $*" >&2; exit 1; }

live_count=$(sqlite3 "file:$DB?mode=ro" "SELECT COUNT(*) FROM tasks;") \
    || fail "cannot read live DB $DB"

# ---- 1. Litestream restore drill ----------------------------------------
command -v litestream >/dev/null || fail "litestream binary not on PATH"
litestream restore -config "$LS_CONFIG" -o "$SCRATCH/restored.db" "$DB" \
    || fail "litestream restore returned non-zero"

ic=$(sqlite3 "$SCRATCH/restored.db" "PRAGMA integrity_check;")
[ "$ic" = "ok" ] || fail "litestream restore integrity_check: $ic"

restored_count=$(sqlite3 "$SCRATCH/restored.db" "SELECT COUNT(*) FROM tasks;")
# The replica trails live by ≤1min of writes; a large deficit means the
# replication path is silently broken.
if [ "$restored_count" -lt $((live_count - 25)) ]; then
    fail "litestream restore row count $restored_count vs live $live_count — replica lagging or broken"
fi
echo "ptask-restore-verify: litestream ok (restored $restored_count tasks, live $live_count)"

# ---- 2. mon1 nightly freshness + integrity ------------------------------
remote_host="${REMOTE%%:*}"
remote_dir="${REMOTE#*:}"
latest=$(ssh -o BatchMode=yes "$remote_host" \
    "ls -1t '$remote_dir'/ptask-tasks-*.db 2>/dev/null | head -1")
[ -n "$latest" ] || fail "no nightly backups found at $REMOTE"

age_h=$(ssh -o BatchMode=yes "$remote_host" \
    "echo \$(( ( \$(date +%s) - \$(stat -c %Y '$latest') ) / 3600 ))")
[ "$age_h" -lt 48 ] || fail "newest mon1 nightly is ${age_h}h old (>48h): $latest"

scp -q "$remote_host:$latest" "$SCRATCH/nightly.db"
ic2=$(sqlite3 "$SCRATCH/nightly.db" "PRAGMA integrity_check;")
[ "$ic2" = "ok" ] || fail "mon1 nightly integrity_check: $ic2"
echo "ptask-restore-verify: mon1 nightly ok ($(basename "$latest"), ${age_h}h old)"

# ---- 3. Off-site freshness ------------------------------------------------
offsite_host="${OFFSITE%%:*}"
offsite_dir="${OFFSITE#*:}"
off_latest=$(ssh -o BatchMode=yes -o ConnectTimeout=15 "$offsite_host" \
    "ls -1t '$offsite_dir'/ptask-tasks-*.db 2>/dev/null | head -1")
[ -n "$off_latest" ] || fail "no off-site backups found at $OFFSITE"

off_age_h=$(ssh -o BatchMode=yes "$offsite_host" \
    "echo \$(( ( \$(date +%s) - \$(stat -c %Y '$off_latest') ) / 3600 ))")
[ "$off_age_h" -lt 48 ] || fail "newest off-site backup is ${off_age_h}h old (>48h): $off_latest"
echo "ptask-restore-verify: offsite ok ($(basename "$off_latest"), ${off_age_h}h old)"

echo "ptask-restore-verify: ALL OK"
