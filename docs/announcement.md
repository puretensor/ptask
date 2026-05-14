# Why we built our own task manager

*Draft — operator to review and publish via the `/bretalon-post` skill.
Tone target: technical reader; voice matched to the operator's existing
Bretalon long-form posts.*

---

## tl;dr

Most software companies put their task manager on someone else's
servers. We didn't. **pTask v1.0.0** is a single-binary Rust task
system that runs entirely on PureTensor infrastructure — no SaaS, no
API keys, no third-party data plane. It replaces a six-month-old
Python pipeline that already worked, because "already works" isn't the
same as "sovereign."

## Why bother

Linear, Todoist, ClickUp, Notion, Asana — every modern task tool
lives on someone else's compute, with someone else's encryption, with
someone else's data-access policies. That's a fine trade-off for a
distributed team that doesn't think about it. It's not the right
trade-off for an infrastructure company whose pitch starts with the
word *sovereign*.

Our existing Python pipeline (`puretensor-tasks`) had been quietly
distilling voice memos, emails, and CC reports into 204 active tasks,
running a six-stage NLP pipeline (SBERT, BERTopic, Gemini Flash), and
serving a small FastAPI dashboard on `:9500`. It worked. But:

- The dashboard was the only surface — no terminal-native flow.
- The DSL was missing: no inline-token quick-add, no Todoist-style
  filter syntax, no natural-language deadlines, no recurrence.
- Tasks had UUIDs only; nothing like `PT-N` for cross-referencing
  from commits, emails, or chat.
- ML stages called Gemini directly with a project-level API key —
  every fleet node that touched the pipeline needed the credential.
- Sub-second response felt like a stretch for a Python + HTMX surface
  on an already-loaded mon node.

pTask v1.0.0 is a complete rewrite that fixes all of the above while
respecting the existing data layout. The same `~/puretensor-tasks/tasks.db`
file keeps its existing tables; Rust adds a few side tables
(`pt_extensions`, `pt_views`, `pt_recurrence`, `pt_event_log`,
`pt_webhook_log`) and takes ownership of one column-set per phase.

## The phased migration

We didn't cut over. We migrated 11 phases, each shippable on its own:

| Phase | What | Tag |
|---|---|---|
| 1 | Workspace, migrations, PT-N minting | v0.1.1 |
| 2 | Quick-add DSL, dates, RFC 5545 recurrence | v0.2.3 |
| 3 | ratatui TUI | v0.3.1 |
| 4 | Axum HTTP `/sync` + Prometheus `/metrics` | v0.4.1 |
| 5 | Telegram bot (teloxide) | v0.5.2 |
| 6 | Email forward + Gitea/GitHub `Fixes PT-N` | v0.6.2 |
| 6.5 | Distill shim — Rust owns the cron + audit log | v0.6.6 |
| 7 | Accountability — escalation state machine + dispatch | v0.7.1 |
| 8 | Scoring — composite priority recompute | v0.8.1 |
| 9 | Native ML modules (SBERT, classifier, dedup, clustering, consolidation, collectors) | v0.9.0 |
| 10 | Fleet rollout kit (releases, ansible, Litestream, remote client) | v0.10.0 |
| 1.0 | Polish (manpage, completions, docs, benches) | v1.0.0 |

Each phase had a verification pass: bit-perfect parity tests against
the Python output (scoring deltas zero across all 14 active tasks;
SBERT embedding cosines `1.0000` to four decimals against
`sentence-transformers`), per-domain rollback steps, and every commit
tagged with a version bump.

## What's interesting

A few decisions worth flagging:

- **HAL as a vendor-shield.** Instead of building Gemini calls into
  pTask, the speech-act classifier and the consolidation prompt route
  through HAL HTTP endpoints. HAL holds the model-routing policy.
  Swapping Gemini for Nemotron-Super, or for a local Llama variant,
  becomes a HAL config change, not a pTask release.

- **In-tree Brandes betweenness.** petgraph 0.8 doesn't ship
  betweenness centrality. NetworkX does, but we wanted parity with
  the Python scoring formula without pulling another dependency. So
  we ported Brandes' algorithm (directed, normalized) into the
  scoring crate. Diamond + sparse-DAG parity tests pin it to four
  decimals against NetworkX output.

- **Connected-components instead of HDBSCAN.** BERTopic uses
  HDBSCAN over UMAP-reduced embeddings. We replaced both with a
  cosine-threshold connected-components pass at our scale (a few
  hundred items per distill cycle). The clusters land in the same
  topical buckets without dragging linfa-clustering into the build
  matrix. ANN reattach is a v1.x lever if we cross 100k items.

- **Litestream + Ceph.** SQLite WAL streams continuously to a Ceph
  rados gateway. RPO under one minute. The DB file lives on mon1
  with `wal_autocheckpoint=0`; Litestream owns the checkpoint
  cadence. Nightly Ceph snapshots stay as the disaster-recovery
  fallback, independent of Litestream.

- **`pt remote`.** Fleet nodes don't carry their own task DB — they
  speak the `/sync` wire protocol against the canonical host
  (`mon1`) over Tailscale. `pt remote add "..."` from any node;
  writes land on mon1. The same quick-add grammar parses on both
  ends.

- **AI-assisted authoring.** The build was paired with Claude Opus
  (Claude Code) on a fast cadence — code on `main`, review on
  separate Codex passes. Phase boundaries became review boundaries;
  each `v0.N.0` tag closes after the operator merges the Codex
  improvement PR for that phase. The Codex PR for Phase 7
  (accountability dry-run + budget gating fixes) is a good example
  of how the model + agent split actually works in practice.

## What's deferred to v1.x.x

A handful of carryovers are deliberately post-launch:

- TUI discrete edit verbs (`r`/`d`/`l` single-key) and `gt`/`gi`
  triage shortcuts.
- Structured `#[instrument]` tracing spans on every HTTP handler +
  DB write.
- In-process counter metrics (gauges already ship via `/metrics`).
- Telegram `/snooze` + `/defer` handlers.
- `in_progress` status transitions on branch/PR-creation webhook
  events (currently only the merge event flips status).
- HAL `/compose-nudge` endpoint (the pTask side reads
  `PTASK_HAL_NUDGE_URL` today; HAL repo needs the matching route).

## Numbers

- **259 tests** at v1.0.0.
- **5 crates** in the workspace (`ptask-core`, `ptask-cli`,
  `ptask-server`, `ptask-tui`, `ptask-bot`, `ptask-distill`).
- **14 MB** stripped Linux x86_64 release binary (glibc, x86-64-v3
  baseline). Static-musl target queued.
- **Bit-perfect scoring parity** (max-delta 0 across all four
  `score_*` columns + `priority_score`) against
  `~/puretensor-tasks/api/scoring.py`.
- **Bit-perfect SBERT parity** (cosine 1.0000, max-abs-delta 0.0)
  against `sentence_transformers.encode(normalize_embeddings=True)`.

## Where the code lives

- Public: <https://github.com/puretensor/ptask>
- Internal mirror: `ssh://git@100.92.245.5:2222/puretensor/ptask.git`
- Releases: <https://github.com/puretensor/ptask/releases>

Single binary, MIT license, edition 2024.

## Closing

We could have kept the Python pipeline running. Nothing was on fire.
But "nothing is on fire" isn't a competitive advantage — and a task
system that touches voice memos, calendar, email, git, and Telegram
is exactly the kind of surface that should be operator-owned
end-to-end. v1.0.0 ships that surface.

*pTask v1.0.0 — single binary, no API keys, no SaaS, no third-party
data plane.*
