# pTask Operations

## Backup (v0.1.0)

The canonical SQLite store at `~/puretensor-tasks/tasks.db` is hot-backed
up nightly to Ceph via mon1.

### Mechanism

`scripts/ptask-backup.sh` runs the SQLite online backup against the live
file (safe with WAL — readers and writers continue concurrently), copies
the resulting snapshot to `backup-host:/var/backups/ptask/`, and prunes
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
scp backup-host:/var/backups/ptask/ptask-tasks-$(date -u +%Y-%m-%d).db /tmp/
sqlite3 /tmp/ptask-tasks-*.db 'SELECT COUNT(*) FROM tasks, COUNT(*) FROM pt_extensions'
```

The count should match the live DB row counts.

### Recovery

Restore: copy a snapshot back to `~/puretensor-tasks/tasks.db` (stop Python
services first if running). The pre-v0.1.0 baseline is at
`~/puretensor-tasks/tasks.db.pre-ptask-backup`.

## Distillation (v3.0.0)

`pt distill` runs the native Rust delta pipeline over unprocessed
`raw_items` and records each invocation in `pt_event_log`. It preflights
Gemini before consuming data, sends classify/consolidate calls with
`thinkingBudget=0`, retries transient Gemini failures, deduplicates
candidates, writes tasks through `ptask-core`, and fails closed with a
`distill.failed` event on provider or pipeline errors.

The legacy Python distiller is retired from the CLI and from the timer path.
It remains only in `~/puretensor-tasks-legacy` as historical reference.

### Cutover from `puretensor-tasks-distill.timer`

The legacy system-mode timer was already disabled on the workstation.
The cutover here is installing the user-mode `ptask-distill.timer`.
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

Cadence: hourly with 300s jitter. `pt distill --batch 300` is the production
service command; interactive runs can lower `--batch` for smoke tests.

A run stops starting provider calls after 64 calls or 20 minutes of chunk
work, whichever comes first, and marks what finished as processed. The unit's
30-minute `TimeoutStartSec` therefore never kills a run mid-batch; rows left
over wait for the next hourly run.

Only one run distills at a time. For its whole duration a run holds an
exclusive `flock` on the database's directory (e.g. `~/puretensor-tasks/`),
opened read-only. A concurrent `pt distill` prints `distill skipped`, consumes
nothing, records no event and exits 0. Every user who can open the database
can open its directory, so a `sudo pt distill` and the timer user always
contend on the same object. The kernel drops the lock when the holder exits or
is killed, so a crashed run never blocks the next one.

The database file itself is never opened for locking: closing any descriptor
on it would drop SQLite's own fcntl locks. A directory descriptor is not the
database file, so SQLite's locks are untouched. The old
`<db>.distill.lock` side file is no longer used; a leftover one is harmless
and can be deleted. Two databases in the same directory serialise each other's
runs. On NFS, flock may be emulated with fcntl, so keep the database on a local
filesystem (SQLite needs that anyway). In-memory databases take no lock, and a
`file:` URI (percent-decoded) locks the directory of the file it names.

Symlinks are resolved first, so a run through `other/link.db -> real/x.db`
locks `real/`. The directory is opened with `O_DIRECTORY`, which means every
user who runs distill needs read permission on it. A `0711` or `0733`
database directory makes every run fail with "cannot open the database
directory … make the directory readable by this user". Fix it with, for
example, `chmod g+r` or `o+r` on that directory.

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
pt distill --batch 300
```

### Failure behaviour

Any native provider or pipeline error writes a `distill.failed` event to
`pt_event_log` with the provider name and detailed error chain, then exits
non-zero. systemd records the failure; the operator's existing Telegram
alert pipeline (or any HMAC webhook subscriber) can scrape `pt_event_log`
for `distill.failed` events. A missing `GOOGLE_API_KEY` exits 3 before any
raw item is consumed.

### Poison captures and quarantine (v3.8.0)

The batch is sent to the provider in chunks of 25, not in one call. A chunk
the provider cannot classify is halved until the offending row is alone, so
one unprocessable capture no longer takes its whole batch down — every other
chunk still lands and is marked processed.

Consolidation output is not capped: the model is asked for one task per
distinct commitment, and each task names the input captures it covers
(`sources`). A kept capture is marked processed only when a created or
deduplicated task covers it. Kept captures the model left uncovered are
walked again as a smaller chunk in the same run, so a model that stops early
or merges too eagerly cannot make a commitment disappear. One task covers at
most 8 captures (`MAX_SOURCES_PER_CANDIDATE`); captures beyond that go round
again instead of being consumed on a single over-merged answer. A task that
claims 3 or more captures (an over-merge suspect) covers each one only if that
capture's own text supports the title: at least half of the title's content
words (2+ letters, stopwords removed) appear in it, where a shared prefix of
4+ letters counts as a match. A catch-all title such as "Do everything" over a
batch therefore consumes nothing, and those captures go round again in smaller
batches. Claims of 1-2 captures are trusted, and so is a lone capture's own
answer, whether that answer creates a task or dedups against an existing one,
even when the model numbers its sources from 1. A lexical check would reject
honest paraphrases such as "tell hal to fix the raid" → "Replace failed disk
in storage array". Tasks returned without `sources` cover nothing.
They are counted as `sourceless_candidates` in the `distill.run` payload and
printed by `pt distill`; a non-zero count means the model is ignoring the
schema and burning calls on re-walks. A consolidation
that covers none of the captures classified as commitments uses the same
isolation and retry path as a provider failure. Those captures remain
unprocessed and are quarantined after three failed attempts, so empty
provider output cannot block newer captures indefinitely. Noise in a failed
chunk is reclassified during bisection and counted as consumed only once.

Each capture is capped at 4,000 characters in the prompt (`MAX_ITEM_CHARS`),
with a visible `[… N chars truncated]` marker, so one very long email cannot
exceed the model context and end up quarantined. The stored `raw_items.text`
is never truncated.

The isolated row is charged one `raw_items.distill_attempts`, with the reason
in `raw_items.distill_error`. After 3 charges it is **quarantined**: no longer
served by `fetch_unprocessed`, so it cannot sit at the head of the
oldest-first queue and block newer captures. A *database* failure (as opposed
to a provider/classification failure) is never charged — a local outage must
not push a good capture toward quarantine.

A provider **outage** is not charged either. Once retries are exhausted, a
rate limit (429), overload (503), timeout (408, 504 or client-side), connection
failure, or a 401/403/404 (credentials or model gone) aborts the run
immediately. The class is the worst one seen across the retry attempts, so a
500/503/500 sequence counts as an overload. There is no bisection and no attempt charged, the chunks that
already finished are still marked processed, and the run fails closed with
`distill.failed` ("provider unavailable (…): …").

Other 5xx errors (500/502…) can be specific to one input, for example a server
that returns 500 on context overflow. After each one, distill sends a canary
on the stage that failed: one known-benign capture through classify, or
through consolidate. If the canary fails, it is the provider: the run aborts,
and any provisional charges from earlier in the run are dropped. If the
canary succeeds, the chunk is bisected, however many poison rows it holds. A
lone row is charged provisionally only if it fails again on a second attempt
after the healthy canary, so a transient 500 is never charged.

A provisional charge is applied only if a chunk of real captures succeeded
later in the same run, because a canary alone does not prove the provider
handles real data. Otherwise the charge is deferred: a `distill.deferred`
event is recorded and nothing is charged. Each such run fails closed (nothing
was consumed), so `distill.failed` alerts the operator. A capture deferred in
6 runs (`DEFERRALS_BEFORE_CHARGE`) is charged anyway. A provider that answers
the canary but fails all real data charges nothing for about 6 hours. A queue
holding nothing but poisons still drains: those rows are quarantined after
about 6 + 3 runs. Mixed with healthy captures, poisons are charged straight
away and quarantined after 3 runs.
Retries honour
a `Retry-After` header of up to 30 s; a longer one aborts at once instead of
waiting.

Quarantine is visible, never silent:

- `pt_distill_quarantined_captures` (Prometheus gauge on `/metrics`) — alert
  on `> 0`; nothing clears it automatically. Add the rule; the gauge existing
  is not the same as anyone being told.
- a `distill.quarantined` event per row in `pt_event_log`.
- `pt distill` prints the count and the triage query on **both** the success
  and the failure path. The fail-closed run is the one on which rows actually
  cross the ceiling, so reporting it only on success would hide it exactly
  when it matters.

#### Known exposure: a provider answering with garbage still charges attempts

An attempt is charged whether or not anything else succeeded in the same run.
This is a deliberate simplification, and it has a cost worth stating rather
than discovering: a provider that keeps *answering* but with unusable output
— a bad model deploy, a schema regression in the structured output — charges
every row the bisection reaches, not just genuinely-unprocessable ones.
(Outages and rejected keys no longer do; see above.)

Measured at the current `CHUNK = 25` / `MAX_PROVIDER_CALLS = 64` settings, a
fully-failing run charges roughly **31 captures**. Three consecutive fully
failing runs (about three hours on the hourly timer) can therefore quarantine
~31 good captures that had nothing wrong with them.

That is bounded and fully recoverable — the rows are retained, counted, and
re-armed by resetting `distill_attempts` as below — but it means
**`pt_distill_quarantined_captures` rising sharply is a signal to check the
provider, not the captures.** A slow trickle indicates genuinely poison rows;
a jump of ~30 after a deploy indicates the provider broke and good rows were
charged for it.

The narrower fix (skip charging when zero provider calls succeeded this run)
was considered and not taken, because it re-opens the original wedge in the
case where the isolated row was the only row served. If this exposure ever
bites in practice, the cheaper mitigation is to make `GeminiProvider::preflight`
exercise the same array/structured-output schema that `classify_batch` uses, so
a schema regression fails preflight and consumes nothing.

Inspect and, once the underlying problem is fixed, release them:

```bash
sqlite3 ~/puretensor-tasks/tasks.db \
  "SELECT id, distill_attempts, distill_error, substr(text,1,80)
     FROM raw_items WHERE processed=0 AND distill_attempts>=3;"

# Re-arm a row for the next run (or set processed=1 to drop it):
sqlite3 ~/puretensor-tasks/tasks.db \
  "UPDATE raw_items SET distill_attempts=0, distill_error=NULL WHERE id=<id>;"
```

## Accountability (v0.7.0)

`pt accountability run` is the Rust port of the Python `accountability/engine.py`.
It walks the 6-level escalation state machine, gates on the 22:00 — 08:00 UTC
quiet window, respects a daily Telegram budget of 3, and enforces a 4-hour
cooldown per task between reminders.

Task age is measured from the start of the current occurrence. Completing a
recurring task (which advances it to its next occurrence) or reopening a
done/dismissed task resets its escalation level, level timestamp and reminder
cooldown, so an on-schedule daily task never climbs the ladder and a task
reopened after the level-5 final notice is reminded again from level 1.

Each reminder is recorded before it is sent. Immediately before the send,
one conditional update re-checks the task's current row (still pending, not
snoozed or completed meanwhile, same level, not already reminded by a
concurrent run) and stamps the 4-hour cooldown; the Telegram budget slot is
reserved at the same point. A failed delivery releases both. A database
write that fails after a delivered nudge is logged and reported on that task
but no longer aborts the run, and the nudge is not repeated.

A level whose channels are all unconfigured falls back to the configured
channel: on a Telegram-only install the level-5 final notice goes to Telegram
(budgeted like any Telegram nudge); on an email-only install levels 1-2 go to
email.

Each SMTP send is bounded at 30 s end to end (connect through DATA); a
stalled mail server counts as a failed email send instead of hanging the run.
After the first failed email in a run, email is skipped for the rest of that
run (Telegram still goes out), so a dead server costs one timeout, not one
per task.

The From, To and CC addresses are validated before anything is sent. If one
is invalid, the run prints `email misconfigured`, disables email for that run
(nudges still go out on Telegram and are stamped), then exits non-zero. A
channel error during a send (for example a rejected address) only fails that
channel: the run continues and stamps what was delivered elsewhere.

### Config (env)

| Variable | Purpose |
|---|---|
| `PTASK_TELEGRAM_BOT_TOKEN` *(or `TELEGRAM_BOT_TOKEN`)* | Telegram Bot API token |
| `PTASK_ACCOUNTABILITY_CHAT_ID` *(falls back to `PTASK_TELEGRAM_DIGEST_CHATS[0]`, then `TELEGRAM_CHAT_ID`)* | int64 chat to nudge |
| `PTASK_SMTP_HOST` *(or `SMTP_HOST`)* | SMTP server |
| `PTASK_SMTP_PORT` *(or `SMTP_PORT`)* | default 587 |
| `PTASK_SMTP_USER` / `PTASK_SMTP_PASS` *(or `SMTP_USER` / `SMTP_PASS`)* | STARTTLS creds |
| `PTASK_SMTP_FROM` *(or `SMTP_FROM`)* | From mailbox, e.g. `HAL <hal@puretensor.ai>`; defaults to `HAL <SMTP_USER>`, so set it whenever the SMTP login is not an address |
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

## Litestream WAL replication (v0.9.4 / v1.0.3 — the canonical host canonical)

`tasks.db` is continuously replicated. Target recovery-point objective:
< 1 minute. Litestream owns the SQLite WAL checkpoint cadence — anything
else doing `PRAGMA wal_checkpoint(...)` on the live DB races the
replicator.

**Active replica (v1.0.3): CephFS file** at
`/var/backups/ptask-litestream/tasks.db`. The original v0.9.4 plan
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
2. CephFS mounted at `/var/backups` on the canonical host (already
   in place on the canonical host).
3. `~/.config/litestream/.env` exists (can be empty — required by the
   service's `ConditionPathExists=` gate, but the CephFS replica has no
   env-driven knobs):
   ```bash
   touch ~/.config/litestream/.env
   chmod 600 ~/.config/litestream/.env
   ```
   For the alternate RGW config, populate with:
   ```ini
   PTASK_LITESTREAM_ENDPOINT=https://s3.example
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
sudo mkdir -p /var/backups/ptask-litestream  # CephFS replica root
sudo chown ptask:ptask /var/backups/ptask-litestream
ln -sf ~/ptask/scripts/litestream/litestream.yml ~/.config/litestream/litestream.yml
ln -sf ~/ptask/scripts/systemd/ptask-litestream.service ~/.config/systemd/user/

systemctl --user daemon-reload
systemctl --user enable --now ptask-litestream.service
loginctl enable-linger "$USER"
```

### Rust API server (`ptask-serve.service`)

The canonical host also runs the Rust HTTP server so fleet clients can
hit `/sync`. The unit binds a non-loopback Tailscale address and always mounts
the dashboard, so both `PTASK_API_TOKEN` and `PTASK_DASH_PASS` must be present
in `~/puretensor-tasks/.env` before the service will start:

```bash
grep '^PTASK_API_TOKEN=' ~/puretensor-tasks/.env
grep '^PTASK_DASH_PASS=' ~/puretensor-tasks/.env
# Optional: allow only the Command Center to frame the cockpit. Without this
# exact HTTPS origin, dashboard documents retain X-Frame-Options: DENY.
grep '^PTASK_DASH_FRAME_ANCESTOR=' ~/puretensor-tasks/.env || true
ln -sf ~/ptask/scripts/systemd/ptask-serve.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now ptask-serve.service

# The unit binds $PTASK_SERVE_BIND from ~/puretensor-tasks/.env (the canonical
# host's Tailscale IP in production; loopback only if unset). Probe the address
# it actually binds: loopback does not answer a tailnet-only bind.
BIND=$(sed -n 's/^PTASK_SERVE_BIND=//p' ~/puretensor-tasks/.env); BIND=${BIND:-127.0.0.1:9501}
curl "http://$BIND/healthz"   # → ok
curl -H "Authorization: Bearer $PTASK_API_TOKEN" \
  "http://$BIND/version"       # → {"ptask_core":"<current version>"}
```

Fleet clients reach this over Tailscale at the canonical host's tailnet
address (`PTASK_SERVE_BIND`, e.g. `http://100.x.y.z:9501`);
`/etc/profile.d/ptask.sh` sets `PTASK_SYNC_URL` everywhere. The unit binds
that interface IP directly, keeping the API off the public/LAN NICs.
Application-level auth is now fail-closed for non-loopback binds. Only use
`PTASK_ALLOW_UNAUTHENTICATED=1` for a deliberately isolated test deployment.

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

## CI runner (self-hosted, public repository)

Every GitHub Actions job runs on the self-hosted `tensor-core` runner, the
same host that serves the canonical store, and this repository is public.
For `pull_request` events GitHub runs the workflow files from the pull
request's own merge ref, so a fork can edit `.github/workflows/ci.yml` or add
a workflow of its own that targets `[self-hosted, tensor-core]`.

- `ci.yml` skips every job for pull requests whose head branch is not in this
  repository. That keeps ordinary fork PRs off the runner. It cannot stop a
  fork that rewrites the workflow. Skipped jobs report success, so a
  GitHub-hosted `fork PR (CI not run)` job fails on such PRs to keep them from
  looking tested.
- The control that does stop it is the repository setting **Settings →
  Actions → General → Approval for running fork pull request workflows →
  Require approval for all external contributors**. Keep it set, and read the
  diff for `.github/` changes before approving a run.
- To test a fork's change, push its branch into this repository after reading
  it. CI then runs as a same-repository pull request.
