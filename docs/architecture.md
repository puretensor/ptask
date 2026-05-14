# pTask Fleet Architecture

## Topology

```
                                     ┌─────────────────────────────┐
                                     │  ┌──────────────────────┐   │
              Tailscale ACL (write)──┤  │   mon1  (canonical)  │   │
              ─────────────────────► │  │  ~/puretensor-tasks/ │   │
                                     │  │       tasks.db       │   │
                                     │  └──────────┬───────────┘   │
                                     │             │ Litestream    │
                                     │             ▼ (v0.9.4)      │
                                     │   ceph://ptask-wal/         │
                                     │   (RPO < 1 min)             │
                                     └─────────────────────────────┘
                                                  ▲
                                                  │ HTTP /sync
                                                  │ (v0.9.5)
            ┌─────────────────────────────────────┴──────────────────────────────────┐
            │           │           │           │           │           │           │
        ┌───┴────┐  ┌───┴────┐  ┌───┴────┐  ┌───┴────┐  ┌───┴────┐  ┌───┴────┐
        │  mon2  │  │  mon3  │  │  arx1  │  │ fox-n0 │  │ fox-n1 │  │tens-core│
        │ client │  │ client │  │ client │  │ client │  │ client │  │ client  │
        └────────┘  └────────┘  └────────┘  └────────┘  └────────┘  └────────┘
            (binary installed; timers DISABLED until v0.9.5 wires the proxy)
```

## Canonical-store election (v0.9.3)

**mon1 owns `tasks.db`.** Locked here as of v0.9.3 unless v0.10 audit
shows mon1 read pressure justifying arx2 (a stronger machine with more
NVMe headroom). Drivers for the choice:

- mon1 already hosts the monitoring plane (Grafana, Prometheus, Loki)
  and is the natural source-of-truth node for cross-fleet observability.
- It runs the existing `ptask-backup.timer` to Ceph; co-locating the DB
  with its backup target keeps the failure modes simple.
- The user-mode systemd timers (`ptask-backup`, `ptask-distill`,
  `ptask-accountability`, `ptask-scoring`) all live there today.
- mon1 has 24/7 uptime targets — arx nodes run thermal-aware power
  cycling and aren't appropriate for a write canonical.

## Roles

| Role | Where | What's enabled |
|---|---|---|
| **canonical** | `mon1` | `tasks.db` + WAL on local NVMe; all four user-mode timers active; Litestream replication to Ceph (v0.9.4) |
| **client** | `mon2/3`, `arx1-4`, `fox-n0/1`, `tensor-core` | Same `pt` binary; timers **disabled**; reads/writes route through `https://ptask.ts.puretensor.local/sync` (v0.9.5) |
| **excluded** | `coldiron`, `spore-azure-1`, `spore-gcp-1` | No `pt` install — these are GCP/Azure-ephemeral and shouldn't carry task state. The Ansible inventory deliberately omits them. |

## Phase-9 vs Phase-10 separation

- **Phase 9 (v0.8.2 – v0.9.0)** added the native ML modules. The Python
  pipeline is still authoritative — only mon1 needs to be aware.
- **Phase 10 (v0.9.1 – v0.10.0)** wires the fleet. Each tier-0 node gets
  the `pt` binary so the operator can `pt add "..."` from any
  workstation; writes proxy back to mon1 over Tailscale.

## Environment

| Variable | Used by | Purpose |
|---|---|---|
| `PTASK_DB` | `pt` | Override the SQLite path. Defaults to `~/puretensor-tasks/tasks.db`. |
| `PTASK_FLEET_ROLE` *(planned v0.9.5)* | `pt` | One of `canonical / client / standalone`. Drives whether commands open a local DB or hit `PTASK_SYNC_URL`. |
| `PTASK_SYNC_URL` *(planned v0.9.5)* | `pt` | `https://ptask.ts.puretensor.local/sync`. Required when `PTASK_FLEET_ROLE=client`. |
| `PTASK_HAL_CLASSIFY_URL` | `pt distill` (native) | HAL endpoint for the speech-act classifier (v0.8.3). |
| `PTASK_HAL_CONSOLIDATE_URL` | `pt distill` (native) | HAL endpoint for cluster → task consolidation (v0.8.7). |
| `PTASK_TELEGRAM_BOT_TOKEN`, `PTASK_ACCOUNTABILITY_CHAT_ID`, `PTASK_SMTP_*` | accountability | Existing v0.7 surface. |

## Recovery

- **Lose mon1 disk:** restore latest Ceph snapshot from
  `mon1:/mnt/cephfs/ptask-backups/` (nightly cron, 30-day retention) +
  apply post-snapshot WAL from Litestream (v0.9.4 onward).
- **Lose mon1 entirely:** promote arx2 by setting `PTASK_DB` to a
  fresh restore and updating Tailscale DNS so
  `ptask.ts.puretensor.local` resolves to arx2. Inventory edit + one
  Ansible run completes the rest of the fleet's reconfiguration.
- **Lose a client node:** no data loss; `pt` config points at the
  canonical URL, local cache is rebuildable on next sync.

## What ships when

| Tag | What | Phase |
|---|---|---|
| v0.9.0 | All native ML modules in tree | 9 close |
| v0.9.1 | `cargo build --release` release pipeline | 10 |
| v0.9.2 | Ansible playbook + inventory | 10 |
| **v0.9.3** | This doc — canonical-store election locked | 10 |
| v0.9.4 | Litestream WAL → Ceph + runbook | 10 |
| v0.9.5 | `pt --remote` / `PTASK_SYNC_URL` client mode | 10 |
| v0.10.0 | Phase 10 close — fleet cutover signed off | 10 close |
