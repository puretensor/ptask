# Approval inbox

One queue for everything waiting on the operator. Agents **request**; only the
operator **decides**; an approval binds to an exact payload that pTask itself
stores and hashes; executors **consume** an approval exactly once.

Human ids are `AP-<n>`, minted from `pt_counters.approval_id` the same way
tasks mint `PT-<n>`.

## The model

| Role | Who | What they may do |
|---|---|---|
| Requester | any write-capable actor (`$PTASK_ACTOR`, HTTP `client_id`, MCP actor) | `request`, `withdraw` (own pending rows), `list`/`show` |
| Operator | a human at a TTY, the dashboard sidecar holding `PTASK_DASH_DECIDE_TOKEN`, Telegram (operator chat, with `PTASK_TG_APPROVAL_BUTTONS=1`), or an **admin** HTTP token | `approve` / `reject` / `decide`, `payload --any-status` (inspect) |
| Executor | a script or agent holding the approved bytes | `payload` → act → `consume` |

A request is pending until it is approved, rejected, withdrawn, or expired.
Decisions are immutable. The SQLite triggers on `approvals` refuse:

- any change to `digest`, `payload`, or `payload_kind` after insert
- any change to a non-pending row except the one-time consume latch
  (`consumed_at` / `consumed_by` NULL → a value, and only on an **approved** row)

`notified_at` may still be set while the row is pending.

## Payload binding

Exactly one of:

- `--payload-file F` — store the file bytes (max 256 KiB). `payload_name` is
  the basename, `payload_ref` is the path. Larger files error and the message
  tells the caller to use `--digest`.
- `--payload-json J` — parse JSON and store the **canonical** form: keys
  sorted recursively, compact separators, non-ASCII kept as UTF-8, floats
  in Python `repr` form. For every payload pTask accepts this is
  byte-identical to Python
  `json.dumps(v, sort_keys=True, separators=(",", ":"), ensure_ascii=False)`.
  One digest must name one payload, so input that different parsers read
  differently is refused, not normalised: duplicate object keys, integers
  beyond 64 bits, non-integers of magnitude 2^53 or more, number literals
  with more precision than a 64-bit float (`0.1000000000000000000001`), and
  `-0`. Send such values as strings. `payload_json` over HTTP and MCP is
  parsed by the transport first, so there only the number-range rules can
  apply; the strict text rules bind again at `verify`/`consume`, where the
  executor's `--payload-json` is parsed the same way.
- `--digest H` — 64 lowercase hex; nothing is stored (`payload_stored: false`).

`digest` is always SHA-256 of the stored bytes (or the supplied digest when
nothing is stored). The same requester re-requesting the same digest while
their row is still pending gets that row back (partial unique index on
`(lower(requester), digest) WHERE status='pending'`). A different requester
asking for the same payload gets a fresh `AP-n` of their own: dedupe never
hands one actor another actor's approval.

`preview` is rendered by pTask **from the stored payload**, never from the
requester's prose:

- UTF-8 text is shown as-is
- JSON is pretty-printed
- non-UTF-8 → `<binary N bytes>`
- digest-only → a line saying the payload is not stored

Agent prose belongs in `request_note` (`--note` / `--note-file`) and is always
shown labelled as the requester's note.

Text fields are bounded on every surface: `title` 300 characters, the
requester's note and the operator's decision note 16384 characters each,
`payload_name` 255 characters. Longer values are refused, not truncated.

Kinds: `email`, `ebay`, `spend`, `destroy`, `external`, `budget`, `other`.

## Consume

The executor pattern:

```bash
pt approval payload AP-12 > /tmp/letter.html
# send / publish / transfer using those exact bytes
pt approval consume AP-12 --payload-file /tmp/letter.html
```

`payload` releases the bytes only while the approval is in force: approved,
not past `expires_at`, not yet consumed. Otherwise it writes nothing to
stdout and exits 3 (pending), 4 (rejected / withdrawn / expired) or 6
(already consumed), so an executor that skips the status check still cannot
act on bytes the operator never approved. A digest-only request has no bytes
to release (exit 1).

`payload --any-status` is the operator's inspection path (for example a
binary file whose preview is `<binary N bytes>`): it prints the bytes
whatever the status, and is refused when `CLAUDECODE` is set or stdin is
not a TTY. `show`/`approval_status` still carry `preview` at every status,
since that is what the operator decides on.

`verify` is the same check without the latch. Exit codes (both verbs):

| Code | Meaning |
|---|---|
| 0 | approved, digest matches, not yet consumed (consume then latches) |
| 3 | pending |
| 4 | rejected / withdrawn / expired, or approved or pending but past `expires_at` |
| 5 | digest mismatch (consume does **not** latch) |
| 6 | already consumed |
| 1 | anything else (unknown id, bad args) |

Both accept exactly one of `--payload-file` / `--payload-json` / `--digest`,
canonicalised identically to `request`.

## Authority rules

Local CLI (`pt approve` / `pt reject` / `pt approval decide`):

- refused when `CLAUDECODE` is set and non-empty (even with `--via dashboard`)
- refused when stdin is not a TTY unless `--via dashboard`
- `decided_via` is `cli` on a TTY, `dashboard` with `--via dashboard`
- `decided_by` is `$PTASK_ACTOR`
- the requester cannot decide their own request; actor names compare
  trimmed and ASCII case-insensitively (`HAL` is `hal`), here and for
  withdraw-own-rows

HTTP `POST /api/approvals/{id}/decide` requires **admin** scope;
`decided_via=api`. Write-scope tokens may request and withdraw (own rows
only). Read-scope tokens may list and get.

Dashboard sidecar `POST /api/approvals/AP-n/approve|reject` (the cockpit's
inbox) runs `pt approve|reject AP-n --via dashboard`, so it is the one route
that reaches the CLI's decide path without a TTY. The sidecar has no login
(PT-2201) and the tailnet carries the fleet's agents as well as the operator,
so the route requires the `X-PTask-Decide-Token` header to equal
`$PTASK_DASH_DECIDE_TOKEN`. With the variable unset, or shorter than 16
characters, the sidecar refuses every decision (403, `decide_disabled`); a
missing or wrong header gets 403 `decide_token_required`. The cockpit asks for
the token once and keeps it in the browser's localStorage.

Telegram `/tg/callback` verbs `ptapprove:AP-n` / `ptreject:AP-n` are accepted
only when `$PTASK_TG_APPROVAL_BUTTONS=1` (the switch that puts decide buttons on
the pings), the authenticated `client_id` is in `$PTASK_TG_FORWARDERS`
(default `nexus`) **and** `from_id` equals `$PTASK_ACCOUNTABILITY_CHAT_ID`.
Otherwise 403 and no state change. `decided_via=telegram`,
`decided_by=operator@telegram`. Idempotent per `callback_id`. The
`ptdone:` / `ptsnooze:` / `ptdismiss:` verbs are journaled as `telegram` (the
operator's tap) only from a forwarder; any other write client is journaled as
`telegram via <client_id>`.

MCP exposes `approval_request`, `approval_list`, `approval_status`,
`approval_withdraw`. No MCP tool can decide. `pt mcp` without
`$PTASK_ACTOR` requests as `mcp`, not the CLI's default `shell`, so the
operator's own `pt approve` (actor `shell`) is never mistaken for the
requester. Two unconfigured MCP clients share `mcp`; set `PTASK_ACTOR` per
client to tell them apart.

## Notify

A **new** request (not the same requester's idempotent re-request) best-effort pings the
operator Telegram chat: `AP-n`, kind, title, requester, a bounded HTML-escaped
excerpt of `preview`, the requester note labelled as such, a digest prefix,
and an inline **URL** button to `$PTASK_DASH_URL/#approvals` (omitted if
unset). Tap-to-decide callback buttons are added only when
`PTASK_TG_APPROVAL_BUTTONS=1` **and** the message shows the whole payload.
The preview excerpt is 2000 characters (the note 800). A longer preview is
cut, marked **PREVIEW TRUNCATED** with the shown and total character
counts, and gets no decide buttons, so padding cannot push a harmful tail
out of sight of a one-tap approval. Binary and digest-only payloads are
likewise marked "Payload not shown" with no decide buttons. Those requests
are decided from the inbox, the CLI, or an admin token. Send failure never fails the request;
success sets `notified_at`.

`pt approval notify` retries pending rows with `notified_at` NULL. The same
sweep runs from `pt accountability run` so the existing 15-minute timer
retries a failed ping.

A pending request past `expires_at` cannot be decided: `approve`/`reject`
flips it to `expired` and refuses. `pt accountability run` (the 15-minute
timer) sweeps stale rows before pinging, and a re-request of an expired
payload mints a fresh `AP-n` instead of returning the stale one.
`pt approval expire` runs the same sweep on demand (idempotent).

**`expires_at` bounds execution too, not only the decision window.** It is
one deadline for the whole approval: an approval granted five minutes
before its `expires_at` is usable for those five minutes and then dies.
Pick `--expires-in` to cover the time the executor needs after the
decision, not just the time the operator needs to decide. An approved row
whose `expires_at` has passed stays `approved` in the record (decided rows
are frozen), but `payload`, `verify` and `consume` refuse it with exit 4
unless it was already consumed. Ask for a fresh approval instead.

A pending row past `expires_at` that no sweep has flipped yet is reported
the same way (exit 4, "expired"), not as pending (3): it can no longer be
approved. An `expires_at` that does not parse as a timestamp (pTask never
writes one; only damage or a hand edit can) is treated as already past:
the row cannot be decided, released, verified or consumed.

## Surfaces

```bash
pt approval request --kind email --title "Send the Q3 memo" \
    --note-file note.md --payload-file letter.html
pt approval ls
pt approval show AP-12
pt approve AP-12                  # operator TTY
pt reject AP-12 --via dashboard   # dashboard sidecar
pt approval consume AP-12 --payload-file letter.html
```

HTTP: `GET/POST /api/approvals`, `GET /api/approvals/{id}`,
`POST /api/approvals/{id}/withdraw`, `POST /api/approvals/{id}/decide`.

## Threat model

Every pTask writer — CLI, MCP stdio, HTTP tokens, the dashboard sidecar —
runs as **one Unix user** on tensor-core against one SQLite file. These
rules are guardrails against an agent approving its own request by
accident (a `CLAUDECODE` session calling `pt approve`, an MCP tool named
`approve`, a write-scope token hitting `/decide`, a tailnet agent POSTing to
the open dashboard sidecar). They are not a cryptographic boundary. The decide
token lives in the sidecar's environment file, which any process of the same
user can read. Anyone who can write the database, replace the
binary, or steal an admin token can decide. The tamper triggers raise the
cost of a confused-deputy `UPDATE`; they do not stop a process with the
same uid from disabling them.
