# HTTP Sync API

`pt serve` exposes a small Todoist-style sync surface plus a one-field
`/capture` ingest point. The full surface:

| Endpoint | Method | Use |
|---|---|---|
| `/healthz` | GET | liveness |
| `/version` | GET | JSON with crate versions |
| `/sync` | POST | command + delta sync |
| `/next` | GET | DAG-ready tasks (`?limit=N`); read-token gated (v1.9.0) |
| `/detail/{uuid}` | GET | one task's side-table detail; read-token gated (v1.9.0) |
| `/resolve` | GET | server-side PT-N/title lookup (`?query=...&include_terminal=false`); read-token gated (v1.12.0) |
| `/capture` | POST | one-shot raw-text ingest |
| `/email` | POST | raw RFC 822 email ingest |
| `/webhook/gitea` | POST | HMAC-signed `Fixes PT-N` parser |
| `/webhook/github` | POST | as above, GitHub HMAC |
| `/metrics` | GET | Prometheus exposition |

## Auth

Loopback `pt serve` binds keep the original local-dev mode and accept requests
without an application token. Non-loopback binds fail closed unless
`PTASK_API_TOKEN` is set; `PTASK_ALLOW_UNAUTHENTICATED=1` is an explicit
test-only override for isolated deployments.

`PTASK_API_TOKEN` gates `POST /sync`, `POST /capture`, `POST /email`, and the
read APIs (`GET /next`, `GET /detail/{uuid}`, `GET /resolve`, `GET /metrics`).
`/healthz`, `/version`, and HMAC-verified git webhooks do not use this bearer
token.

When configured, clients must send one of:

```text
Authorization: Bearer <PTASK_API_TOKEN>
X-PTask-Token: <PTASK_API_TOKEN>
```

`pt remote` automatically forwards `PTASK_API_TOKEN` as a bearer token when
the environment variable is set on the client node.

## `POST /sync`

### Request

```json
{
  "sync_token": "42",
  "resource_types": ["tasks"],
  "commands": [
    {
      "type": "task_create",
      "uuid": "<idempotency-key>",
      "temp_id": "client-side-temp",
      "args": { "text": "buy bread tomorrow 10am @home p1 ~30m" }
    },
    {
      "type": "task_done",
      "uuid": "<idempotency-key>",
      "args": { "pt_id": "PT-42" }
    }
  ]
}
```

| Field | Notes |
|---|---|
| `sync_token` | `"*"`, `""`, or absent → full sync. Otherwise an opaque integer string from a prior response. |
| `resource_types` | advisory; `["tasks"]` is the only meaningful value today. |
| `commands` | optional; pure read if empty. |
| `commands[].uuid` | client-generated, idempotency key. Replays return `"ok"` without re-applying. |
| `commands[].temp_id` | optional client-side handle; mapped to the real `task_uuid` in the response. |

### Response

```json
{
  "sync_token": "47",
  "resources": { "tasks": [<Task>, ...] },
  "sync_status": {
    "<command-uuid>": "ok"
    | { "error": "<message>" }
  },
  "temp_id_mapping": { "<temp_id>": "<real-task-uuid>" }
}
```

- `resources.tasks` carries the delta: full task set on full sync,
  changed-since-sync_token on incremental.
- `sync_token` is the new monotonic cursor (current `pt_event_log.id`).

### Commands

| `type` | `args` | Side effects |
|---|---|---|
| `task_create` | `{ text, source_type? }` | runs quick-add parser, inserts to `tasks` + `pt_extensions`, optional `pt_recurrence`. |
| `task_done` | `{ task_uuid }` or `{ pt_id }` | flips status to `done` or advances recurrence in-place, logs an `interaction` row. |
| `task_priority` (v1.8.0) | `{ task_uuid \| pt_id, priority }` | sets priority (1..=5), logs a `priority_change` interaction, rescores. |
| `task_edit` (v1.8.0) | `{ task_uuid \| pt_id, deadline }` | sets the deadline (ISO string) or clears it (JSON `null`); rescores. |
| `task_reopen` (v1.8.0) | `{ task_uuid \| pt_id }` | flips a done/dismissed task back to `pending` (logs the neglect-score reopen signal). |
| `task_retext` (v1.9.0) | `{ task_uuid \| pt_id, title?, description? }` | replaces the title and/or description (at least one required). |
| `task_dismiss`, `task_start`, `task_snooze` (args.until ISO), `task_depend` (args.on query, args.clear bool), `task_delete` (v1.10.0) | `{ task_uuid \| pt_id }` | soft-closes a task (`status → dismissed`); reversible via `task_reopen`. |

Each command records exactly one event keyed on its `uuid`, so `/sync` replays
are idempotent. More commands (`task_delete`, `view_save`, …) are backward-
compatible additions; the wire format is stable.

## `GET /resolve`

Server-side lookup for remote clients that need a single `task_uuid` before
issuing a mutation. This avoids full-syncing the entire task table for
`pt remote done|edit|priority|dismiss|reopen|show`.

```text
GET /resolve?query=PT-42&include_terminal=false
GET /resolve?query=archive%20receipt&include_terminal=true
```

Semantics:

- `PT-N` or bare integer `N` matches the exact PT id across any status.
- Other queries perform a case-insensitive title substring search.
- `include_terminal=false` excludes `done` and `dismissed` title matches.
- `include_terminal=true` includes all statuses for read/reopen flows.

Response:

```json
{ "task": <Task> }
```

Status codes: `200` one match, `400` empty query, `404` no match, `409`
multiple substring matches.

## `POST /capture`

```json
{ "text": "...", "source": "telegram|email|cli|..." }
```

Drops into `raw_items` for the distillation pipeline. Returns:

```json
{ "id": 123, "source_type": "telegram", "source_date": "2026-06-25" }
```

## `POST /email`

Accepts a raw RFC 822 message body (`message/rfc822` or `text/plain`); parses
subject/body into one `raw_items` row with `source_type="email"`. Returns:

```json
{ "id": 123, "subject": "Subject line", "source_file": "email:<message-id>" }
```

## Webhooks

`POST /webhook/{gitea,github}` parses pushed commit messages for
`Fixes PT-N` / `Closes PT-N` directives and marks matching tasks done.
`Ref PT-N` and `Skip PT-N` are recognised by the magic-word parser but do
not close tasks.

HMAC verification: the secret comes from `PTASK_GITEA_WEBHOOK_SECRET` /
`PTASK_GITHUB_WEBHOOK_SECRET`. Body signature is `X-Hub-Signature-256`
(GitHub) or `X-Gitea-Signature` (Gitea).

## Outbound webhooks

Configure `PTASK_WEBHOOK_URLS=<url1>,<url2>` and `PTASK_WEBHOOK_SECRET` for HMAC-signed POSTs
on task events such as `task.created`, `task.completed`, and
`task.recurrence_advanced`.
Logged to `pt_webhook_log`. Signature header: `X-Ptask-Signature: sha256=<hex>`.

## Metrics

`/metrics` exposes (subset):

| Metric | Type | Labels |
|---|---|---|
| `pt_tasks_total` | gauge | `status` |
| `pt_capture_total` | counter | `source` |
| `pt_dsl_parse_duration_seconds` | histogram | `kind` (`quickadd` / `filter`) |
| `pt_webhook_send_total` | counter | `result` (`ok` / `error`) |
| `pt_sync_commands_total` | counter | `kind` (`task_create` / `task_done` / ...) |

## Dashboard surface (v2.3.0)

The Triage Cockpit's API lives in `pt serve` (the Python sidecar shrank to a
voice shim). HTTP **Basic** auth (`PTASK_DASH_USER`/`PTASK_DASH_PASS`; open
when no password configured — local/dev only). Same shapes as the sidecar
v0.6.0 contract.

Reads: `GET /api/stats · /api/tasks?status=&limit= · /api/critical?limit= ·
/api/timeline · /api/heatmap · /api/tasks/{id}/events` (journal history) ·
`GET /api/stream` (SSE, `event: change` frames with journal deltas).
`GET /` serves the cockpit when `PTASK_DASH_WWW` exists, else the banner.

Writes (attributed `actor=dashboard`): `POST /api/tasks` (create, quick-add
tokens parse) · `POST /api/tasks/{id}/done|dismiss|reopen` ·
`/{id}/snooze {days}` · `/{id}/priority {level}` ·
`/{id}/edit {title?,description?,priority?,deadline?|null}`.
`POST /api/voice` proxies to the Python voice shim (`PTASK_VOICE_SHIM_URL`,
default http://127.0.0.1:9510).

## POST /tg/callback (v2.2.0)

Executes a Telegram inline-button tap forwarded by nexus (the bot's single
`getUpdates` owner). Requires `write` scope.

```json
{"data": "ptdone:<task-uuid>", "callback_id": "<telegram callback id>"}
```

Verbs: `ptdone` | `ptsnooze` (3 days) | `ptdismiss`. Idempotent per
`callback_id` (journal uuid `tg-cb:<id>`); duplicate taps return
`{"ok":true,"duplicate":true}`. Actions land in the journal as
`actor=telegram`, `source=tg-callback`.

## GET /list (v2.0.0)

`GET /list?filter=<DSL>&status=pending|all&limit=N` — server-side filtered
task list (read scope). The DSL is the `pt list` grammar; parse errors
return 400 with the reason.

## POST /capture fast lane (v2.1.0)

**v2.4.0:** optional `client_key` makes capture idempotent — a re-send with
the same key + text returns HTTP 200 `{"duplicate": true, "id": <original>}`
instead of a new row. Federation adapters MUST pass one (docs/agent-surface.md).


`severity >= 3` (explicit field, or a puresentinel incident source with
`[puresentinel sevN]` in the text) creates the task SYNCHRONOUSLY —
attributed to the capturing token identity, `source_type=incident`,
priority sev3→4, sev4+→5. The response then carries `task_uuid` + `pt_id`,
and the raw_items record is marked processed. Requires capture scope.
