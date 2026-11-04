# Machine event journal

`GET /api/events?cursor=0&limit=50` accepts an existing read, write or admin
machine token in `Authorization: Bearer ...` or `X-PTask-Token`. Unlike local
development query routes, it never permits anonymous access. It does not accept
dashboard Basic authentication. No new credentials are created by this feature.

The reply carries `items`, `count`, `total` (remaining rows in this snapshot),
`next_cursor`, `snapshot_cursor`, `oldest_cursor`, `has_more`, and `resync`.
Continue pagination with `cursor=next_cursor&snapshot=snapshot_cursor`; omit
snapshot when beginning the next poll. Limits are 1–200. Negative/ahead cursors,
invalid limits, and unknown query fields fail explicitly. Empty results contain
`items:[]`, `count:0`, `total:0`.

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

The journal is durable; this endpoint does not delete it. If prior rows have
been removed, `resync:true` tells the consumer to refresh authoritative task
state. A cursor beyond the journal after a restore returns HTTP 409 with resync.
Polling may deliver an event more than once after a disconnect; deduplicate by
`source` and `event_id`. One snapshot is read in a database transaction so its
denominator and high-water mark do not change during a page.
