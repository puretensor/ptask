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
F=ptask-tasks-$(date -u +%Y-%m-%d).db
scp backup-host:/var/backups/ptask/$F /tmp/
sqlite3 /tmp/$F 'PRAGMA integrity_check; SELECT COUNT(*) FROM tasks;'
sqlite3 "file:$HOME/puretensor-tasks/tasks.db?mode=ro" 'SELECT COUNT(*) FROM tasks;'
```

`integrity_check` must print `ok`, and the snapshot's task count should be at
or a little below the live count (the snapshot is up to a day old).
`ptask-restore-verify.timer` runs this check weekly, together with a
Litestream restore and the off-site copy (`scripts/ptask-restore-verify.sh`,
which also compares the off-site copy's sha256 with the same-date nearby
one). `ptask-replica-check.timer` runs the Litestream part alone daily
(`ptask-restore-verify.sh --replica-only`): it restores the replica to a
scratch file and fails, and alerts, when any task write older than 10
minutes is missing from it, so a Litestream that runs but has stopped
replicating is caught within a day.

### Recovery

Never `cp` a snapshot over the live `tasks.db`. Copy it to a scratch path,
verify it as above, then put it live with
[Promote a restored copy over the live DB](#promote-a-restored-copy-over-the-live-db).
The pre-v0.1.0 baseline is at `~/puretensor-tasks/tasks.db.pre-ptask-backup`.

### Failure alerts

Every pTask oneshot (backup, restore drill, replica check, distill,
accountability, scoring, reaper, export) and `ptask-litestream.service` carry
`OnFailure=ptask-failure-alert@%n.service`, which sends one Telegram message
naming the failed unit and host. The alert unit reads
`~/puretensor-tasks/.env` and then `~/.config/ptask/alert.env`; both are
optional and the second wins. Keep the alert credentials in `alert.env` so
alerts still go out when `.env` itself is missing or broken (one of the
failures they report):

```bash
install -d -m 0700 ~/.config/ptask
install -m 0600 /dev/null ~/.config/ptask/alert.env
cat >> ~/.config/ptask/alert.env <<'EOF'
PTASK_TELEGRAM_BOT_TOKEN=...
PTASK_ACCOUNTABILITY_CHAT_ID=...
EOF
```

With no token in either file the helper exits 64 and logs "not configured"
at err priority; a failed send logs "delivery failed" at err priority. Both
show in `journalctl --user -p err`, the backstop when Telegram is down.

### Rolling back a release

Every `pt` process applies its embedded migrations on open, so the first
command run by a new binary moves the schema forward. An older binary then
refuses that DB ("database schema is at V20__..., newer than this pt binary
knows"): it cannot honour constraints, triggers or columns it has never
heard of, so running it anyway could write rows the newer schema considers
invalid. Migrations are forward-only; there is no down-migration.

To roll back a release that added a migration:

1. Before deploying, check whether it adds one
   (`git diff --stat <old-tag>..<new-tag> -- crates/ptask-core/migrations`)
   and, if so, take a fresh snapshot first
   (`systemctl --user start ptask-backup.service`).
2. To roll back, stop every `pt` writer (the timers and services listed
   under Litestream *Recovery* below, plus `ptask-serve` and
   `ptask-dashboard`), restore the pre-upgrade snapshot (or a Litestream
   point-in-time restore from just before the deploy) over `tasks.db`,
   install the old binary, and start the services again.
3. Writes made after the upgrade are in the newer DB only; re-enter them,
   or keep the newer binary and fix forward instead.

A release whose migrations only add tables or nullable columns can also be
rolled back by fixing forward (a patch release on the newer schema); prefer
that over a restore when the data written since the deploy matters.

## Distillation (v3.0.0)

`pt distill` runs the native Rust delta pipeline over unprocessed
`raw_items` and records each invocation in `pt_event_log`. It preflights
the configured LLM provider before consuming data, retries transient
provider failures, deduplicates candidates, writes tasks through
`ptask-core`, and fails closed with a `distill.failed` event on provider or
pipeline errors.

### Provider (env)

| Variable | Default | Purpose |
|---|---|---|
| `PTASK_LLM_BACKEND` | `local` | `local` (an OpenAI-compatible endpoint) or `gemini`. Any other value exits 1 before a provider call. |
| `LOCAL_LLM_URL` | `http://127.0.0.1:8600/v1` | `local` backend base URL; `/chat/completions` is appended. |
| `LOCAL_LLM_MODEL` | `nemotron-lightning` | `local` backend model id. |
| `GOOGLE_API_KEY` | — | `gemini` backend only, and required there. |
| `GEMINI_CONSOLIDATE_MODEL` | `gemini-3.5-flash` | `gemini` backend model; calls use structured output with `thinkingBudget=0`. |

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
nothing, records a `distill.skipped` event (holder unknown: flock does not say
who holds it) and exits 0. Anyone who can read the directory can hold that
lock, including another database's distill in the same directory, so the
third consecutive skipped run, counted by event history, exits non-zero and
fires the unit's OnFailure alert. A completed run resets the count. Every user
who can open the database
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
for `distill.failed` events. With `PTASK_LLM_BACKEND=gemini`, a missing
`GOOGLE_API_KEY` exits 3 before any raw item is consumed (no event: nothing
was attempted). The default `local` backend needs no key; an unreachable or
misbehaving endpoint fails the preflight, which records `distill.failed` and
exits 1 with nothing consumed.

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
in storage array".

A lone capture whose answer dedups against an existing task is audited:
- a match against a task created earlier in the same run always stands;
- a match where the capture's text supports either title stands silently;
- otherwise the match still stands, but a `distill.lone_unsupported_dedup`
  event records the capture and the matched task for review;
- the exception is an unsupported match against a done or dismissed task: the
  capture is left unconsumed and uncharged rather than silently filed under
  closed work. Each block is recorded as a `distill.lone_unsupported_dedup`
  event with `"closed": true`, and the run's `distill.failed` text names the
  raw_item. After 3 blocks, the candidate is created as a new task beside the
  closed one. This fails toward a duplicate, as dedup does when in doubt, and
  stops the row from failing every run.

Tasks returned without `sources` cover nothing.
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
6 runs (`DEFERRALS_BEFORE_CHARGE`) is charged anyway. Only deferrals since the
most recent successful `distill.run` count, so every new incident gets the
full grace, whatever happened in earlier ones. A provider that answers
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
It walks the 6-level escalation state machine, gates on the 22:00 — 08:00
Europe/London quiet window (the operator's wall clock, so it follows BST),
respects a daily Telegram budget of 3, and enforces a 4-hour
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

### Expired claims (v3.44.0)

An agent that claims with a lease and dies stops heartbeating; its task stays
`in_progress` with an expired lease (`pt_claims_expired` on `/metrics`,
`expired_claims` in the digest, `pt reclaim` lists them). Returning them to
todo changes state, so it is manual by default: `pt reclaim --apply`. To let
the hourly scoring run do it, add `PTASK_CLAIM_RECLAIM=1` to
`~/puretensor-tasks/.env` (the `ptask-scoring.service` EnvironmentFile); each
pass then reclaims before it scores and prints how many it returned.
Claims without a lease are never reclaimed. V021 adds the claim columns and a
trigger: rolling back past 3.44.0 needs the pre-upgrade backup (see
"Rolling back a release").

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

Every process that writes the DB must open it with
`PTASK_WAL_AUTOCHECKPOINT=0` (a per-connection pragma, not stored in the
file): the `scripts/systemd` units get it from `~/puretensor-tasks/.env`, and
`dashboard/ptask-dashboard.service` sets it for the `pt` writers the sidecar
spawns.

### One-time SQLite tunings

```bash
sqlite3 ~/puretensor-tasks/tasks.db 'PRAGMA journal_mode = WAL;'   # persists in the file
# Leave checkpointing to Litestream for every .env-loading pt process. Adds
# the line only when .env does not set the key (an existing value is kept);
# the ansible playbook does the same on the canonical host.
grep -q '^PTASK_WAL_AUTOCHECKPOINT=' ~/puretensor-tasks/.env \
  || echo 'PTASK_WAL_AUTOCHECKPOINT=0' >> ~/puretensor-tasks/.env
```

Only `journal_mode` is stored in the database. `wal_autocheckpoint` and
`synchronous` are per-connection: running them in a `sqlite3` shell changes
that one shell. `pt` sets `synchronous=NORMAL` itself and applies
`PTASK_WAL_AUTOCHECKPOINT` to every connection it opens; `ptask-serve` and
the `pt` timer units (distill, accountability, scoring, reaper, export) load
it from `~/puretensor-tasks/.env`.

Litestream does most of the checkpointing, not all of it. Processes that do
not load `.env` (an interactive `pt`, the dashboard sidecar's `pt` calls, a
`sqlite3` shell) keep SQLite's default and run a PASSIVE checkpoint once the
WAL passes 1000 pages. Litestream tolerates that: a PASSIVE checkpoint never
blocks or truncates under its read lock, and Litestream ships the frames
before it restarts the WAL.

### Install

```bash
mkdir -p ~/.config/litestream ~/.config/systemd/user
# CephFS replica root, owner-only: the replica is the whole DB (raw captures,
# approval payloads, token hashes). Existing install: sudo chmod -R go-rwx it.
sudo install -d -m 0700 -o ptask -g ptask /var/backups/ptask-litestream
ln -sf ~/ptask/scripts/litestream/litestream.yml ~/.config/litestream/litestream.yml
ln -sf ~/ptask/scripts/systemd/ptask-litestream.service ~/.config/systemd/user/
ln -sf ~/ptask/scripts/systemd/ptask-replica-check.service ~/.config/systemd/user/
ln -sf ~/ptask/scripts/systemd/ptask-replica-check.timer   ~/.config/systemd/user/

systemctl --user daemon-reload
systemctl --user enable --now ptask-litestream.service ptask-replica-check.timer
loginctl enable-linger "$USER"
```

### Rust API server (`ptask-serve.service`)

The canonical host also runs the Rust HTTP server so fleet clients can
hit `/sync`. The unit binds a non-loopback Tailscale address and always mounts
the dashboard, so it refuses to start without both machine-API auth and a
dashboard password: `PTASK_DASH_PASS` in `~/puretensor-tasks/.env`, and either
an active named token (`pt token create`, checked with `pt token list`) or the
legacy `PTASK_API_TOKEN` in the same file:

```bash
grep '^PTASK_DASH_PASS=' ~/puretensor-tasks/.env
pt token list                                  # at least one active token, or:
grep '^PTASK_API_TOKEN=' ~/puretensor-tasks/.env
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
curl "http://$BIND/version"   # → {"ptask_core":"<current version>"} (no token needed)
```

Fleet clients reach this over Tailscale at the canonical host's tailnet
address (`PTASK_SERVE_BIND`, e.g. `http://100.x.y.z:9501`);
`/etc/profile.d/ptask.sh` sets `PTASK_SYNC_URL` everywhere. The unit binds
that interface IP directly, keeping the API off the public/LAN NICs.
Application-level auth is now fail-closed for non-loopback binds. Only use
`PTASK_ALLOW_UNAUTHENTICATED=1` for a deliberately isolated test deployment.

The server speaks HTTP/1.1 and closes a connection whose request headers
take longer than 30s to arrive (slow-header / slowloris protection). The same
timer reaps idle keep-alive connections: one that sends no new request within
30s of its last response is closed, so clients must expect to reconnect.

Known gaps, by design for a tailnet-only service: request **bodies** and
streamed responses (the `/mcp` SSE stream) are not time-limited, so a client
that sends complete headers and then trickles its body (body slowloris) holds
its connection and task until it finishes or the server shuts down; and there
is no cap on concurrent connections (each costs a task and a file
descriptor, bounded only by `LimitNOFILE`). Keep the bind on the tailnet
address; put a reverse proxy with body and connection limits in front if the
API is ever exposed more widely. On SIGTERM it stops
accepting, closes open MCP SSE streams, gives other in-flight requests up to
10s and then closes whatever is still open (a trickled request body, say),
and finally gives queued outbound webhooks up to 15s. A request blocked on
SQLite's 30s busy timeout holds the runtime meanwhile, so the worst case is
about 30s (measured 29.3s), inside systemd's default 90s stop timeout.

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
sqlite3 /tmp/tasks-restored.db 'PRAGMA integrity_check; SELECT COUNT(*) FROM tasks;'
```

### Promote a restored copy over the live DB

The one procedure for putting any restored file live: a Litestream restore,
a nightly snapshot, or the pre-v0.1.0 baseline. Every step is load-bearing.
SQLite treats whatever `tasks.db-wal` sits next to `tasks.db` as that file's
log, so a WAL left by the old database is replayed onto the restored one: a
small WAL gives "database disk image is malformed", a large one silently
reverts the restore. `pt serve` keeps pooled connections open (with
`PTASK_WAL_AUTOCHECKPOINT=0` SQLite never checkpoints its WAL), the dashboard
sidecar holds long-lived read connections, and a killed process leaves its
WAL behind. So: stop everything, prove nothing holds the files, and move the
`-wal`/`-shm` aside together with the database. Run the steps one at a time
and check each result before going on.

```bash
DBDIR=~/puretensor-tasks
RESTORED=/tmp/tasks-restored.db     # the copy to promote

# 1. Verify the copy out of place: "ok", and note the task count.
sqlite3 "$RESTORED" 'PRAGMA integrity_check; SELECT COUNT(*) FROM tasks;'
test ! -e "$RESTORED-wal" || echo "STOP: $RESTORED has its own WAL; checkpoint it first"

# 2. Stop every pTask unit: timers first so nothing new starts, then the
#    dashboard, pt serve, Litestream and any oneshot still running.
systemctl --user list-units 'ptask-*' --state=active --no-legend   # note what to restart
systemctl --user stop 'ptask-*.timer'
systemctl --user stop 'ptask-*.service'
systemctl --user list-units 'ptask-*' --state=active,activating,deactivating --no-legend
#    ^ must print nothing

# 3. Nothing may still hold the files (pt bot, pt tui, an open sqlite3 shell);
#    use sudo if a reader runs as another user. Go only on "nothing holds".
#    Without fuser (psmisc) the check cannot run, which is not a "go".
if ! command -v fuser >/dev/null; then
    echo "STOP: fuser not installed (apt install psmisc), so nothing is checked"
elif fuser -v "$DBDIR"/tasks.db*; then
    echo "STOP: the processes above still hold the DB"
else
    echo "nothing holds the DB"
fi

# 4. Move the database AND its -wal/-shm aside (kept as the way back).
ASIDE="$DBDIR/pre-restore-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -m 0700 "$ASIDE"
for f in tasks.db tasks.db-wal tasks.db-shm; do
    if [ -e "$DBDIR/$f" ]; then mv "$DBDIR/$f" "$ASIDE/"; fi
done

# 5. Install the restored file: copy under a temp name, rename into place.
install -m 0600 "$RESTORED" "$DBDIR/tasks.db.restoring"
mv "$DBDIR/tasks.db.restoring" "$DBDIR/tasks.db"

# 6. Check what is now live: "ok" and the count from step 1.
sqlite3 "$DBDIR/tasks.db" 'PRAGMA integrity_check; SELECT COUNT(*) FROM tasks;'

# 7. Replication first, then serve, the dashboard and the timers (plus
#    anything else step 2 listed).
systemctl --user start ptask-litestream.service
systemctl --user start ptask-serve.service
systemctl --user start ptask-dashboard.service
systemctl --user start ptask-backup.timer ptask-distill.timer ptask-accountability.timer \
    ptask-scoring.timer ptask-reaper.timer ptask-export.timer ptask-restore-verify.timer \
    ptask-replica-check.timer
litestream generations -config ~/.config/litestream/litestream.yml "$DBDIR/tasks.db"
#    ^ Litestream starts a new generation for the replaced file
```

`$ASIDE` keeps the pre-restore database with its own WAL: open
`$ASIDE/tasks.db` in place to read that state, or move the three files back
(steps 2–7 again) to undo the promotion.

### Rollback

Without Litestream, while `pt` runs with `PTASK_WAL_AUTOCHECKPOINT=0`, only
the occasional process that does not load `.env` checkpoints the WAL, so in
practice it keeps growing. A `PRAGMA
wal_autocheckpoint` from a `sqlite3` shell does not help: it changes only
that shell's connection, and `pt serve`'s pooled connections keep the value
they were opened with. Change it where `pt` reads it, then restart:

```bash
systemctl --user disable --now ptask-litestream.service ptask-replica-check.timer
# Drop the override: pt then keeps SQLite's default (checkpoint every 1000 pages).
sed -i '/^PTASK_WAL_AUTOCHECKPOINT=/d' ~/puretensor-tasks/.env
# Long-lived processes reopen their connections; oneshot timers re-read .env
# on every run (let any running one finish: the second command lists them).
systemctl --user restart ptask-serve.service ptask-dashboard.service
systemctl --user list-units 'ptask-*.service' --state=activating --no-legend
# Verify: prints 0|0|0 (not blocked, WAL emptied); retry if the first field is 1.
sqlite3 ~/puretensor-tasks/tasks.db 'PRAGMA wal_checkpoint(TRUNCATE);'
ls -l ~/puretensor-tasks/tasks.db-wal   # recheck after a day: stays in the low MB (~1000 pages)
```

The ansible playbook enables Litestream and seeds the `.env` line on the
canonical host. While rolled back, run it with `-e ptask_litestream=false`:
it then keeps Litestream and the replica check disabled, removes
`PTASK_WAL_AUTOCHECKPOINT=0` (any other value is left alone) and restarts
`ptask-serve`.

The weekly restore drill's Litestream check now fails, correctly: the replica
is frozen. Expect that alert until Litestream is back, or stop
`ptask-restore-verify.timer` (which also pauses its nightly checks).

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

## Release tags (who can publish a release)

Both release workflows refuse a tag whose commit is not on `main` (the
"verify the tagged commit is on main" step). That stops accidents only. A
tag push runs the workflow file from the tagged commit, so whoever can push
a `v*` tag can also delete that step in the commit they tag, and
`scripts/release.sh`'s clean-`main` check runs only on the operator's
machine. Who can publish a release is decided by who can push `v*` tags,
and these forge settings are the real controls:

- **GitHub tag ruleset**: Settings → Rules → Rulesets → New tag ruleset,
  targeting `refs/tags/v*`: restrict creation, update and deletion to the
  release maintainers (bypass list) and block force pushes.
- **GitHub `release` environment**: the publishing job of
  `.github/workflows/release.yml` declares `environment: release`. In
  Settings → Environments → `release`, limit deployment branches and tags
  to the protected `v*` tags (optionally add a required reviewer). A run
  from any other ref then cannot publish. Until rules are set the
  environment gates nothing.
- **Gitea protected tags**: repository Settings → Tags → protect `v*` and
  allow only the release maintainers, so a tag pushed straight to the
  mirror cannot publish the Gitea release either.

A tag that edits the workflow to drop the `environment:` line escapes the
environment rule too, which is why the tag ruleset comes first.
