# Machine event journal

`GET /api/events?cursor=0&limit=50` accepts any existing machine credential
that authenticates at read scope or above, in `Authorization: Bearer ...` or
`X-PTask-Token`: a named token of any scope (read, capture, write, admin), the
legacy `PTASK_API_TOKEN`, or the Prometheus scraper's `PTASK_METRICS_TOKEN`.
Those credentials can already read `/list` and `/detail`. Unlike local
development query routes, it never permits anonymous access. It does not accept
dashboard Basic authentication. No new credentials are created by this feature.

The reply carries `items`, `count`, `total` (remaining rows in this snapshot),
`next_cursor`, `snapshot_cursor`, `oldest_cursor`, `has_more`, and `resync`.
Continue pagination with `cursor=next_cursor&snapshot=snapshot_cursor`; omit
snapshot when beginning the next poll. Limits are 1–200. Malformed parameters
(a negative cursor, a snapshot below the cursor, a limit outside 1–200, an
unknown query field) return HTTP 400 and never ask for a resync. A cursor or
snapshot past the newest journal row returns HTTP 409 with `resync:true` (see
below). Empty results contain `items:[]`, `count:0`, `total:0`.

Rows include a stable source/event identity, task UUID, actor, observation time,
event type and a sanitized payload. Only status words and integer priorities
(1–5) are exported in the payload. A status is exported when pTask's status
model accepts it (`triage`, `backlog`, `todo`, `in_progress`, `snoozed`, `done`,
`dismissed`, `blocked`, emitted in that canonical spelling) or when it is one of
the legacy column values `pending` and `delayed`. Prompts, notes, descriptions,
command arguments and nested values are never copied into the payload.
`payload_redacted:true` means at least one content field was withheld: a key
outside that allowlist, or an allowlisted key whose value is not exportable.
The journal envelope (`actor`, `source`, `cmd`) and task references
(`task_uuid`, `pt_id`) are not content and do not set it; the row already
carries `actor` and `task_id`. Resolve the task through the existing read API
when more detail is needed.

The sanitizer covers the payload only. The row identifiers are exported as
stored: `actor` is the authenticated client or local user name and can be an
email address, and `event_id` is the journal's idempotency key, which keyed
callers supply and which can carry operational text (a capture key names its
incident, for example). Treat both as identifiers, not as sanitized content.
They are no wider than what a read credential already sees through `/detail`.

The journal is durable; this endpoint does not delete it. If prior rows have
been removed, `resync:true` tells the consumer to refresh authoritative task
state. A cursor beyond the journal after a restore returns HTTP 409 with resync.
Polling may deliver an event more than once after a disconnect; deduplicate by
`source` and `event_id`. One snapshot is read in a database transaction so its
denominator and high-water mark do not change during a page.

Known limitation: restore detection only sees a journal that is still behind
the cursor. `pt_event_log` ids come from SQLite `AUTOINCREMENT`, and a restore
also restores `sqlite_sequence`, so ids above the restore point are issued
again. If new events push the restored journal past a consumer's cursor before
it polls again, the endpoint cannot tell: it answers 200 with `resync:false`,
and the reissued rows at or below that cursor are never delivered. A consumer
that must notice this can keep the `event_id` of the row at its cursor and
re-read that row (`cursor=<cursor-1>&limit=1`) before trusting a new page; a
different `event_id` there means the journal was rewound, so refresh
authoritative task state and restart at `cursor=0`. After any known restore of
the pTask database, reset consumers the same way.
