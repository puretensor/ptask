# pTask Fleet Architecture

## Topology

```
                                ┌─────────────────────────────────────┐
                                │  ┌──────────────────────────────┐   │
       Tailscale (HTTP /sync)──►│  │  tensor-core  (canonical)    │   │
                                │  │  100.121.42.54:9501          │   │
                                │  │  ~/puretensor-tasks/         │   │
                                │  │       tasks.db (204 rows)    │   │
                                │  └──────────┬───────────────────┘   │
                                │             │ Litestream WAL        │
                                │             ▼                       │
                                │  /mnt/ceph-backup/                  │
                                │    ptask-litestream/tasks.db        │
                                │  (CephFS file replica, RPO < 1 min) │
                                └─────────────────────────────────────┘
                                              ▲
                                              │ HTTP /sync
                                              │ (PTASK_SYNC_URL)
            ┌──────────────────────────────────┴───────────────────────────────────┐
            │           │           │           │           │           │
        ┌───┴────┐  ┌───┴────┐  ┌───┴────┐  ┌───┴────┐  ┌───┴────┐  ┌───┴────┐
        │  mon1  │  │  mon2  │  │  mon3  │  │ arx1-4 │  │ fox-n0 │  │ fox-n1 │
        │ client │  │ client │  │ client │  │ client │  │ client │  │ client │
        └────────┘  └────────┘  └────────┘  └────────┘  └────────┘  └────────┘
            (binary installed; timers DISABLED; pt remote → tensor-core:9501)
```

## Canonical-store election

**tensor-core owns `tasks.db`.** Locked at the v1.0 activation
(2026-05-14). The pre-v1.0 plan nominated mon1, but the live DB and
the active timer set always ran on tensor-core; the docs were ahead of
reality. Drivers for keeping it there:

- tensor-core is the bridge node hosting the 2× RTX PRO 6000 Blackwell
  GPUs — it's the de-facto compute centre of the fleet and already runs
  the heaviest distill workloads.
- The four user-mode timers (`ptask-backup`, `ptask-distill`,
  `ptask-accountability`, `ptask-scoring`) plus the new `ptask-serve`
  and `ptask-litestream` services were already there at v1.0
  activation; no migration was needed.
- CephFS is mounted at `/mnt/ceph-backup` on tensor-core, so the
  Litestream replica path is local-filesystem-fast without a network
  hop.
- 24/7 uptime targets — arx nodes run thermal-aware power cycling and
  aren't appropriate for a write canonical.

Re-audit gate: if read pressure on tensor-core ever justifies an
NVMe-headroom move to arx2, the migration is `pt serve` host swap +
DB copy + Tailscale-route flip. Inventory edit + one ansible run
completes the rest of the fleet's reconfiguration.

## Roles

| Role | Where | What's enabled |
|---|---|---|
| **canonical** | `tensor-core` | `tasks.db` + WAL on local NVMe; `ptask-serve` on tensor-core Tailscale `100.121.42.54:9501`; Litestream → CephFS; all four user-mode timers active |
| **client** | `mon1-3`, `arx1-4`, `fox-n0/1` | Same `pt` binary; timers **disabled**; reads/writes route through `pt remote` → `PTASK_SYNC_URL=http://100.121.42.54:9501` |
| **excluded** | `coldiron`, `spore-azure-1`, `spore-gcp-1` | No `pt` install — these are GCP/Azure-ephemeral and shouldn't carry task state. The Ansible inventory deliberately omits them. |
| **retired** | k3s `puretensor-tasks` namespace on mon1 | Was a long-running Python deployment behind `https://tasks.fox/` with its own k3s-PVC-backed DB. Discovered post-v1.0.3 activation; merged into tensor-core via UUID union (132 overlap, 306 mon1-only, 72 tc-only → 510 final). Namespace + IngressRoutes deleted at v1.0.4. |

### Arch / glibc heterogeneity

The fleet isn't uniform — Ansible has per-host overrides:

| Host | Arch / glibc | Binary source |
|---|---|---|
| tensor-core | x86_64, glibc 2.39 (Ubuntu 24.04) | `cargo build --release` on controller |
| mon1, arx1-4, fox-n0/1 | x86_64, glibc 2.39 | Same as controller |
| **mon2** | x86_64, **glibc 2.35** | Build in `ubuntu:22.04` container |
| **mon3** | **aarch64** | Build `--target aarch64-unknown-linux-gnu` |

Use `-e ptask_binary=/path/to/per-host/build` on the per-host Ansible
invocation to override the default per-arch `dist/pt-<target-triple>`
artifact.

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
| `PTASK_SYNC_URL` | `pt remote` (clients) | `http://100.121.42.54:9501`. Set fleet-wide by `/etc/profile.d/ptask.sh`. |
| `PTASK_API_TOKEN` | `pt serve`, `pt remote` | Required for non-loopback `pt serve` binds; clients send it as `Authorization: Bearer`. |
| `PTASK_ALLOW_UNAUTHENTICATED` | `pt serve` | Emergency/test override for unauthenticated non-loopback binds. Do not set in production. |
| `GOOGLE_API_KEY`, `GEMINI_CONSOLIDATE_MODEL` | `pt distill` | Gemini structured-output credentials/model for native classify+consolidate. Missing key = preflight exit 3, fail closed. |
| `PTASK_TELEGRAM_BOT_TOKEN`, `PTASK_ACCOUNTABILITY_CHAT_ID`, `PTASK_SMTP_*` | accountability | Existing v0.7 surface. |
| `PTASK_LITESTREAM_*` | `litestream` | Only consulted by the alternate S3 replica config — unused while the CephFS replica is active. |

## Recovery

- **Lose tensor-core disk, DB intact in CephFS replica:** stop
  `ptask-serve.service` and `ptask-litestream.service` on tensor-core,
  run `litestream restore -config ~/.config/litestream/litestream.yml
  -o ~/puretensor-tasks/tasks.db.restored
  ~/puretensor-tasks/tasks.db`, swap atomically, restart services.
  RPO ≤ 1 min.
- **Lose tensor-core entirely:** promote a different node by editing
  inventory + running ansible, fresh `litestream restore` from the
  CephFS replica into the new host's `~/puretensor-tasks/tasks.db`,
  update `PTASK_SYNC_URL` in `/etc/profile.d/ptask.sh` fleet-wide.
- **Lose CephFS:** nightly Ceph snapshot via `ptask-backup.timer`
  keeps a 30-day file backup independent of Litestream — last-resort
  recovery path.
- **Lose a client node:** no data loss; `pt` config points at the
  canonical URL, local cache is rebuildable on next sync.

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
| v1.0.3 | Doc reality reconciliation (canonical = tensor-core) | post-1.0 |
| **v1.0.4** | **k3s puretensor-tasks namespace retired (DB merged into tensor-core)** | post-1.0 |
