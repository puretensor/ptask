# Agent-native surface (v2.4.0)

pTask is native vocabulary for agents: an MCP server, claim/lease mechanics,
provenance links, idempotent capture, and a git-diffable export.

## MCP server

Two transports, one handler, 11 tools (`task_next / task_list / task_add /
task_show / task_done / task_dismiss / task_edit / task_claim / task_capture /
task_search / task_digest`):

- **streamable-HTTP** at `http://100.121.42.54:9501/mcp`, bearer-gated to the
  **hal** named token (write scope). This mount IS HAL's surface — per-request
  identity can't reach rmcp tool handlers, so attribution is pinned
  `actor=hal, source=mcp` and the gate only admits hal's credential. Other
  agents use the scoped REST API with their own named tokens.
- **stdio** via `pt mcp` — local registration without a network hop; actor
  from `$PTASK_ACTOR`.

Registration (`~/.claude.json` → `mcpServers`):

```json
"ptask": {
  "type": "http",
  "url": "http://100.121.42.54:9501/mcp",
  "headers": { "Authorization": "Bearer $(cat ~/.config/ptask/hal.token)" }
}
```

or stdio: `{ "type": "stdio", "command": "pt", "args": ["mcp"], "env": {"PTASK_ACTOR": "hal"} }`.

## Agent mechanics

- **task_claim** — atomic todo/backlog/triage → in_progress; the check-and-set
  is one UPDATE, so parallel agents can't both win. Journaled `task.claimed`.
- **task_add(discovered_from)** — records a `discovered_from` link in
  `task_links`; mirrors HAL's spawn_task provenance pattern.
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
# fleet-sentry escalation → task (idempotent per escalation id)
curl -s -X POST http://100.121.42.54:9501/capture \
  -H "Authorization: Bearer $PTASK_API_TOKEN" -H 'Content-Type: application/json' \
  -d '{"text":"[fleet-sentry] ceph HEALTH_WARN: 3 pgs degraded",
       "source":"fleet-sentry", "severity":3,
       "client_key":"fleet-sentry:esc-1234"}'

# pureMind heartbeat attention item (no severity — goes through distill)
curl -s -X POST http://100.121.42.54:9501/capture \
  -H "Authorization: Bearer $PTASK_API_TOKEN" -H 'Content-Type: application/json' \
  -d '{"text":"PureClaw PR #50 needs operator review (open 38h)",
       "source":"heartbeat", "client_key":"heartbeat:pr50-review"}'
```

Adapter wiring lives in the CONSUMING repos (fleet-sentry, pureMind
heartbeat, nexus) — each has a named scoped token. pending.md ↔ ptask
reconciliation: heartbeat items that reference a PT-N stop being re-raised
(the PT task is the record); new pending.md entries flow through the capture
adapter above.

## Export

`pt export --git` writes `tasks.jsonl` / `task_links.jsonl` /
`task_labels.jsonl` to `~/puretensor-tasks/export/` and commits in place —
a greppable, diffable projection (the SQLite spine stays canonical).
`ptask-export.timer` runs it nightly at 04:45 UTC.

## Outbound webhooks (specola)

`pt serve` fans out journal events to `PTASK_WEBHOOK_URLS` (comma-separated,
HMAC-signed with `PTASK_WEBHOOK_SECRET` — see `webhooks::sign`). Point one at
specola's ingest to push task changes instead of having specola poll.
