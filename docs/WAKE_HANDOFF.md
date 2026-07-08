# WAKE HANDOFF — pTask v1.0 activation (closed)

*Activation completed 2026-05-14 ~04:41 BST on tensor-core. Full
report at `~/reports/cc/2026-05-14_04-41_ptask-v1-final-activation-handover.md`.
This file is kept for historical reference; consult it when reasoning
about the cutover from the v0.6.5 Python shim → v1.0 native fleet.*

## What ran

All five activation steps executed, with one deliberate withdrawal:

| Step | Status | Notes |
|---|---|---|
| 1. Drop Python fallback in `~/.claude/skills/ptask/SKILL.md` | ✅ done | Also updated `~/.codex/skills/ptask/SKILL.md`. Both backed up to `.bak-20260514T041459+0100`. |
| 2. Litestream live on canonical host | ✅ done | tensor-core, CephFS file replica at `/mnt/ceph-backup/ptask-litestream/tasks.db`. RGW/S3 wasn't reachable so the CephFS path took over — see `scripts/litestream/litestream.yml`. |
| 3. Bring up `ptask-serve` on canonical host | ✅ done | `0.0.0.0:9501`, `/healthz` returns `ok`, `/version` returns `1.0.2`. |
| 4. Retire legacy `puretensor-tasks.service` | ✅ done | `sudo systemctl disable --now puretensor-tasks.service`. Port `:9500` (legacy FastAPI) is dead. |
| 5. Archive `~/puretensor-tasks/` → `~/puretensor-tasks-legacy/` read-only | ✅ done | Python code + `.git` + legacy unit files moved; live `.env` + `tasks.db` + WAL kept in place so `ptask-*` services keep firing. `chmod -R a-w`. |
| 6. Fleet ansible deploy | ✅ done | `pt 1.0.2` on all tier-0 nodes. Timers only enabled on tensor-core. `/etc/profile.d/ptask.sh` sets `PTASK_SYNC_URL=http://100.121.42.54:9501` fleet-wide. |
| ~~7. Bretalon post~~ | ❌ withdrawn | Category error — Bretalon is a separate UK Ltd with its own editorial surface for external subjects. PureTensor internal-tool announcements don't belong there. Internal repo + the activation report are the launch artefacts. |
| 8. Retire k3s `puretensor-tasks` namespace (v1.0.4) | ✅ done | Discovered post-activation: a parallel Python deployment had been running in k3s on mon1 for 63 days, serving `https://tasks.fox/`. UUID-union-merged into tensor-core (132 overlap, 306 mon1-only, 72 tc-only → 510 tasks total). Namespace + IngressRoutes + PVCs deleted; `tasks.fox` now 404. Snapshots preserved at `/tmp/ptask-migration/`. |
| 9. Fix mon1 `k3s.service` (v1.0.4) | ✅ done | Unrelated to pTask but blocked step 8: ExecStart had a literal `\` from a hand-edit, crash-looping. Restored baseline `/usr/local/bin/k3s server`. Broken unit at `/etc/systemd/system/k3s.service.bak-malformed-20260514T041017` for operator to re-introduce `--nodeport-addresses` correctly. |

## Key deviations from the pre-v1.0 plan

These triggered the v1.0.3 doc-reality patch. Item 4 was superseded by
v3.0.0.

1. **Canonical = tensor-core, not mon1.** mon1 has no `~/puretensor-tasks` and never did in this cycle. The pre-v1.0 docs were ahead of reality.
2. **Litestream uses CephFS file replica, not S3 RGW.** RGW endpoint wasn't reachable; CephFS at `/mnt/ceph-backup` was. The repo config now documents the CephFS path as active and keeps the S3 path as a documented alternate.
3. **Fleet is heterogeneous.** mon2 needs glibc 2.35 binary (Ubuntu 22.04); mon3 needs aarch64. Documented in `scripts/ansible/inventory.yml` per-host `ptask_arch_override`. Ansible playbook still assumes one controller-side binary works everywhere — the multi-arch build/dispatch logic is a v1.x.x follow-up.
4. **Superseded: `pt distill` used to call Python.** As of v3.0.0, `pt distill` is the native Rust pipeline. The `PTASK_DISTILL_PY_ROOT` drop-in is obsolete and should be removed wherever found.

## Quick verification commands

```bash
# On tensor-core:
pt --version                                              # → 1.0.3+
systemctl --user list-units 'ptask-*' --no-pager
systemctl is-active puretensor-tasks.service              # → inactive
curl -fsS http://127.0.0.1:9501/healthz                   # → ok
pt remote list --url http://127.0.0.1:9501 -n 3
litestream snapshots -config ~/.config/litestream/litestream.yml ~/puretensor-tasks/tasks.db

# From any other fleet node:
pt --version
pt remote list -n 1                                        # uses PTASK_SYNC_URL
```

## Carryovers still to land

See `docs/master-plan.md` § Carryovers. None are deployment-blocking.

- Multi-arch / old-glibc build matrix in `release.yml` and the Ansible
  playbook.
- DNS: either point `ptask.ts.puretensor.local` at tensor-core:9501 or
  drop the name from the docs.
- TUI single-key edit verbs (`r`/`d`/`l`), `gt`/`gi` triage views,
  structured tracing spans, in-process counter metrics, Telegram
  `/snooze` + `/defer`, branch/PR-creation → `in_progress`, live HAL
  `/compose-nudge` endpoint.
