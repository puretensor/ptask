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
