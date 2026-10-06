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
  with more than 17 significant digits (`0.1000000000000000000001`), a
  nonzero literal that underflows to zero (`1e-400`), and `-0`. Send such
  values as strings. Round-tripping literals that are not the shortest form
  (`0.10000000000000001`, as C `%.17g`, jq 1.6 or Postgres
  `extra_float_digits=3` print them) are accepted and stored in Python's
  shortest form (`0.1`). `payload_json` over HTTP and MCP is parsed by the
  transport first, so there only the number-range rules can apply; the
  strict text rules bind again at `verify`/`consume`, where the executor's
  `--payload-json` is parsed the same way.

  **JSON approvals stored before strict canonicalisation shipped** used the old
  form (ryu float digits, no number limits). Re-canonicalising the
  executor's JSON can disagree with them: a float of magnitude 2^53 or more
  (`1e16`) is now refused, so `verify`/`consume --payload-json` exits **1**;
  an exponent-range float (old `1e-5`, now `1e-05`) or a 17-digit float the
  old parser rounded differently gives a digest mismatch, exit **5**. For
  such rows fetch the stored bytes and bind to them instead:
  `pt approval payload AP-n > p.json` then `pt approval consume AP-n
  --payload-file p.json`.
- `--digest H` — 64 lowercase hex; nothing is stored (`payload_stored: false`).

`digest` is always SHA-256 of the stored bytes (or the supplied digest when
nothing is stored). The same requester re-requesting the same digest while
their row is still pending gets that row back (partial unique index on
`(lower(requester), digest) WHERE status='pending'`). A different requester
asking for the same payload gets a fresh `AP-n` of their own: dedupe never
hands one actor another actor's approval.

A re-request returns the existing row **unchanged**: a new `--expires-in`,
title, note, kind or task is not applied (expiry is never moved silently).
The response carries `"deduplicated": true` and, when something requested
was not applied, a `"notice"` naming it (`… not applied: expires_in "3h"
(kept expires_at …), title`); the CLI also prints the notice on stderr, and
HTTP answers 200 instead of 201. To change those fields, withdraw the
request and request again.

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

`payload` releases the bytes only while the approval is **in force**:
approved, not past `expires_at`, not yet consumed. Otherwise it writes
nothing to stdout and exits 3 (pending), 4 (rejected / withdrawn / expired)
or 6 (already consumed). A digest-only request has no bytes to release
(exit 1).

Every approval JSON object (`show`/`ls --json`, HTTP, MCP) carries
`"in_force": true|false`, the same verdict computed at read time. Pollers
should wait on `in_force`, not on `status`: `status` stays `approved` after
the approval expires and after it is consumed.

**What this gate is, and is not.** It is a correctness guard for executors
that follow the documented `payload` → act → `consume` pattern: such an
executor cannot act on bytes the operator never approved, even if it forgets
to check the status. It is **not** a confidentiality boundary, and it does
not withhold the bytes from an agent that wants them:

- `preview` is the full text of any UTF-8 payload (JSON pretty-printed) and
  is on every read surface at every status: `show`/`ls --json`, HTTP
  `GET /api/approvals[/{id}]`, MCP `approval_status`/`approval_list`, and
  the dashboard sidecar's `GET /api/approvals`, which needs no auth since
  PT-2201 (the tailnet is the gate). It must be, since it is what the
  operator decides on.
- `payload --any-status` (the operator's inspection path, for example a
  binary file whose preview is `<binary N bytes>`) prints the bytes
  whatever the status. It is refused when `CLAUDECODE` is set or stdin is
  not a TTY, but that guard is a speed bump against accidents: any process
  can allocate a pseudo-terminal (`script -qc …`) and unset the variable.

Do not put secrets in an approval payload on the assumption that only an
approved executor will see them.

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
Otherwise 403 and no state change. An **approve** tap is also refused (403,
"review in the inbox") when the request is not tap-decidable by the rules
under Notify, whatever ping the tap came from: pings sent by older builds
carried buttons under looser rules. Reject taps stay allowed.
`decided_via=telegram`,
`decided_by=operator@telegram`. Idempotent per `callback_id`. The
`ptdone:` / `ptsnooze:` / `ptdismiss:` verbs are journaled as `telegram` (the
operator's tap) only from a forwarder; any other write client is journaled as
`telegram via <client_id>`.

MCP exposes `approval_request`, `approval_list`, `approval_status`,
`approval_withdraw`. No MCP tool can decide. `pt mcp` takes its actor from
`$PTASK_MCP_ACTOR`, else `$PTASK_ACTOR`, else `mcp` (never the CLI's
default `shell` unless you set it), so the operator's own `pt approve`
(actor `shell`) is not mistaken for the requester. `PTASK_MCP_ACTOR` wins
so that an operator shell exporting `PTASK_ACTOR=shell` does not pass that
identity to an MCP server it launches; set `PTASK_MCP_ACTOR` in each MCP
registration. Two unconfigured MCP clients share `mcp`.

## Notify

A **new** request (not the same requester's idempotent re-request) best-effort pings the
operator Telegram chat: `AP-n`, kind, title, requester, a bounded HTML-escaped
excerpt of `preview`, the requester note labelled as such, a digest prefix,
and an inline **URL** button to `$PTASK_DASH_URL/#approvals` (omitted if
unset). Tap-to-decide callback buttons are added only when
`PTASK_TG_APPROVAL_BUTTONS=1` **and** the message shows the whole payload
faithfully. Budgets are in UTF-16 code units, which is what Telegram's
4096 limit counts (an emoji is 2): preview 2000, note 800, title 200,
requester 100. A longer preview is cut (never inside a surrogate pair),
marked **PREVIEW TRUNCATED** with the shown and total unit counts, and gets
no decide buttons, so padding cannot push a harmful tail out of sight of a
one-tap approval. Binary and digest-only payloads are marked "Payload not
shown"; a preview containing bidi embeddings, overrides or isolates
(U+202A–202E, U+2066–2069), zero-width / default-ignorable characters, or
a control character other than a newline, tab or CRLF (a lone CR or a
backspace can overwrite what is shown) is marked as containing invisible or
direction-changing characters. Neither gets decide buttons. This is
deliberately conservative: emoji that use a joiner (👨‍👩‍👧) or a variation
selector (❤️, ✔️, keycaps) also lose tap-to-decide. If a message would still exceed 4096 units, the note
is dropped first. Those requests are decided from the inbox, the CLI, or
an admin token. Send failure never fails the request; success sets
`notified_at`.

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
same uid from disabling them. Payload bytes are not secret from any of
these writers either: see "What this gate is, and is not" under Consume.
