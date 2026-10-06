#!/usr/bin/env bash
# ptask-restore-verify.sh — weekly proof that the pTask backups actually
# restore. A backup that has never been restored is a hope, not a backup:
# the Litestream replica was verified exactly once (at activation) and
# never re-drilled until this script existed.
#
# Three independent checks. All three always run (one broken leg must not
# hide another); any failure exits non-zero → OnFailure alert.
#   1. The Litestream replica restores to a scratch path, passes
#      PRAGMA integrity_check, holds the live task count (±COUNT_SLACK, live
#      read just before and just after the restore), and holds every task
#      write older than LAG_MIN minutes. updated_at moves on every create,
#      status change, edit and claim — the UPDATEs a row count never sees —
#      so a replica that froze days ago with the right number of rows and
#      stale statuses fails here. Writes newer than LAG_MIN are excluded, so
#      traffic while the drill runs cannot be blamed on replication.
#   2. The newest nearby nightly is <48h old, is at least the size floor,
#      passes integrity_check, and holds at least NIGHTLY_MIN_PCT% of live's
#      task count (it is up to 48h of task creation behind).
#   3. The newest off-site nightly is <48h old and at least the size floor
#      (stat over ssh — pulling it back over the WAN weekly is unnecessary),
#      and its sha256 equals the same-date nearby copy's (both legs upload
#      the same snapshot; each side is hashed in place over ssh).
#
# `--replica-only` runs check 1 alone and touches no backup host:
# ptask-replica-check.timer runs it daily, so a Litestream that keeps
# running but stops replicating is caught within a day, not a week.
#
# Size floor: max(64 KiB, 50% of live's logical size). A nightly is a
# page-for-page online-backup copy of live from <48h ago, so a healthy one
# lands within a few percent of live's size; half is far outside normal
# growth, while a zero-length, truncated or near-empty file (which still
# passes integrity_check — an empty file is a valid empty database) falls
# well under it. 64 KiB is below an empty pTask schema, so it only rejects
# junk even if live itself were tiny.
#
# Env overrides:
#   PTASK_DB                — live DB (default ~/puretensor-tasks/tasks.db)
#   PTASK_LITESTREAM_CONFIG — (default ~/.config/litestream/litestream.yml)
#   PTASK_BACKUP_REMOTE     — nearby backup target (default backup-host:/var/backups/ptask)
#   PTASK_BACKUP_OFFSITE    — off-site target (default dr-host:dr-backup/ptask)
#   PTASK_VERIFY_STEP_TIMEOUT    — seconds per remote command (default 30)
#   PTASK_VERIFY_XFER_TIMEOUT    — seconds to copy the nearby nightly (default 120)
#   PTASK_VERIFY_RESTORE_TIMEOUT — seconds for `litestream restore` (default 300)
#
# Nightlies are matched as ptask-tasks-*.db, so the .partial name an upload
# carries until ptask-backup.sh renames it is never taken for the newest.
set -euo pipefail

MODE=full
case "${1:-}" in
    "") ;;
    --replica-only) MODE=replica ;;
    *) echo "usage: ptask-restore-verify.sh [--replica-only]" >&2; exit 64 ;;
esac

DB="${PTASK_DB:-$HOME/puretensor-tasks/tasks.db}"
LS_CONFIG="${PTASK_LITESTREAM_CONFIG:-$HOME/.config/litestream/litestream.yml}"
REMOTE="${PTASK_BACKUP_REMOTE:-backup-host:/var/backups/ptask}"
OFFSITE="${PTASK_BACKUP_OFFSITE:-dr-host:dr-backup/ptask}"

COUNT_SLACK=10        # tasks; Litestream trails live by seconds
LAG_MIN=10            # minutes; every task write older than this must be replicated
NIGHTLY_MIN_PCT=90    # nightly task count vs live
FLOOR_MIN_BYTES=65536
FLOOR_PCT=50

# Same options as ptask-backup.sh: keepalives catch a dead peer, and
# coreutils timeout catches a live peer whose command hangs (D-state on a
# hung mount), so one stuck leg fails its own check instead of the unit's
# TimeoutStartSec killing the drill before the other checks run.
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=4)
STEP_TIMEOUT=${PTASK_VERIFY_STEP_TIMEOUT:-30}
XFER_TIMEOUT=${PTASK_VERIFY_XFER_TIMEOUT:-120}
RESTORE_TIMEOUT=${PTASK_VERIFY_RESTORE_TIMEOUT:-300}
rssh() { timeout -k 10 "$STEP_TIMEOUT" ssh "${SSH_OPTS[@]}" "$@"; }
rscp() { timeout -k 10 "$XFER_TIMEOUT" scp -q "${SSH_OPTS[@]}" "$@"; }
# Worst case with the defaults: restore 300+10 s; nearby 2 rssh (40 s each)
# + 1 rscp (130 s) = 210 s; off-site 4 rssh (list, stat, two sha256) =
# 160 s; total 680 s. ptask-restore-verify.service allows
# TimeoutStartSec=15min (900 s), which leaves 220 s for the local sqlite
# checks. Raise it with the timeouts. --replica-only: 310 s, and
# ptask-replica-check.service allows 10min.

SCRATCH=$(mktemp -d -t ptask-restore-verify-XXXXXX)
cleanup() { rm -rf "$SCRATCH"; }
trap cleanup EXIT

say() { echo "ptask-restore-verify: $*"; }
fail() { echo "ptask-restore-verify: FAIL — $*" >&2; }
die() { fail "$@"; exit 1; }

LIVE="file:$DB?mode=ro"

# "count|settled_jd|settled_iso": COUNT(*) and the newest task write at least
# LAG_MIN minutes old (as a Julian day, and readable; 0/none when there is
# none). Scoring does not touch updated_at, so only real mutations move it.
task_stats() {
    sqlite3 "$1" "SELECT COUNT(*), COALESCE(MAX(s), 0),
        COALESCE(strftime('%Y-%m-%dT%H:%M:%SZ', MAX(s)), 'none')
        FROM (SELECT CASE WHEN julianday(updated_at) <= julianday('now', '-$LAG_MIN minutes')
              THEN julianday(updated_at) END AS s FROM tasks);"
}

live_stats=$(task_stats "$LIVE") || die "cannot read live DB $DB"
IFS='|' read -r live_count live_settled live_settled_iso <<<"$live_stats"
live_bytes=$(sqlite3 "$LIVE" \
    "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size();") \
    || die "cannot read live DB size $DB"
floor=$(( live_bytes * FLOOR_PCT / 100 ))
[ "$floor" -ge "$FLOOR_MIN_BYTES" ] || floor=$FLOOR_MIN_BYTES

# The check functions run from `||`, where bash suspends errexit (also inside
# them), so every fallible step handles its own failure.

# ---- 1. Litestream restore drill ----------------------------------------
check_replica() {
    local ic stats count settled settled_iso after live_after live_settled_after lo hi
    local need need_iso
    command -v litestream >/dev/null || { fail "litestream binary not on PATH"; return 1; }
    timeout -k 10 "$RESTORE_TIMEOUT" litestream restore -config "$LS_CONFIG" -o "$SCRATCH/restored.db" "$DB" \
        || { fail "litestream restore failed or ran past ${RESTORE_TIMEOUT}s"; return 1; }
    ic=$(sqlite3 "$SCRATCH/restored.db" "PRAGMA integrity_check;") \
        || { fail "litestream restore is unreadable"; return 1; }
    [ "$ic" = "ok" ] || { fail "litestream restore integrity_check: $ic"; return 1; }
    stats=$(task_stats "$SCRATCH/restored.db") \
        || { fail "litestream restore has no readable tasks table"; return 1; }
    IFS='|' read -r count settled settled_iso <<<"$stats"
    # Bracket the restore with live reads from before and after it: rows
    # created or deleted while the drill runs widen the count band instead
    # of failing it, so the slack only has to cover replication lag.
    after=$(task_stats "$LIVE") || { fail "cannot re-read live DB $DB"; return 1; }
    IFS='|' read -r live_after live_settled_after _ <<<"$after"
    lo=$(( (live_count < live_after ? live_count : live_after) - COUNT_SLACK ))
    hi=$(( (live_count > live_after ? live_count : live_after) + COUNT_SLACK ))
    if [ "$count" -lt "$lo" ] || [ "$count" -gt "$hi" ]; then
        fail "litestream restore holds $count tasks vs live $live_count..$live_after (±$COUNT_SLACK) — replica diverged"
        return 1
    fi
    # The replica must hold the newest settled write live had both before
    # and after the restore: the smaller of the two reads. A hard delete of
    # live's newest settled row while the restore runs (replicated, so gone
    # from the restore too) lowers the second read and must not look like a
    # stall.
    need=$live_settled need_iso=$live_settled_iso
    if awk -v a="$live_settled_after" -v b="$live_settled" 'BEGIN { exit !(a < b) }'; then
        need=$live_settled_after
        need_iso=${after##*|}
    fi
    if awk -v r="$settled" -v l="$need" 'BEGIN { exit !(r < l) }'; then
        fail "litestream restore lacks task writes older than ${LAG_MIN}min: its newest is $settled_iso, live's $need_iso — replication stalled"
        return 1
    fi
    say "litestream ok (restored $count tasks, live $live_count; newest settled write $settled_iso)"
}

# ---- 2. Nearby nightly freshness, size, integrity, content ---------------
check_nightly() {
    local host="${REMOTE%%:*}" dir="${REMOTE#*:}" latest age_h bytes ic count
    latest=$(rssh "$host" \
        "ls -1t '$dir'/ptask-tasks-*.db 2>/dev/null | head -1") \
        || { fail "cannot list nightlies at $REMOTE"; return 1; }
    [ -n "$latest" ] || { fail "no nightly backups found at $REMOTE"; return 1; }
    age_h=$(rssh "$host" \
        "echo \$(( ( \$(date +%s) - \$(stat -c %Y '$latest') ) / 3600 ))") \
        || { fail "cannot stat $latest on $host"; return 1; }
    [ "$age_h" -lt 48 ] || { fail "newest nearby nightly is ${age_h}h old (>48h): $latest"; return 1; }
    rscp "$host:$latest" "$SCRATCH/nightly.db" \
        || { fail "cannot copy $latest from $host"; return 1; }
    bytes=$(stat -c %s "$SCRATCH/nightly.db")
    [ "$bytes" -ge "$floor" ] \
        || { fail "nearby nightly is $bytes bytes, under the $floor-byte floor (live $live_bytes): $latest"; return 1; }
    ic=$(sqlite3 "$SCRATCH/nightly.db" "PRAGMA integrity_check;") \
        || { fail "nearby nightly is unreadable: $latest"; return 1; }
    [ "$ic" = "ok" ] || { fail "nearby nightly integrity_check: $ic"; return 1; }
    count=$(sqlite3 "$SCRATCH/nightly.db" "SELECT COUNT(*) FROM tasks;") \
        || { fail "nearby nightly has no readable tasks table: $latest"; return 1; }
    [ $(( count * 100 )) -ge $(( live_count * NIGHTLY_MIN_PCT )) ] \
        || { fail "nearby nightly holds $count tasks, under ${NIGHTLY_MIN_PCT}% of live's $live_count: $latest"; return 1; }
    say "nearby nightly ok ($(basename "$latest"), ${age_h}h old, $bytes bytes, $count tasks)"
}

# ---- 3. Off-site freshness + size ------------------------------------------
check_offsite() {
    local host="${OFFSITE%%:*}" dir="${OFFSITE#*:}" latest age_size age_h bytes
    latest=$(rssh "$host" \
        "ls -1t '$dir'/ptask-tasks-*.db 2>/dev/null | head -1") \
        || { fail "cannot list off-site backups at $OFFSITE"; return 1; }
    [ -n "$latest" ] || { fail "no off-site backups found at $OFFSITE"; return 1; }
    age_size=$(rssh "$host" \
        "echo \$(( ( \$(date +%s) - \$(stat -c %Y '$latest') ) / 3600 )) \$(stat -c %s '$latest')") \
        || { fail "cannot stat $latest on $host"; return 1; }
    read -r age_h bytes <<<"$age_size"
    [ "$age_h" -lt 48 ] || { fail "newest off-site backup is ${age_h}h old (>48h): $latest"; return 1; }
    [ "$bytes" -ge "$floor" ] \
        || { fail "newest off-site backup is $bytes bytes, under the $floor-byte floor (live $live_bytes): $latest"; return 1; }
    # Same-date digest: both legs upload one snapshot, so the copies must be
    # identical. Each side is hashed where it lives; nothing crosses the WAN.
    local name off_sum near_sum digest_note
    name=$(basename "$latest")
    off_sum=$(rssh "$host" "sha256sum '$latest'") \
        || { fail "cannot hash $latest on $host"; return 1; }
    off_sum=${off_sum%% *}
    if near_sum=$(rssh "${REMOTE%%:*}" \
            "f='${REMOTE#*:}/$name'; if [ -f \"\$f\" ]; then sha256sum \"\$f\"; fi"); then
        near_sum=${near_sum%% *}
        if [ -z "$near_sum" ]; then
            digest_note="no same-date nearby copy to compare"
        elif [ "$near_sum" != "$off_sum" ]; then
            fail "off-site $name (sha256 ${off_sum:0:16}…) differs from the nearby copy (${near_sum:0:16}…)"
            return 1
        else
            digest_note="sha256 matches nearby"
        fi
    else
        digest_note="nearby unreachable, digest not compared"
    fi
    say "offsite ok ($name, ${age_h}h old, $bytes bytes, $digest_note)"
}

failed=()
check_replica || failed+=(litestream)
if [ "$MODE" = replica ]; then
    [ "${#failed[@]}" -eq 0 ] || exit 1
    say "replica OK"
    exit 0
fi
check_nightly || failed+=(nearby)
if [ "$OFFSITE" = "none" ]; then
    say "offsite skipped (PTASK_BACKUP_OFFSITE=none)"
else
    check_offsite || failed+=(offsite)
fi

if [ "${#failed[@]}" -gt 0 ]; then
    fail "${#failed[@]} check(s) failed: ${failed[*]}"
    exit 1
fi
say "ALL OK"
