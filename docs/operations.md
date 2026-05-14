# pTask Operations

## Backup (v0.1.0)

The canonical SQLite store at `~/puretensor-tasks/tasks.db` is hot-backed
up nightly to Ceph via mon1.

### Mechanism

`scripts/ptask-backup.sh` runs the SQLite online backup against the live
file (safe with WAL — readers and writers continue concurrently), copies
the resulting snapshot to `mon1:/mnt/cephfs/ptask-backups/`, and prunes
files older than 30 days.

### Deployment (workstation that owns `tasks.db`)

Symlink-install the user-mode systemd units:

```bash
mkdir -p ~/.config/systemd/user
ln -sf ~/ptask/scripts/systemd/ptask-backup.service ~/.config/systemd/user/
ln -sf ~/ptask/scripts/systemd/ptask-backup.timer   ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now ptask-backup.timer
# Enable lingering so the user timer runs even when the operator is logged out.
loginctl enable-linger "$USER"
```

Check status:

```bash
systemctl --user list-timers ptask-backup.timer
systemctl --user status ptask-backup.service
journalctl --user -u ptask-backup.service -n 100
```

Force a manual run:

```bash
systemctl --user start ptask-backup.service
```

### Retention

- Daily snapshots, named `ptask-tasks-YYYY-MM-DD.db`.
- 30-day retention, pruned by `find -mtime +29 -delete` after each successful upload.
- Override via `PTASK_BACKUP_RETAIN=N` (env var, picked up by the script).

### Verifying a backup

```bash
scp mon1:/mnt/cephfs/ptask-backups/ptask-tasks-$(date -u +%Y-%m-%d).db /tmp/
sqlite3 /tmp/ptask-tasks-*.db 'SELECT COUNT(*) FROM tasks, COUNT(*) FROM pt_extensions'
```

The count should match the live DB row counts.

### Recovery

Restore: copy a snapshot back to `~/puretensor-tasks/tasks.db` (stop Python
services first if running). The pre-v0.1.0 baseline is at
`~/puretensor-tasks/tasks.db.pre-ptask-backup`.

## Distillation (v0.6.5)

`pt distill` runs the existing Python `ingest.distill` pipeline as a
subprocess and records each invocation in `pt_event_log`. The Python ML
is unchanged; Rust owns the timer + audit-log surface and will swap the
subprocess for a native pipeline at v0.9.0.

### Cutover from `puretensor-tasks-distill.timer`

The legacy system-mode timer was already disabled on the workstation —
the cutover here is just installing the user-mode `ptask-distill.timer`.
For nodes still running the legacy unit, disable it first:

```bash
sudo systemctl disable --now puretensor-tasks-distill.timer
```

Then install the new timer:

```bash
mkdir -p ~/.config/systemd/user
ln -sf ~/ptask/scripts/systemd/ptask-distill.service ~/.config/systemd/user/
ln -sf ~/ptask/scripts/systemd/ptask-distill.timer   ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now ptask-distill.timer
loginctl enable-linger "$USER"
```

Cadence: `*-*-* 00,06,12,18:00:00 UTC` with 300s jitter (matches the
legacy unit). `pt distill --days 60` matches the legacy invocation.

### Inspect

```bash
systemctl --user list-timers ptask-distill.timer
journalctl --user -u ptask-distill.service -n 200
sqlite3 ~/puretensor-tasks/tasks.db \
  "SELECT id, event_type, ts FROM pt_event_log
   WHERE event_type LIKE 'distill.%' ORDER BY id DESC LIMIT 10;"
```

### Force a run

```bash
systemctl --user start ptask-distill.service
# Or from a shell:
pt distill --days 60
```

### Failure behaviour

Non-zero Python exit → `pt distill` writes a `distill.failed` event to
`pt_event_log` with the captured stderr tail, then exits with the same
code. systemd records the failure; the operator's existing Telegram
alert pipeline (or any HMAC webhook subscriber) can scrape
`pt_event_log` for `distill.failed` events.

## Accountability (v0.7.0)

`pt accountability run` is the Rust port of the Python `accountability/engine.py`.
It walks the 6-level escalation state machine, gates on the 22:00 — 08:00 UTC
quiet window, respects a daily Telegram budget of 3, and enforces a 4-hour
cooldown per task between reminders.

### Config (env)

| Variable | Purpose |
|---|---|
| `PTASK_TELEGRAM_BOT_TOKEN` *(or `TELEGRAM_BOT_TOKEN`)* | Telegram Bot API token |
| `PTASK_ACCOUNTABILITY_CHAT_ID` *(falls back to `PTASK_TELEGRAM_DIGEST_CHATS[0]`, then `TELEGRAM_CHAT_ID`)* | int64 chat to nudge |
| `PTASK_SMTP_HOST` *(or `SMTP_HOST`)* | SMTP server |
| `PTASK_SMTP_PORT` *(or `SMTP_PORT`)* | default 587 |
| `PTASK_SMTP_USER` / `PTASK_SMTP_PASS` *(or `SMTP_USER` / `SMTP_PASS`)* | STARTTLS creds |
| `PTASK_NOTIFY_EMAIL` *(or `NOTIFY_EMAIL`)* | escalation recipient |
| `PTASK_NOTIFY_CC` *(or `PTASK_OPS_EMAIL`)* | always CC'd (defaults to `ops@puretensor.ai` per CLAUDE.md) |
| `PTASK_HAL_NUDGE_URL` | optional HAL endpoint that POSTs back `{message: "..."}`; falls back to static templates if unset |
| `PTASK_ACCOUNTABILITY_DRY_RUN` | `1` / `true` to log without sending |

### Cutover from `puretensor-tasks-accountability.timer`

```bash
sudo systemctl disable --now puretensor-tasks-accountability.timer
mkdir -p ~/.config/systemd/user
ln -sf ~/ptask/scripts/systemd/ptask-accountability.service ~/.config/systemd/user/
ln -sf ~/ptask/scripts/systemd/ptask-accountability.timer   ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now ptask-accountability.timer
loginctl enable-linger "$USER"
```

### Inspect

```bash
systemctl --user list-timers ptask-accountability.timer
journalctl --user -u ptask-accountability.service -n 200
pt accountability run --dry-run
```

## Scoring (v0.7.3)

`pt scoring run` is the Rust port of `~/puretensor-tasks/api/scoring.py`. It
recomputes the composite priority score (and the four `score_*` columns) for
every task with `status NOT IN ('done', 'dismissed')`. Pure-local: no
network, no LLM call. Reads `tasks` + `interactions`, writes
`priority_score`, `score_urgency`, `score_dependency`, `score_neglect`.

```text
composite = 0.30·urgency + 0.20·dependency + 0.20·neglect + 0.30·manual
```

### Cutover from `puretensor-tasks-scoring.timer`

The legacy system-mode timer at `/etc/systemd/system/puretensor-tasks-scoring.timer`
fires hourly. Disable it before enabling the Rust one:

```bash
sudo systemctl disable --now puretensor-tasks-scoring.timer
mkdir -p ~/.config/systemd/user
ln -sf ~/ptask/scripts/systemd/ptask-scoring.service ~/.config/systemd/user/
ln -sf ~/ptask/scripts/systemd/ptask-scoring.timer   ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now ptask-scoring.timer
loginctl enable-linger "$USER"
```

Cadence: `OnCalendar=hourly` with 60s jitter (matches the legacy unit).

### Inspect

```bash
systemctl --user list-timers ptask-scoring.timer
journalctl --user -u ptask-scoring.service -n 200
pt scoring run --dry-run
```

### Rollback

```bash
systemctl --user disable --now ptask-scoring.timer
sudo systemctl enable --now puretensor-tasks-scoring.timer
```

## Litestream WAL replication (v0.9.4 / v1.0.3 — tensor-core canonical)

`tasks.db` is continuously replicated. Target recovery-point objective:
< 1 minute. Litestream owns the SQLite WAL checkpoint cadence — anything
else doing `PRAGMA wal_checkpoint(...)` on the live DB races the
replicator.

**Active replica (v1.0.3): CephFS file** at
`/mnt/ceph-backup/ptask-litestream/tasks.db`. The original v0.9.4 plan
was an S3 rados-gateway replica, but no RGW endpoint was live at
activation. The config still documents the RGW path as the alternate
config — see `scripts/litestream/litestream.yml`.

### Pre-requisites

1. Litestream binary at `/usr/local/bin/litestream`. Operator installed
   `v0.3.13` via the upstream `.deb`:
   ```bash
   wget https://github.com/benbjohnson/litestream/releases/download/v0.3.13/litestream-v0.3.13-linux-amd64.deb
   sudo dpkg -i litestream-v0.3.13-linux-amd64.deb
   # /usr/bin/litestream → symlink /usr/local/bin/litestream if needed
   sudo ln -sf /usr/bin/litestream /usr/local/bin/litestream
   ```
2. CephFS mounted at `/mnt/ceph-backup` on the canonical host (already
   in place on tensor-core).
3. `~/.config/litestream/.env` exists (can be empty — required by the
   service's `ConditionPathExists=` gate, but the CephFS replica has no
   env-driven knobs):
   ```bash
   touch ~/.config/litestream/.env
   chmod 600 ~/.config/litestream/.env
   ```
   For the alternate RGW config, populate with:
   ```ini
   PTASK_LITESTREAM_ENDPOINT=https://ceph-rgw.ts.puretensor.local
   PTASK_LITESTREAM_BUCKET=ptask-wal
   LITESTREAM_ACCESS_KEY_ID=...
   LITESTREAM_SECRET_ACCESS_KEY=...
   ```

### One-time SQLite tunings

```bash
sqlite3 ~/puretensor-tasks/tasks.db <<'SQL'
PRAGMA journal_mode = WAL;
PRAGMA wal_autocheckpoint = 0;   -- Litestream owns checkpoints
PRAGMA synchronous = NORMAL;
SQL
```

### Install

```bash
mkdir -p ~/.config/litestream ~/.config/systemd/user
sudo mkdir -p /mnt/ceph-backup/ptask-litestream  # CephFS replica root
sudo chown puretensorai:puretensorai /mnt/ceph-backup/ptask-litestream
ln -sf ~/ptask/scripts/litestream/litestream.yml ~/.config/litestream/litestream.yml
ln -sf ~/ptask/scripts/systemd/ptask-litestream.service ~/.config/systemd/user/

systemctl --user daemon-reload
systemctl --user enable --now ptask-litestream.service
loginctl enable-linger "$USER"
```

### Rust API server (`ptask-serve.service`)

The canonical host also runs the Rust HTTP server so fleet clients can
hit `/sync`. v1.0.3 ships the unit file in the repo:

```bash
ln -sf ~/ptask/scripts/systemd/ptask-serve.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now ptask-serve.service

curl http://127.0.0.1:9501/healthz   # → ok
curl http://127.0.0.1:9501/version   # → {"ptask_core":"1.0.3"}
```

Fleet clients reach this via Tailscale at `http://100.121.42.54:9501`;
`/etc/profile.d/ptask.sh` sets `PTASK_SYNC_URL` everywhere.

### Inspect

```bash
systemctl --user status ptask-litestream.service
journalctl --user -u ptask-litestream.service -n 200 --follow
litestream snapshots -config ~/.config/litestream/litestream.yml ~/puretensor-tasks/tasks.db
litestream wal -config ~/.config/litestream/litestream.yml ~/puretensor-tasks/tasks.db
```

### Recovery

Point-in-time restore to a different file (does not touch live DB):

```bash
litestream restore -config ~/.config/litestream/litestream.yml \
    -o /tmp/tasks-restored.db \
    -timestamp $(date -u -d '5 minutes ago' '+%FT%TZ') \
    ~/puretensor-tasks/tasks.db
sqlite3 /tmp/tasks-restored.db 'SELECT count(*) FROM tasks'
```

Promote a restore over the live DB (requires stopping `pt distill`,
`ptask-backup`, etc. first):

```bash
systemctl --user stop ptask-backup.timer ptask-distill.timer \
    ptask-accountability.timer ptask-scoring.timer ptask-litestream.service
cp /tmp/tasks-restored.db ~/puretensor-tasks/tasks.db
systemctl --user start ptask-litestream.service
systemctl --user start ptask-backup.timer ptask-distill.timer \
    ptask-accountability.timer ptask-scoring.timer
```

### Rollback

```bash
systemctl --user disable --now ptask-litestream.service
sqlite3 ~/puretensor-tasks/tasks.db 'PRAGMA wal_autocheckpoint = 1000;'
```

Nightly Ceph snapshot via `ptask-backup.timer` keeps a 30-day file
backup independent of Litestream — it is the recovery path of last
resort if Litestream itself misbehaves.
