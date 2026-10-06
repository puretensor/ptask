# Approval inbox

One queue for everything waiting on the operator. Agents **request**; only the
operator **decides**; an approval binds to an exact payload that pTask itself
stores and hashes; executors **consume** an approval exactly once.

Human ids are `AP-<n>`, minted from `pt_counters.approval_id` the same way
tasks mint `PT-<n>`.

## The model

| Role | Who | What they may do |
|---|---|---|
| Requester | any write-capable actor (`$PTASK_ACTOR`, HTTP `client_id`, MCP actor) | `request`, `withdraw` (own pending rows), `list`/`show`/`payload` |
| Operator | a human at a TTY, the dashboard sidecar holding `PTASK_DASH_DECIDE_TOKEN`, Telegram (operator chat, with `PTASK_TG_APPROVAL_BUTTONS=1`), or an **admin** HTTP token | `approve` / `reject` / `decide` |
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
  sorted recursively, compact separators, non-ASCII kept as UTF-8 (the
  `serde_json` compact encoding of a key-sorted value).
- `--digest H` — 64 lowercase hex; nothing is stored (`payload_stored: false`).

`digest` is always SHA-256 of the stored bytes (or the supplied digest when
nothing is stored). Re-requesting the same digest while a row is still
pending is idempotent (partial unique index on `digest WHERE status='pending'`).

`preview` is rendered by pTask **from the stored payload**, never from the
requester's prose:

- UTF-8 text is shown as-is
- JSON is pretty-printed
- non-UTF-8 → `<binary N bytes>`
- digest-only → a line saying the payload is not stored

Agent prose belongs in `request_note` (`--note` / `--note-file`) and is always
shown labelled as the requester's note.

Kinds: `email`, `ebay`, `spend`, `destroy`, `external`, `budget`, `other`.

## Consume

The executor pattern:

```bash
pt approval payload AP-12 > /tmp/letter.html
# send / publish / transfer using those exact bytes
pt approval consume AP-12 --payload-file /tmp/letter.html
```

`verify` is the same check without the latch. Exit codes (both verbs):

| Code | Meaning |
|---|---|
| 0 | approved, digest matches, not yet consumed (consume then latches) |
| 3 | pending |
| 4 | rejected / withdrawn / expired |
| 5 | digest mismatch (consume does **not** latch) |
| 6 | already consumed |
| 1 | anything else (unknown id, bad args) |

`pt approve` (and `pt approval decide … approve`) exits 7 when it refuses a
flagged payload preview without `--force` (see Authority rules). Approving
or rejecting a request that is no longer pending exits 4 (its terminal state
is named); the `--force` check only applies to a pending request.

Both accept exactly one of `--payload-file` / `--payload-json` / `--digest`,
canonicalised identically to `request`.

## Authority rules

Local CLI (`pt approve` / `pt reject` / `pt approval decide`):

- refused when `CLAUDECODE` is set and non-empty (even with `--via dashboard`)
- refused when stdin is not a TTY unless `--via dashboard`
- `decided_via` is `cli` on a TTY, `dashboard` with `--via dashboard`
- `decided_by` is `$PTASK_ACTOR`
- the requester cannot decide their own request
- approve (not reject) refuses without `--force` when the payload preview
  holds control, bidi or invisible characters (a zero-width space in an
  address, tag characters after an amount): the digest binds the stored
  bytes, not what a screen shows. `pt approval show` marks them as U+FFFD
  under a warning; inspect the exact bytes with
  `pt approval payload AP-n | cat -v`, then `pt approve AP-n --force`. The
  refusal exits **7**. The dashboard sidecar path (`--via dashboard`) is
  gated the same way and never forces: the sidecar answers 409
  `{"code": "payload_flagged"}` with pt's message plus "approve from the CLI
  with `pt approve AP-n --force`", and the cockpit shows that as its toast.
  The check only applies to a pending request.
- what counts: control characters other than LF/tab/CRLF, bidi controls,
  every Unicode format (Cf) and Default_Ignorable code point — **including
  a ZWJ (U+200D) and a soft hyphen (U+00AD)**, the Khmer inherent vowels,
  unassigned U+FFF0–FFF8 and the whole U+E0000–E0FFF block — the
  line/paragraph separators, the blank-rendering braille blank (U+2800) and
  ideographic space (U+3000), and variation selectors (U+FE00–FE0F, U+E0100–E01EF, the
  "emoji smuggling" carrier) **except** exactly one VS15/VS16 directly after
  a pictograph (real emoji, not enclosed alphanumerics or plain arrows) or
  in a keycap — and at most 8 of those per payload: each optional selector
  can carry about 1.6 bits that render identically, so a 9th flags it. So a lone VS16 emoji such as ❤️, ☀️ or ✔️ in
  an email body needs no `--force`, while a second selector in a run, any
  other selector, a ZWJ sequence such as a family emoji (👨‍👩‍👧) — the
  preview never keeps joiners — or a soft hyphen still does.

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
`approval_withdraw`. No MCP tool can decide.

## Notify

A **new** request (not an idempotent re-request) best-effort pings the
operator Telegram chat: `AP-n`, kind, title, requester, a bounded HTML-escaped
excerpt of `preview`, the requester note labelled as such, a digest prefix,
and an inline **URL** button to `$PTASK_DASH_URL/#approvals` (omitted if
unset). Tap-to-decide callback buttons are added only when
`PTASK_TG_APPROVAL_BUTTONS=1`. Send failure never fails the request;
success sets `notified_at`.

`pt approval notify` retries pending rows with `notified_at` NULL. The same
sweep runs from `pt accountability run` so the existing 15-minute timer
retries a failed ping.

A pending request past `expires_at` cannot be decided: `approve`/`reject`
flips it to `expired` and refuses. `pt accountability run` (the 15-minute
timer) sweeps stale rows before pinging, and a re-request of an expired
payload mints a fresh `AP-n` instead of returning the stale one.
`pt approval expire` runs the same sweep on demand (idempotent).

## Surfaces

```bash
pt approval request --kind email --title "Send the Q3 memo" \
    --note-file note.md --payload-file letter.html
pt approval ls
pt approval show AP-12
pt approve AP-12                  # operator TTY
pt approve AP-12 --force          # after inspecting a flagged payload
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
