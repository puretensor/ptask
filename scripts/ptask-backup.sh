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
#   PTASK_BACKUP_STEP_TIMEOUT — seconds per remote command (default: 60)
#   PTASK_BACKUP_XFER_TIMEOUT — seconds per upload (default: 600)
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

# Every ssh/scp gets a connect timeout and keepalives, which catch a dead
# peer. They cannot catch a live peer whose command hangs: a remote mkdir or
# find stuck in D-state on a hung CephFS mount keeps answering keepalives.
# So every remote step also runs under coreutils timeout; without it the
# unit's TimeoutStartSec kills the whole run inside the nearby leg and the
# off-site leg never starts.
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=4)
STEP_TIMEOUT=${PTASK_BACKUP_STEP_TIMEOUT:-60}
XFER_TIMEOUT=${PTASK_BACKUP_XFER_TIMEOUT:-600}
rssh() { timeout -k 10 "$STEP_TIMEOUT" ssh "${SSH_OPTS[@]}" "$@"; }
rscp() { timeout -k 10 "$XFER_TIMEOUT" scp -q "${SSH_OPTS[@]}" "$@"; }
# Worst case with the defaults: a leg is 4 rssh steps (mkdir, mv, prune,
# count) of at most 60+10 s plus one rscp of at most 600+10 s = 890 s, and
# two legs = 1780 s. ptask-backup.service allows TimeoutStartSec=40min
# (2400 s), which leaves 620 s for the local snapshot. If you raise either
# timeout, keep TimeoutStartSec above 2 * (4 * (STEP+10) + XFER+10) plus
# the snapshot time.

# Upload the snapshot to one target under a .partial name and rename it into
# place, so a cut-off transfer never leaves a truncated file under the
# "newest nightly" name. Then prune to RETAIN_DAYS (also dropping .partial
# files a killed run left behind) and count what is retained. Called from
# `||`, where bash suspends errexit (also inside the function), so every
# step carries its own `|| return 1`.
upload_leg() {
    local label=$1 target=$2
    local host="${target%%:*}" dir="${target#*:}" retained
    local final="$dir/ptask-tasks-$DATE.db"
    rssh "$host" "mkdir -p '$dir'" || return 1
    rscp "$TMP" "$host:$final.partial" || return 1
    rssh "$host" "mv -f '$final.partial' '$final'" || return 1
    # Retention prune. `-mtime +N` means strictly older than N days.
    rssh "$host" \
        "find '$dir' -maxdepth 1 -type f \
         \\( \\( -name 'ptask-tasks-*.db' -mtime +$((RETAIN_DAYS - 1)) \\) \
         -o \\( -name 'ptask-tasks-*.db.partial' -mmin +720 \\) \\) -delete" || return 1
    retained=$(rssh "$host" \
        "find '$dir' -maxdepth 1 -type f -name 'ptask-tasks-*.db' | wc -l") || return 1
    echo "ptask-backup: $label ok $host:$final (${SIZE} bytes, ${retained} backups retained)"
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
