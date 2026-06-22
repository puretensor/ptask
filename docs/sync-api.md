# HTTP Sync API

`pt serve` exposes a small Todoist-style sync surface plus a one-field
`/capture` ingest point. The full surface:

| Endpoint | Method | Use |
|---|---|---|
| `/healthz` | GET | liveness |
| `/version` | GET | `pt --version` string |
| `/sync` | POST | command + delta sync |
| `/next` | GET | DAG-ready tasks (`?limit=N`); read-token gated (v1.9.0) |
| `/detail/{uuid}` | GET | one task's side-table detail; read-token gated (v1.9.0) |
| `/capture` | POST | one-shot raw-text ingest |
| `/webhook/gitea` | POST | HMAC-signed `Fixes PT-N` parser |
| `/webhook/github` | POST | as above, GitHub HMAC |
| `/metrics` | GET | Prometheus exposition |

## Auth

By default, `pt serve` preserves the original localhost/Tailscale-only
operating mode and accepts mutating requests without an application token.
Set `PTASK_API_TOKEN` to require application-level auth for `POST /sync`,
`POST /capture`, and `POST /email`.

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
| `task_dismiss` (v1.10.0) | `{ task_uuid \| pt_id }` | soft-closes a task (`status → dismissed`); reversible via `task_reopen`. |

Each command records exactly one event keyed on its `uuid`, so `/sync` replays
are idempotent. More commands (`task_delete`, `view_save`, …) are backward-
compatible additions; the wire format is stable.

## `POST /capture`

```json
{ "text": "...", "source": "telegram|email|cli|..." }
```

Drops into `raw_items` for the distillation pipeline. Returns `{ "ok":
true, "raw_item_id": N }`.

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
