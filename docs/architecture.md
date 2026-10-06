# pTask Architecture

pTask is designed for a single canonical SQLite store plus optional
read/write clients. One host owns `tasks.db` and runs `pt serve`; other
hosts install the same `pt` binary, keep background timers disabled, and
talk to the canonical API through `pt remote`.

## Topology

```
                    ┌─────────────────────────────────┐
   HTTP /sync  ───► │  canonical host                 │
                    │  pt serve  (PTASK_SYNC_URL)     │
                    │  ~/puretensor-tasks/tasks.db    │
                    │           │                     │
                    │           ▼ Litestream WAL      │
                    │  replica path (local FS or S3)  │
                    └─────────────────────────────────┘
                                   ▲
                                   │ HTTP /sync
                    ┌──────────────┴──────────────┐
                    │  client hosts               │
                    │  same `pt` binary           │
                    │  timers disabled            │
                    │  PTASK_SYNC_URL → canonical │
                    └─────────────────────────────┘
```

## Canonical store

Pick one always-on host as canonical. That host:

- Keeps `tasks.db` on local disk (NVMe preferred).
- Runs `ptask-serve`, backup, distill, accountability, scoring, and Litestream.
- Replicates WAL to a durable replica (filesystem or S3-compatible) with RPO under one minute.

Clients never own the write database. Ephemeral cloud nodes should stay out of
the inventory so they do not hold task state.

### Multi-arch builds

Ansible accepts a per-host `ptask_arch_override` when glibc or CPU
architecture differs from the controller. Override the artifact with
`-e ptask_binary=/path/to/per-host/build`. Example triples:

| Target | Typical build |
|---|---|
| controller (x86_64, current glibc) | `cargo build --release --bin pt --features native-ml --locked` |
| older glibc | build inside an `ubuntu:22.04` container |
| aarch64 | add `--target aarch64-unknown-linux-gnu` |

## Schema v2 (v2.0.0)

V010 merged `pt_extensions` into `tasks` (the side table survives as a
compat VIEW for the dashboard sidecar until Phase 7), introduced the
8-state `status_v2` model (legacy `tasks.status` is maintained in sync for
dashboard/accountability compatibility),
`task_links` (depends_on/blocks/discovered_from/subtask_of),
`task_labels`, `due_at`/`snoozed_until`/`parent_uuid`, one-way folded
`interactions` into `pt_event_log`, UTC-normalized timestamps, and FTS5
(`tasks_fts`). Subtask JSON of non-terminal parents was promoted to real
child rows by the one-shot Rust converter in `pt backfill`.

## Environment

Since v1.16.0 the environment is read exactly once per process, at the
binary entrypoint, via `ptask_core::config::Config::from_env()`; library
code (auth, storage, accountability, webhooks, distill) receives injected
config and never touches `std::env`. Network dispatch (Telegram/SMTP/HAL)
lives in the `ptask-notify` crate behind the `Dispatch` trait — ptask-core
carries no HTTP/TLS/executor dependencies.

| Variable | Used by | Purpose |
|---|---|---|
| `PTASK_DB` | `pt` (canonical) | Override the SQLite path. Defaults to `~/puretensor-tasks/tasks.db`. |
| `PTASK_SYNC_URL` | `pt remote` (clients) | Canonical `pt serve` URL, for example `http://ptask.example:9501`. |
| `PTASK_API_TOKEN` | `pt serve`, `pt remote` | Legacy machine-API bearer credential. Non-loopback `pt serve` binds need it or an active named token, together with `PTASK_DASH_PASS`. |
| `PTASK_DASH_USER`, `PTASK_DASH_PASS` | `pt serve` cockpit | Dashboard Basic auth. The password is required together with `PTASK_API_TOKEN` (or an active named token) for non-loopback binds. The Python sidecar in `dashboard/` ignores both: it has no login and is reachable from the whole tailnet. |
| `PTASK_DASH_ALLOWED_HOSTS` | `pt serve` cockpit, dashboard sidecar | Extra `Host` names (`.suffix` entries match the suffix) answered besides IP literals, `localhost`, the machine's short hostname and `*.ts.net` (the DNS-rebinding guard; `pt serve` applies it to its dashboard only while no dashboard password is set, and always to anonymous machine-API access). |
| `PTASK_DASH_DECIDE_TOKEN` | dashboard sidecar | Secret (≥ 16 characters) the cockpit must send to approve or reject; unset disables approval decisions on the sidecar. |
| `PTASK_DASH_FRAME_ANCESTOR` | `pt serve` cockpit | Optional single HTTPS origin allowed to frame dashboard documents through CSP. Unset or invalid values fail closed with `X-Frame-Options: DENY`. |
| `PTASK_ALLOW_UNAUTHENTICATED` | `pt serve` | Emergency/test override for unauthenticated non-loopback binds. Do not set in production. |
| `PTASK_LLM_BACKEND` | `pt distill` | LLM provider for native classify+consolidate: `local` (default) or `gemini`. |
| `LOCAL_LLM_URL`, `LOCAL_LLM_MODEL` | `pt distill` | `local` backend: OpenAI-compatible endpoint (default `http://127.0.0.1:8600/v1`) and model (default `nemotron-lightning`). A failed preflight records `distill.failed` and exits 1, nothing consumed. |
| `GOOGLE_API_KEY`, `GEMINI_CONSOLIDATE_MODEL` | `pt distill` | `gemini` backend: credential (required there; missing = exit 3 before anything is consumed) and model (default `gemini-3.5-flash`). |
| `PTASK_TELEGRAM_BOT_TOKEN`, `PTASK_ACCOUNTABILITY_CHAT_ID`, `PTASK_SMTP_*` | accountability | Existing v0.7 surface. |
| `PTASK_LITESTREAM_*` | `litestream` | Used only by the optional S3 replica config. |

## Recovery

- **Lose canonical disk, replica intact:** `litestream restore` to a scratch
  file, verify it, then put it live with
  [Promote a restored copy over the live DB](operations.md#promote-a-restored-copy-over-the-live-db):
  stop every pTask unit (dashboard, `ptask-serve`, `ptask-litestream`, the
  timers), confirm nothing holds the files, move `tasks.db` and its
  `-wal`/`-shm` aside, install and check the restored file, restart in order.
  A rename alone is not enough: a stale `-wal` next to the new file corrupts
  or reverts it. RPO is the Litestream interval (default under one minute).
- **Lose the canonical host:** promote a client: restore from the replica,
  install it with the same procedure, update inventory, point
  `PTASK_SYNC_URL` at the new host.
- **Lose the replica:** the nightly `ptask-backup.timer` snapshot is the
  last-resort path (default 30-day retention), promoted the same way.
- **Lose a client node:** no data loss; clients rebuild from the canonical
  URL.

## What ships when

| Tag | What | Phase |
|---|---|---|
| v0.9.0 | All native ML modules in tree | 9 close |
| v0.9.1 | `cargo build --release` release pipeline | 10 |
| v0.9.2 | Ansible playbook + inventory | 10 |
| v0.9.3 | Architecture doc + canonical-store nomination | 10 |
| v0.9.4 | Litestream config + runbook | 10 |
| v0.9.5 | `pt remote` client mode | 10 |
| v0.10.0 | Phase 10 close — fleet kit | 10 close |
| v1.0.0 | Phase 1.0 close — polish | 1.0 close |
| v1.0.2 | DB-free client dispatch + temp_id + lint fix | post-1.0 |
| v1.0.3 | Canonical-store documentation | post-1.0 |
| **v1.0.4** | Retired the parallel k3s task deployment; single canonical DB | post-1.0 |
