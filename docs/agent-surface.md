# Agent-native surface (v2.4.0)

pTask is native vocabulary for agents: an MCP server, an atomic task claim
with an owner and an optional lease (see below), provenance links, idempotent capture, and
a git-diffable export.

## MCP server

Two transports, one handler, 25 tools (`task_next / task_list / task_add /
task_show / task_done / task_dismiss / task_note / task_edit / task_claim /
task_heartbeat / task_release / task_promote / task_duplicates / task_merge /
task_depend / task_capture / task_search / task_digest` plus
`approval_request / approval_list / approval_status / approval_withdraw` —
agents request, they never decide; see [`approvals.md`](approvals.md) — plus
`goal_list / goal_show / goal_link`; `task_show`, `task_next` and `task_claim`
carry `goal_chain` and `goal_source` per task; see [`goals.md`](goals.md)):

- **streamable-HTTP** at `http://127.0.0.1:9501/mcp` (or your `PTASK_SYNC_URL`),
  bearer-gated to HAL alone: only a named token whose client is `hal` with
  write (or admin) scope (`pt token create hal --scope write`) is accepted.
  Any other named token, and the legacy `PTASK_API_TOKEN`, gets 401 `mcp
  requires the hal token`. Per-request identity cannot reach rmcp tool
  handlers, so attribution is pinned `actor=hal, source=mcp`. Other agents
  use the scoped REST API with their own named tokens, or stdio.
- **stdio** via `pt mcp` — local registration without a network hop; actor
  from `$PTASK_MCP_ACTOR`, else `$PTASK_ACTOR`, default `mcp` (deliberately
  not the CLI's `shell`, so the operator can decide the client's approval
  requests; the MCP variable wins over an exported `PTASK_ACTOR`).

Registration (`~/.claude.json` → `mcpServers`):

```json
"ptask": {
  "type": "http",
  "url": "http://127.0.0.1:9501/mcp",
  "headers": { "Authorization": "Bearer <hal token>" }
}
```

The header value is sent verbatim: nothing in `~/.claude.json` runs shell
substitutions, so `$(cat ~/.config/ptask/agent.token)` would be sent as
those literal characters (401). Paste the token itself and keep the file
mode 0600, since it now holds a write credential. A project-scoped
`.mcp.json` can instead reference an environment variable, which Claude
Code expands there: `"Bearer ${PTASK_HAL_TOKEN}"`.

or stdio: `{ "type": "stdio", "command": "pt", "args": ["mcp"], "env": {"PTASK_MCP_ACTOR": "hal"} }`.

## Agent mechanics

`task_edit` applies all requested fields, labels and its attributed event in
one transaction. Invalid fields or database failures while applying the edit
leave the task and journal unchanged. A successful combined edit produces one
`task.updated` event containing the requested fields. Rescoring runs after
commit; a scoring failure does not roll back a successful edit.

- **task_done** — completes a task, or advances a recurring one in place
  (`status=advanced`, `next_deadline`). The reply lists `unblocked`: the
  tasks this close made ready. With `claim_next: true` (v3.47.0) it also
  claims the next ready task (task_next order, skipping in-progress ones) and
  returns it as `claimed_next` with its goal chain and `claim_token` (for
  task_heartbeat and task_release; null when nothing is claimable): close and
  continue without a task_next + task_claim round trip. Pass `expected_deadline` (the deadline
  you last saw; `""` = none) to make a retry or a duplicate safe: if the task
  has moved on, the call errors and nothing changes, instead of completing the
  next occurrence too. The dashboard's `POST /api/tasks/{id}/done` takes the
  same optional `{"expected_deadline": …}` body, and `/sync` `task_done` the
  same arg. Completing an already-done task is an error.
- **task_note** (v3.43.0) — append a note: findings, partial progress, a
  handover. Append-only, attributed to the caller (actor + `source=mcp`);
  works on done/dismissed tasks by PT-N or uuid, for evidence that arrives
  after the close. `task_done` and `task_dismiss` take an optional `note`
  (closure evidence / the reason) that rides inside the closing event, so the
  evidence and the close commit together: a close without it is a bare
  claim. `task_show` returns `notes` (oldest first, the newest 100) and
  `pt context` / the worker brief carries them as a `## Notes` section, so
  the next worker starts from what earlier ones found. `task_digest` gives
  each recently closed task its closing `note`. Compact surfaces (digest,
  the markdown worker brief) truncate a long note to about 300 characters
  with a marker; `task_show` / `pt context --json`'s `notes` array stay
  full. A note is transparent to `pt undo`: it is never reversed and never
  shadows the close it follows.
- **task_claim** — atomic todo/backlog/triage → in_progress; the check-and-set
  is one UPDATE, so parallel agents can't both win. Journaled `task.claimed`.
  Since v3.44.0 the task records its holder (`claimed_by`, `claimed_at`; a
  losing claimer's error names the holder) and, with `lease_minutes`
  (1..=1440), a lease (`claim_expires_at`). Each take returns `claim_token`
  (opaque, new on every claim): pass it to `task_heartbeat` and
  `task_release`. `task_show` returns `claim` (`by`, `at`, `expires_at`,
  `expired`) and does not include the token. A claim without a lease never
  expires on its own. An expired lease is free to claim (takeover, new
  token). Any writer can still `task_done` or `task_dismiss` a claimed task;
  leaving in_progress by any path drops the claim.
- **task_heartbeat** — renew the lease of the claim instance named by
  `claim_token` (default 30 minutes from now). Not journaled (it changes no
  task state). It fails with `claim lost: …` when that instance is no longer
  current (released, reclaimed, closed, retaken by another session of the
  same actor): that is the signal to **stop working** and not close the task.
  A heartbeat without `claim_token` is refused. A lease that ran out but was
  not reclaimed yet is still that instance's to renew.
- **task_release** — hand back the claim instance named by `claim_token`
  (in_progress → todo) without closing it, with an optional `reason`;
  journaled `task.released`. There is no force over MCP: a missing or stale
  token, another agent's claim, and an unowned in-progress task are all
  refused. The operator releases those from the CLI with `pt release --force`.
- **Recovery.** `pt reclaim` lists in-progress tasks whose lease ran out
  (`--apply` returns them to todo, journaled `task.reclaimed` with the holder
  and lease end; each is re-checked under the write lock, so a late
  heartbeat wins). An expired lease is also free: `task_claim` / `pt claim`
  and `pt start` take it over (start clears the lease so a later reclaim
  cannot undo the start). The hourly `pt scoring run` reclaims **only when
  the operator sets `PTASK_CLAIM_RECLAIM=1`** (off by default: it changes
  state on a timer). `task_digest` lists `expired_claims`; `/metrics` exports
  `pt_claims_active{holder}` and `pt_claims_expired`. A claim taken before
  v3.44.0 has its holder backfilled from its `task.claimed` event and no
  lease.
- **task_depend** — `task` depends `on` a prerequisite (`remove=true` drops the
  edge). **A task with open prerequisites cannot be closed** — `task_done`
  (and `pt done`, the dashboard, Telegram, sync, git-webhook auto-close,
  capture close-on-recovery) all refuse with a `Blocked` error naming every
  open blocker. How each surface reports it: `pt serve`'s dashboard route
  (`POST /api/tasks/{id}/done`) returns 409; Telegram's `/tg/callback`
  returns 422; `/sync` answers 200 with the error in that command's
  `sync_status` entry; the git webhook lists it under `errors`;
  `POST /capture/resolve` answers 200 and simply leaves the blocked task
  open (it is missing from `closed` and `pt_ids`; the refusal is only in the
  server log); the cockpit sidecar relays `pt done`'s refusal with HTTP 500;
  MCP answers with a JSON-RPC invalid-params error. Chains (`3 on 2 on 1`) enforce strict order; fan-out (`2 on 1`,
  `3 on 1`) lets 2 and 3 close in any order once 1 is done. A dismissed
  prerequisite counts as satisfied. `task_show` returns `blocked_by`.
- **task_promote** — flips an investigation into implementation work
  (`kind` scout → ship, `deliverable` report → pr) on the SAME row. Promotion
  must never close the scout and open a ship duplicate: that is re-ticketing,
  not disposition, and it inflates the open count. Journaled `task.promoted`;
  refuses a terminal task so a resurrection is always a deliberate `reopen`.
- **task_add(discovered_from)** — records a `discovered_from` link in
  `task_links`; mirrors HAL's spawn_task provenance pattern.
- **Duplicates (v3.45.0).** `task_add` replies with `possible_duplicates`
  when open tasks, or tasks closed in the last 14 days, have a similar title
  (lexical, deterministic; see `pt add` in the CLI reference). With
  `skip_if_duplicate: true` it creates nothing when one scores at least 0.75
  with the same identifier-like words (stricter than the 0.6 reporting
  threshold: related work, and titles that differ only in a number, date,
  hash or host, are mentioned, not refused) and replies `ok: false`,
  `created: false`, `skipped: true` with the candidates: the agent works or notes the
  existing task instead of filing a second copy. `task_duplicates(id)` lists
  candidates for an existing task. `task_merge(duplicate, into, reason?)`
  folds one into the other: dismissed as `duplicate_of`, dependents moved to
  the target (so nothing unblocks), prerequisites, labels, recurrence, goal,
  provenance and subtasks carried, the higher priority kept. A done target
  with open dependents on the duplicate is refused. `task_show` returns
  `duplicate_of` and `merged_in` from the `task_links` row.
- **task_digest** — deterministic session priming (recent done/dismissed,
  created count, ready queue). Deliberately NOT an LLM summary: the consumer
  is a model; structured facts beat a second model's paraphrase and can't
  fail closed or hallucinate.
- **pt delegate PT-N** — operator-gated skeleton: prints the headless
  `claude -p` command, never spawns it. Autonomy revisited once the loop is
  proven (per master-plan default).

## Federation (killing the parallel task stores)

Every adapter POSTs to `/capture` with a **stable `client_key`** — a re-send
of the same key + text returns `{"duplicate": true}` instead of a new inbox
row, which is what stops re-nag loops. severity ≥ 3 fast-lanes into a task.

```bash
# monitoring escalation → task (idempotent per escalation id)
curl -s -X POST http://127.0.0.1:9501/capture \
  -H "Authorization: Bearer $PTASK_API_TOKEN" -H 'Content-Type: application/json' \
  -d '{"text":"[monitor] disk usage above 90% on db-1",
       "source":"monitor", "severity":3,
       "client_key":"monitor:esc-1234"}'

# heartbeat attention item (no severity — goes through distill)
curl -s -X POST http://127.0.0.1:9501/capture \
  -H "Authorization: Bearer $PTASK_API_TOKEN" -H 'Content-Type: application/json' \
  -d '{"text":"PR #50 needs operator review (open 38h)",
       "source":"heartbeat", "client_key":"heartbeat:pr50-review"}'
```

Adapter wiring lives in the CONSUMING repos (fleet-sentry, pureMind
heartbeat, nexus) — each has a named scoped token. pending.md ↔ ptask
reconciliation: heartbeat items that reference a PT-N stop being re-raised
(the PT task is the record); new pending.md entries flow through the capture
adapter above.

## Export

`pt export --git` writes `tasks.jsonl` / `task_links.jsonl` /
`task_labels.jsonl` / `task_notes.jsonl` to `~/puretensor-tasks/export/` and commits in place —
a greppable, diffable projection (the SQLite spine stays canonical).
`ptask-export.timer` runs it nightly at 04:45 UTC.

## Outbound webhooks (specola)

`pt serve` POSTs events to `PTASK_WEBHOOK_URLS` (comma-separated,
HMAC-signed with `PTASK_WEBHOOK_SECRET` — see `webhooks::sign`). Only changes
made *through `pt serve`'s* `/sync` commands and git-webhook auto-closes are
pushed; writes from the local CLI, TUI, Telegram bot, dashboard sidecar, MCP
tools, timers and the server's other routes land in `pt_event_log` but are not
pushed. A consumer that needs every change should poll `/sync` with its
cursor (or use the push as a hint to sync) rather than rely on the webhook.
