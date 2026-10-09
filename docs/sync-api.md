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
without application credentials until a token is configured: `PTASK_API_TOKEN`,
`PTASK_METRICS_TOKEN`, or any unrevoked named token (`pt token create`) closes
anonymous access. While anonymous access is open, a request is served only
when its `Host` names the server itself (IP literal, `localhost`, the machine's short
hostname, `*.ts.net`, `PTASK_DASH_ALLOWED_HOSTS`), because a DNS-rebinding
page carries no credential either. A `.suffix` entry does not match the apex
name itself (list it separately), a request with no `Host` header passes, and
`pt serve` learns the machine's short hostname only on Linux. Tokenless clients
that address `pt serve` by another DNS name (a reverse proxy forwarding its own
`Host`, a LAN name in `PTASK_SYNC_URL`) need that name in
`PTASK_DASH_ALLOWED_HOSTS`. Because the machine APIs and the always-
mounted dashboard use separate auth schemes, non-loopback binds fail closed
unless machine-API auth (`PTASK_API_TOKEN` or a named token) and
`PTASK_DASH_PASS` are both set, and a non-loopback listener never serves
anonymous callers, even after its last named token is revoked.
`PTASK_ALLOW_UNAUTHENTICATED=1` is an explicit test-only override for isolated
deployments.

Machine-API auth (the env token or a named token) gates `POST /sync`, `POST /capture`,
`POST /capture/resolve`, `POST /email`, `POST /tg/callback`, and the read APIs
(`GET /next`, `GET /detail/{uuid}`, `GET /resolve`, `GET /list`,
`GET /metrics`). The `/mcp` mount separately requires a non-revoked named
`hal` token with write scope; the legacy environment token is not accepted
there. `/healthz`, `/version`, and HMAC-verified git webhooks do not use the
machine-API bearer token.

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
| `commands[].uuid` | client-generated, idempotency key, scoped to the authenticated client. Replaying the *same* command (same `type`, `args` and `temp_id`) returns `"ok"` (with its `temp_id_mapping`) without re-applying; the same uuid with a different type, args or temp_id is a per-command error and nothing is applied. A uuid starting with `capture` (any case) is reserved for the capture lane and is a per-command error. |
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
  "temp_id_mapping": { "<temp_id>": "<real-task-uuid>" },
  "deleted_task_uuids": ["<task-uuid>", ...]
}
```

- `resources.tasks` carries the delta: full task set on full sync,
  changed-since-sync_token on incremental.
- `deleted_task_uuids` are tombstones: tasks hard-deleted (`task_delete`,
  `pt delete`) since `sync_token`. Drop them from the local copy. Always
  empty on a full sync, whose task set replaces client state wholesale.
- `sync_token` is the new monotonic cursor (current `pt_event_log.id`).

### Commands

| `type` | `args` | Side effects |
|---|---|---|
| `task_create` | `{ text, source_type? }` | runs quick-add parser, inserts to `tasks` + `pt_extensions`, optional `pt_recurrence`. |
| `task_done` | `{ task_uuid }` or `{ pt_id }`, optional `expected_deadline` | flips status to `done` or advances recurrence in-place, logs an `interaction` row. A done task is refused. With `expected_deadline` (the deadline the client last saw; `""` = none) the command fails, changing nothing, if the task has moved on — so two queued completions of one occurrence never advance a recurring task twice. Omitted, the current occurrence completes as before. |
| `task_priority` (v1.8.0) | `{ task_uuid \| pt_id, priority }` | sets priority (1..=5), logs a `priority_change` interaction, rescores. |
| `task_edit` (v1.8.0) | `{ task_uuid \| pt_id, deadline }` | sets the deadline (ISO string) or clears it (JSON `null`); other JSON types or an omitted deadline are rejected without mutation; rescores. |
| `task_reopen` (v1.8.0) | `{ task_uuid \| pt_id }` | flips a done/dismissed task back to `pending` (logs the neglect-score reopen signal). |
| `task_retext` (v1.9.0) | `{ task_uuid \| pt_id, title?, description? }` | replaces the title and/or description (at least one required). |
| `task_dismiss` (v1.10.0) | `{ task_uuid \| pt_id }` | soft-closes a task (`status → dismissed`); reversible via `task_reopen`. |
| `task_start` (v1.10.0) | `{ task_uuid \| pt_id }` | `status → in_progress`. |
| `task_snooze` (v1.10.0) | `{ task_uuid \| pt_id, until }` | snoozes until the ISO `until`. |
| `task_depend` (v1.10.0) | `{ task_uuid \| pt_id, on, clear? }` | adds (or with `clear: true` removes) a `depends_on` edge to the `on` query (PT-N or title); cycles are rejected. |
| `task_delete` (v1.10.0) | `{ task_uuid \| pt_id }` | **hard-deletes** the task row and its side-table rows (labels, links, recurrence, `interactions` history) — not reversible. A `task.deleted` tombstone stays in `pt_event_log` and reaches other clients as `deleted_task_uuids`. Use `task_dismiss` for a reversible close. |

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
subject/body into one `raw_items` row with `source_type="email"`. Bodies over
2 MiB get 413. An embedded message in base64 or quoted-printable (which RFC
2046 forbids but Exchange-style gateways send) is decoded and checked like
any other; more than 2 such encoded layers, or embedded messages nested more
than 32 deep counting decoded ones, get 400 and nothing is stored. At most 4
messages are parsed at once; beyond that the answer is 503 with
`Retry-After: 5`. Returns:

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

One delivery closes at most 20 distinct PT-N; the rest are counted in
`skipped_count`, the first 100 of them are listed under `skipped`, and the
count is logged. `PTASK_GIT_CLOSE_REPOS=owner/repo,...`
limits which repositories (`repository.full_name`, case-insensitive) may
close tasks at all; a push from any other repository gets 200 with
`skipped_repo` and closes nothing. Unset, any repository holding the secret
may close tasks.

## Outbound webhooks

Configure `PTASK_WEBHOOK_URLS=<url1>,<url2>` and `PTASK_WEBHOOK_SECRET` for HMAC-signed POSTs
of the events produced by applied `/sync` commands (`task.created`,
`task.completed`, `task.recurrence_advanced`, `task.updated`,
`task.deleted`, ...; replays are not re-sent) and by `/webhook/{gitea,github}`
auto-closes. Nothing else is pushed: writes from the CLI, TUI, bot,
dashboard, MCP mount, timers and other routes reach `pt_event_log` (and so
`/sync` deltas) but not the webhook. Treat a push as a hint and `/sync` as
the complete feed.
Logged to `pt_webhook_log`. Signature header: `X-Ptask-Signature: sha256=<hex>`.

Body:

```json
{ "event_type": "task.created", "task_uuid": "<uuid>", "payload": { ... },
  "ts": "2026-10-06T14:03:11.512000+01:00", "event_id": 4711 }
```

`ts` is the event's commit time (its `pt_event_log.ts`, operator timezone)
and `event_id` its journal id, not the delivery time. `/sync` commands and
git-webhook closes commit and enqueue under one process-wide lock, so events
are enqueued in commit order. Each URL has its own worker that delivers its
events one at a time in that order, so a slow or dead subscriber delays only
itself; no retries, 10s timeout per POST. Each URL's backlog is capped at
10,000 events: past that, new events for that URL are dropped, logged and
counted in `pt_webhook_dropped_total`. On graceful shutdown (SIGTERM /
SIGINT) the server finishes in-flight requests (up to 10s) and then gives the
queued events up to 15s to go out; whatever is left after that is dropped
(and logged).

## Metrics

`/metrics` exposes these series. All but `pt_webhook_dropped_total` (an
in-process counter, reset on restart) are computed from the database at
scrape time:

| Metric | Type | Labels |
|---|---|---|
| `pt_tasks_total` | gauge | `status` |
| `pt_tasks_priority_total` | gauge | `priority` |
| `pt_raw_items_unprocessed` | gauge | — |
| `pt_views_total` | gauge | — |
| `pt_event_log_cursor` | gauge | — (highest `pt_event_log.id`, the sync cursor) |
| `pt_webhook_log_total` | gauge | `direction` (`in` / `out`) |
| `pt_recurrence_total` | gauge | — |
| `pt_distill_last_success_age_seconds` | gauge | — (`-1` = never ran) |
| `pt_distill_failed_total` | gauge | — |
| `pt_distill_last_run_ok` | gauge | — (`1` ok / `0` failed) |
| `pt_distill_quarantined_captures` | gauge | — |
| `pt_notifications_last_sent_age_seconds` | gauge | `channel` |
| `pt_webhook_dropped_total` | counter | — (outbound events dropped on a full per-URL backlog) |

## Dashboard surface (v2.3.0)

The Triage Cockpit's API also lives in `pt serve`. HTTP **Basic** auth
(`PTASK_DASH_USER`/`PTASK_DASH_PASS`; open when no password configured —
local/dev only). Same shapes as the sidecar v0.6.0 contract. While no password
is configured, these routes (and `GET /`) answer only to the server's own names
— IP literals, `localhost`, the machine's short hostname, `*.ts.net`,
`PTASK_DASH_ALLOWED_HOSTS` (`.suffix` entries match the suffix) and the host of
`PTASK_DASH_URL` — and refuse any other `Host` with 421. Together with the
Host check on anonymous machine-API access (see Auth), a DNS-rebinding page
cannot drive the server.

The Python sidecar in `dashboard/` (the live tailnet cockpit) is a different
process with a different posture: no login at all since PT-2201, the same Host
check on every request, and approval decisions gated by
`PTASK_DASH_DECIDE_TOKEN` (see `dashboard/README.md`).

Reads: `GET /api/stats` (its `flux.by_window.<w>.by_actor` lists created,
done, dismissed, reopened and net per actor, as `pt flux` — real
open↔closed transitions; deleting an open task is a closure; v3.46.0) ·
`/api/tasks?status=&limit= · /api/critical?limit= ·
/api/timeline · /api/heatmap · /api/tasks/{id}/events` (journal history) ·
`GET /api/stream` (SSE, `event: change` frames with journal deltas).
`GET /` serves the cockpit when `PTASK_DASH_WWW` exists, else the banner.

Writes (attributed `actor=dashboard`): `POST /api/tasks` (create, quick-add
tokens parse) · `POST /api/tasks/{id}/done|dismiss|reopen` ·
`/{id}/snooze {days}` · `/{id}/priority {level}` ·
`/{id}/edit {title?,description?,priority?,deadline?|null}`.
`POST /api/voice` and `POST /api/voice/task` proxy to the Python voice shim
(`PTASK_VOICE_SHIM_URL`, default http://127.0.0.1:9510) — the first returns
drafted fields for the composer to review, the second creates the task
outright for the cockpit's capture bar. Two routes rather than one flag: the
proxy forwards only the path it is given, never the query string.

## POST /tg/callback (v2.2.0)

Executes a Telegram inline-button tap forwarded by nexus (the bot's single
`getUpdates` owner). Requires `write` scope. Only a client named in
`PTASK_TG_FORWARDERS` (default `nexus`) acts as the operator's tap; any other
write client's task tap is journaled as `telegram via <client_id>`, and its
approval taps get 403.

```json
{"data": "ptdone:<task-uuid>", "callback_id": "<telegram callback id>"}
```

Verbs: `ptdone` | `ptsnooze` (3 days) | `ptdismiss`. Idempotent per
`callback_id` (journal uuid `tg-cb:<id>`); duplicate taps return
`{"ok":true,"duplicate":true}`. Forwarded actions land in the journal as
`actor=telegram`, `source=tg-callback`. Approval verbs (`ptapprove:AP-n`,
`ptreject:AP-n`) are refused with 403 unless `PTASK_TG_APPROVAL_BUTTONS=1`; see
`docs/approvals.md`.

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
