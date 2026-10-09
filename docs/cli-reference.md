# pt(1) — Command Reference

Pre-rendered manpage at [docs/gen/pt.1](gen/pt.1); regenerate with
`pt gen-manpage > docs/gen/pt.1`.

## Globals

| Flag | Env | Default |
|---|---|---|
| `--db <PATH>` | `PTASK_DB` | `~/puretensor-tasks/tasks.db` |
| `--help`, `-h` | — | — |
| `--version`, `-V` | — | — |

## Tasks

### `pt add <text> [...]`

Create a task. The free-text title runs through the quick-add parser
(see [dsl.md](dsl.md)) unless `--raw` is set.

| Flag | Use |
|---|---|
| `-p`, `--priority` | `low | normal | high | urgent | critical | 1..=5` |
| `-d`, `--description` | description body |
| `--deadline <ISO>` | `2026-05-21` or `2026-05-21T10:00:00+01:00` |
| `--reason` | persisted as `ai_reasoning` |
| `--raw` | skip quick-add parsing |
| `--unique` | refuse to create when a near-certain duplicate exists (score ≥ 0.75; reporting starts at 0.6, refusing is stricter); lists the candidates and exits 1 (`--json`: `{"created": false, "possible_duplicates": [...]}` on stdout) |

Every add reports likely duplicates (v3.45.0): open tasks, and tasks done or
dismissed in the last 14 days, whose title shares at least two identity words
and scores at least 0.6 (a Dice coefficient over lowercase title words,
stopwords and `PT-N` references dropped, plurals folded). The task is still
created; the human output adds a `duplicate?` line per candidate and the
`pt merge` command, and `--json` adds `possible_duplicates`
(`pt_id`, `title`, `status`, `score`). A task already merged away is never a
candidate; its canonical task is. Local and deterministic: no model, no
network.

### `pt list [filter] [...]` (alias `pt ls`)

| Flag | Use |
|---|---|
| `-s`, `--status` | `pending` (default), `done`, `delayed`, `dismissed`, `blocked`, `all` |
| `-p`, `--priority` | `1..=5` or label |
| `-n`, `--limit` | rows; default 20 |
| `-v`, `--verbose` | show description + UUID |
| `[filter]` positional | DSL — see [dsl.md](dsl.md) |

### `pt done <query> [...] [-m | --note TEXT]`

Mark done by `PT-N`, bare integer `42`, or title substring. `--note` journals
closure evidence (what was done, how it was verified) inside the completion
event, attributed like the close itself; with several tasks each gets the
note. A blank note refuses the close.

### `pt note <query> <text…>` (alias `annotate`, v3.43.0)

Append a note to a task: a finding, partial progress, evidence, a handover.
Words are joined with spaces; a lone `-` reads the note from stdin (pipe a
command's output in). Notes are append-only and attributed (actor + surface
from the journal); at most 16 KiB; blank refused. A substring reaches open
tasks; a done or dismissed task by `PT-N` or uuid, so evidence that arrives
after the close still lands. Honours `--idempotency-key` (a keyed `note -`
fingerprints the stdin text, so a retry with different text is refused). A
note counts as touching the task: it bumps `updated_at`, which the neglect
score, `pt review --stale-days` and the reaper read. Notes are not in `pt
search` (titles and descriptions only). `pt undo` treats `task.noted` as
transparent: a note is never undone and never shadows the close it follows.

Where the trail shows up: `pt show` (a NOTES section, oldest first; `--json`
adds `notes`), `pt context` (a `## Notes` section, one line per note, so a
worker starts from what earlier workers found), `pt log` (the text inline),
`pt digest` (each recently closed task carries its closing `note`), `pt
export` (`task_notes.jsonl`, live tasks only), the TUI detail pane, MCP
`task_show`, and the cockpit's task drawer. Long notes are truncated to about
300 characters with a marker in `pt digest` and the `pt context` markdown
brief; `pt show` and `pt context --json`'s `notes` array keep the full text.
Automated closers write their own evidence: a git `Closes PT-N` names the
push, commit and subject; `/capture/resolve` names the resolver and capture
key; the reaper says it reaped, why, and how to reopen.

### `pt priority <query> <level>` (alias `pt pri`)

Promote/demote a task's priority: `critical | urgent | high | normal | low`
or `1..=5`. Rescores immediately so `pt next` ordering reflects the change.

### `pt edit <query> [--deadline ISO | --clear-deadline] [--title T] [--desc D]` (alias `pt update`)

Edit task fields: set/clear the deadline and/or replace the title/description
(any combination; at least one required). Recurring tasks reject deadline
clearing; setting a deadline updates their next occurrence. A deadline change
feeds `score_urgency` and triggers an immediate rescore; a text-only edit does
not rescore. All fields in one local `pt edit` invocation commit together with
one journal event; a rejected field leaves the entire edit unapplied.

### `pt reopen <query>`

Flip a completed or dismissed task back to `pending` (resolve by PT-N for a
done task — substring resolution only matches active tasks). Logs a
`status_change` interaction the neglect score reads as a reopen, and rescores
immediately so the task re-enters `pt next` ordering.

### `pt show <query>`

Print one task's full row plus side-table detail: labels, project, duration,
dependencies (`deps on` / `blocks`), and recurrence. Global `--json` emits the
task object plus `goal_chain` (`[{id, title, why}, ...]` leaf→root) and
`goal_source` (`direct` | `parent` | `none`). Human output includes a short
Why block when a chain exists.

### `pt context <query>`

Markdown worker brief for dispatch: task title, description, a Why section
listing the goal chain root→leaf (each with its why), and open blockers
(PT id + title). Succeeds with no Why section when the task has no goal.
See [goals.md](goals.md).

## Goals

```
pt goal add TITLE [--why W] [--parent G-n]
pt goal ls [--all]
pt goal show G-n
pt goal link PT-n G-n
pt goal unlink PT-n
pt goal set-parent G-n G-m
pt goal done G-n          # status → achieved
pt goal abandon G-n
pt goal orphans           # open tasks with no effective goal
```

All honour `--json`. Tree order is parent before children, siblings by seq.
`set-parent` refuses self and cycles. Walks are cycle-safe (stop on a
repeated node, depth cap 16). Full model: [goals.md](goals.md).

### `pt dupes [query] [--threshold 0.6] [-n 20]` (alias `dups`, v3.45.0)

Read-only. Without a query: pairs of open tasks that look like the same work,
best first, the older task as `a` and the newer as `b`, each with the
`pt merge B --into A` to fold it. With a query: likely duplicates of that one
task (open or closed in the last 14 days). `--json` honoured.

### `pt merge <duplicate> --into <task> [-m REASON]` (v3.45.0)

Close a duplicate into the task it duplicates, in one transaction:

- the duplicate is dismissed (`task.updated`, `duplicate_of`; a
  `task.merged` marker), so `pt show` reads "duplicate of PT-N (merged)"
  and the target lists it under "merged in" (`--json`: `duplicate_of`,
  `merged_in`);
- every task that depended on the duplicate now depends on the target.
  Dismissing a prerequisite satisfies it, so without the move its dependents
  would silently unblock;
- the duplicate's own prerequisites and labels carry over, the target takes
  the higher priority, and takes the duplicate's deadline when it has none
  (and does not recur);
- a move that would close a dependency cycle refuses the whole merge.

The duplicate must be open; the target may be done (it was already done)
but not dismissed. `pt undo` reopens a mistaken merge's duplicate; what moved
to the target stays there. Honours `--idempotency-key`.

### `pt dismiss <query> [-m | --note TEXT]`

Soft-close a task (`status → dismissed`). Reversible with `pt reopen`. Distinct
from `pt rm`: the row and its history survive. `--note` records why (a
duplicate of PT-N, superseded, obsolete) in the dismissal event.

### `pt rm <query> [-y | --yes]`

Permanently delete a task (hard `DELETE` + a `task.deleted` tombstone for delta
sync). Prompts for confirmation unless `--yes`. The `interactions` history is
lost with the row — prefer `pt dismiss` unless you truly want it gone.

### `pt next [-n LIMIT]`

DAG-ready tasks: open tasks (triage, backlog, todo, in progress; snoozed and
blocked tasks don't compete) whose every `depends_on` prerequisite is done
or dismissed. A deleted prerequisite no longer blocks. Ordered severity
first: `priority DESC, priority_score DESC, created_at DESC`, so the
composite score only breaks ties inside a severity band (`pt why` explains
the score).

### `pt plan [--days N] [--work 09:00-18:00] [--tz TZ] [--slot-default 30] [-n LIMIT] [--gcal PATH] [--write]` (v3.2.0)

Advisory day planner: fits the `pt next` ready queue (with each task's
`duration_min`, defaulting to `--slot-default`) into your calendar's free
slots. Free slots come from `gcalendar.py freebusy --json` (working-hours
windows minus busy blocks). Greedy first-fit in priority order; tasks that
don't fit are listed as unscheduled. **Dry-run by default** (prints the plan,
touches nothing). `--write` creates tentative `[pt] PT-N ...` events on our own
`--account` calendar only. The script defaults to
`$HOME/.config/puretensor/gcalendar.py`; override it with `--gcal` or
`PTASK_GCAL`. `--json` (global) emits `{tz, scheduled[], unscheduled[]}`.

### `pt branch <query>`

Print a Linear-style branch name for the matched task, e.g.
`feature/PT-42-buy-bread-tomorrow-10am`. Pipe into `git checkout -b`.

## Scoring & why (v2.2.0)

```
pt scoring run            # composite v2 (growth urgency, real neglect, link deps, effort)
pt scoring run --v1       # legacy v1 formula
pt scoring run --diff     # fresh top-20 vs the stored ordering, then the run
pt scoring run --dry-run  # compute, print, don't write
pt why PT-42              # component breakdown: urgency/neglect/dependency/effort/llm + rank
```

v2 composite = 0.35·urgency + 0.20·neglect + 0.15·dependency + 0.30·(priority/5·effort_factor) + clamp(score_llm, ±0.15).
No-deadline urgency GROWS with age (aged p5 can never rank below fresh p3). `score_llm` is written by the Phase-8 triage pass; zero until then.

## Saved views

```
pt view save <name> '<filter-dsl>'   # store
pt view list                          # list
pt view show <name> [-s all]          # run (open tasks; -s all lifts it)
pt view rm <name>                     # delete
```

## Agent surface (v2.4.0)

```
pt mcp                    # MCP server over stdio (tools: task_next/list/add/…)
pt digest [--days 7]      # session-priming JSON: recent done/dismissed + ready queue
pt export [--git] [--out DIR]   # JSONL projection of the spine incl. task_notes.jsonl (nightly timer)
pt delegate PT-42         # prints the operator-gated claude -p command (never spawns)
```

HTTP MCP mounts at /mcp in `pt serve` (hal token only) — docs/agent-surface.md.

## Long-running daemons

- `pt tui` — ratatui frontend with `j/k`, single-key edits, fuzzy search.
- `pt serve [--bind 127.0.0.1:9501]` — HTTP API: `/sync`, `/capture`,
  `/webhook/{gitea,github}`, `/metrics`. See [sync-api.md](sync-api.md).
- `pt bot` — Telegram long-poll handler.

## Pipeline orchestrators

| Verb | Cadence | Description |
|---|---|---|
| `pt distill [--batch 200]` | hourly (`*:15`) | Native fail-closed distillation: consumes new `raw_items` only; LLM classify+consolidate via `PTASK_LLM_BACKEND` (default `local`: the OpenAI-compatible endpoint at `LOCAL_LLM_URL`, default `http://127.0.0.1:8600/v1`; `gemini`: structured output with `thinkingBudget=0`), transient retry, token/semantic/temporal dedup. Exit 3 = `gemini` backend without `GOOGLE_API_KEY`, before consumption; a failed provider preflight exits 1 with a `distill.failed` event. Env: [operations.md](operations.md#provider-env). |
| `pt accountability run [--dry-run]` | `*:0/15` | Escalation state machine + dispatch. |
| `pt scoring run [--dry-run]` | `hourly` | Composite priority recompute. |
| `pt backfill` | one-shot | Mint PT-N for any task lacking one. |

## Workflow (v2.0.0)

| Verb | Use |
|---|---|
| `pt start <query>` | mark in progress (status_v2 `in_progress`) |
| `pt kind <query> <scout\|ship> [--deliverable report\|pr\|none]` | set a task's shape; `pt add --kind scout` declares it at creation |
| `pt promote <query>` | investigation → implementation: flips `kind` scout→ship (and `report`→`pr`) on the **same row**, so the open count is unchanged. Refuses a terminal task — reopen it first. |
| `pt snooze <query> <until…>` | park until a date (natural language ok); auto-wakes to todo via the hourly scoring run |
| `pt depend <query> --on <target> [--clear]` | dependency edges in `task_links`; `pt next` hides tasks with unmet deps; **`pt done` refuses (exit non-zero, names the open blockers) while any prerequisite is still open** — dismissed prerequisites count as satisfied; `pt start` / claim are not gated (only closing is); no `--on` shows current edges |
| `pt review [--stale-days N]` | interactive sweep of stale tasks (TTY: k/d/x/s/q; non-TTY prints the list) |
| `pt search <query…> [-n N]` | FTS5 full-text over titles + descriptions; free text: every word must match, punctuation and AND/OR/NOT are literal (`follow-up`, `c++`, `PT-2201` just work), a trailing `*` matches a prefix |
| `pt bulk '<filter>' --set-priority P \| --done \| --dismiss [-m NOTE] [--dry-run]` | one action across every DSL match; `--note` journals the same note with each completion or dismissal |
| `pt done <q1> <q2> …` | done now accepts multiple tasks |

Globals (v2.0.0): `--json` on task-facing verbs emits machine-readable
output; `--idempotency-key <k>` (not starting with `capture`, which the capture
lane reserves) keys the mutation's event so retries are
safe: a retry of the same command on the same task prints `replayed` and
exits 0 without re-applying (a retried `add` returns the task it created);
a key already used for a different command, different arguments, another
task or by another actor is an error. Verbs that cannot replay safely
(`undo`, `token`, `approval`, `approve`/`reject`, `reap`, `review`, reads)
refuse the flag outright. Over `/sync`, command uuids are scoped to the
authenticated client. Since v3.25.0 the human output renders through the shared PureTensor
terminal theme (the `fleet-upgrade` look: gradient headline rules, box-ruled
severity-banded tables, semantic pills — green done, amber needs a human, red
critical). Colour is on only when stdout is a TTY; `--color always|never`,
`--no-color`, `NO_COLOR` and `PT_COLOR=always|never` override that, and
`--json` is always plain. `pt list`, `pt next`, `pt log`, `pt view
show|save|list|rm`, `pt bulk`, `pt review` (the stale list; no interactive
sweep), `pt delegate` and every `pt remote` verb honour `--json` too. Untrusted
task text is printed with control and bidi characters shown as U+FFFD;
`--json` keeps the exact text (serde escapes C0 controls; C1 and bidi pass
through). Quick-add gains `due:<date>` (scheduled) alongside hard deadlines.
Statuses are the 8-state v2 model: triage/backlog/todo/in_progress/
snoozed/done/dismissed/blocked (legacy column maintained for
not-yet-retired consumers).

## Journal & tokens (v1.17.0)

| Verb | Use |
|---|---|
| `pt log <query> [-n N]` | attributed event history for a task: when, who (actor), via which surface, what |
| `pt undo [--yes]` | reverse **your own** most recent eligible mutation (the caller's actor, `$PTASK_ACTOR`, default `shell`, through the CLI/TUI surface: a task a `pt mcp` server added under your actor is not yours) within your last 50 task events (done/dismiss → reopen, create → delete); a later event on that task by anyone protects it, including claims, promotions, edits and prior reversals. A created task that another task depends on or is depended on by, that parents another task, or that an approval references, is never deleted: when your most recent undoable change is such a create, undo refuses and names it rather than reaching further back. Undoing a create deletes the task permanently, so it names the PT-N and title and asks first; without a TTY (or with `--json`) it refuses unless `--yes`. Selection and reversal are atomic (a plan confirmed at the prompt is re-checked before anything changes); the reversal is itself attributed. |
| `pt token create <client_id> [--scope read\|capture\|write\|admin]` | mint a named scoped API token (plain value shown ONCE; only the sha256 is stored) |
| `pt token list` | client, scope, active/revoked, created/last-used |
| `pt token revoke <client_id>` | revoke all active tokens for a client |

Server auth resolves, in order: legacy env `PTASK_API_TOKEN` → env metrics
token → `pt_api_tokens` lookup. Named-token requests are journaled under
their client_id; local mutations under `$PTASK_ACTOR` (default `shell`; `pt mcp` uses `$PTASK_MCP_ACTOR`, else `$PTASK_ACTOR`, else `mcp`).

## Remote (`pt remote`)

Talks to a canonical `pt serve` over Tailscale; no local DB.

| Verb | Use |
|---|---|
| `pt remote add "..." [--url ...]` | quick-add on the remote canonical |
| `pt remote list [-s STATUS -p P -n N]` | server-side `GET /list` (severity order, like local `pt list`); `-p` is folded into the filter |
| `pt remote done <query> [-m NOTE]` | server-side `/resolve` + `task_done` (with the closure note) |
| `pt remote note <query> <text…>` | server-side `task_note`; `-` reads stdin |
| `pt remote priority <query> <level>` (alias `pri`) | server-side `/resolve` + `task_priority` (+ server rescore) |
| `pt remote edit <query> [--deadline ISO \| --clear-deadline] [--title T] [--desc D]` (alias `update`) | server-side `/resolve` + `task_edit` (deadline) and/or `task_retext` (title/desc) |
| `pt remote reopen <query>` | server-side `/resolve` (incl. done) + `task_reopen` |
| `pt remote show <query>` | base row + side-table detail (incl. notes) via `GET /detail/{uuid}` (read-only) |
| `pt remote next [-n N]` | DAG-ready tasks via `GET /next` (server resolves `depends_on`) |
| `pt remote dismiss <query> [-m NOTE]` | server-side `/resolve` + `task_dismiss` (soft close; reversible via reopen) |
| `pt remote start <query>` | server-side `task_start` |
| `pt remote snooze <query> <until…>` | server-side `task_snooze` (date parsed locally) |
| `pt remote depend <query> --on <t> [--clear]` | server-side `task_depend` |
| `pt remote rm <query> [-y]` | server-side `task_delete` (tombstoned); asks `PT-N "title"` first, refuses without `--yes` on a non-TTY or with `--json`; a substring matches open tasks only (exact PT-N/uuid reach any status) |
| `pt remote list --filter '<DSL>'` | SERVER-side filtered list via `GET /list` |
| `pt remote version` | compare client vs server `GET /version`; exits non-zero on skew |

`--url` defaults to `$PTASK_SYNC_URL` then `http://127.0.0.1:9501`.
`$PTASK_API_TOKEN` is sent as a bearer token; when the URL is plain `http`
to a host that is neither loopback nor a Tailscale address (100.64.0.0/10,
`fd7a:115c:a1e0::/48`, `*.ts.net`, all WireGuard-encrypted) the client
prints a one-line cleartext-token warning on stderr (once per process). A
single-label MagicDNS name (`http://tensor-core:9501`) is resolved and counts
as Tailscale when every address it resolves to does. Use `https` elsewhere.

Every remote error also runs the version handshake: a 401/404 from a
mismatched deploy appends `version skew: client vX vs server vY` to the
error instead of masquerading as an auth/routing failure.

## Codegen

```
bash scripts/generated-artifacts.sh --write
bash scripts/generated-artifacts.sh --check
```

The write form refreshes the manpage and all three completion files. The check
form regenerates them in a temporary directory and fails on any drift; CI and
the release helper run it automatically.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | success |
| `1` | runtime error (DB, network, parse, a refused mutation such as a blocked `pt done`) and any failure without a code below |
| `2` | command-line usage error (clap) |
| `3` | `pt approval verify` / `consume`: the approval is still pending. `pt distill`: `PTASK_LLM_BACKEND=gemini` without `GOOGLE_API_KEY` (nothing consumed) |
| `4` | `pt approval verify` / `consume`: rejected, withdrawn or expired |
| `5` | `pt approval verify` / `consume`: payload digest mismatch (consume does not latch) |
| `6` | `pt approval verify` / `consume`: already consumed |
| `64` | usage error from `scripts/release.sh` and similar helpers |

`pt` keeps the default SIGPIPE action, so when a downstream reader closes
early (`pt list | head`) it ends by signal and shells report `141`.
