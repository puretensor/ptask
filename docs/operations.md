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
