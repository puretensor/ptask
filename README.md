# PTASK Triage Cockpit

A visually compelling, on-brand, live **triage cockpit** for PureTensor Task
Intelligence (`pt`). Surfaces pending tasks and critical issues ranked by the
server's composite `priority_score`, with four runtime-switchable themes.

**Live:** https://ptask.puretensor.ai (auth-gated)

## What it is

A **read-only Python stdlib sidecar** that runs alongside the canonical `pt serve`
on tensor-core. It reads the same `~/puretensor-tasks/tasks.db` **read-only**
(WAL ⇒ safe concurrent reads — zero risk to the canonical server) and **delegates
all writes to the `pt` binary**, so the canonical mutation path is never bypassed.

No pip dependencies (stdlib only). No build step on the frontend (single
self-contained `index.html`, vanilla JS + CSS custom properties).

## Themes (switch top-right, persisted in localStorage)

| Theme | Look |
|-------|------|
| **Mission Control** | Dark telemetry ops board, teal→blue glow, dense (default) |
| **Crystal / Glass** | Glassmorphism, frosted panels, gradient, crystal-cube motif |
| **LCARS** | Star-Trek ops console, blocky amber/orange/lavender |
| **Executive** | Light, minimal, whitespace |

## Layout

- **Header** — crystal-cube mark, live UTC clock, count chips (crit / urgent /
  overdue / due≤7d / open), quick-add, theme switcher
- **Critical Now** — top tasks by composite `priority_score` (raw P-level as a
  badge), pulse/glow animation on newly-arrived criticals
- **Priority Lanes** — P5→P1 columns, score-ranked within each, inline done button
- **Deadline Timeline** — dated pending tasks on a horizontal NOW-anchored axis
- **Neglect Heatmap** — pending tasks by priority × age bucket (the real
  "what's been sitting" signal)

## API (sidecar)

| Method | Path | Notes |
|--------|------|-------|
| GET | `/healthz` | no auth (tunnel/systemd probe) |
| GET | `/api/stats` | counts, throughput, overdue, due≤7d |
| GET | `/api/tasks?status=&limit=` | tasks + scoring fields |
| GET | `/api/critical?limit=` | top pending by `priority_score` |
| GET | `/api/timeline` | pending tasks with a deadline |
| GET | `/api/heatmap` | priority × age-bucket matrix |
| POST | `/api/tasks/<id>/done` | shells `pt done <id>` |
| POST | `/api/tasks` `{title}` | shells `pt add "<title>"` |

## Run locally

```bash
# against a copy of the DB (never the live one for dev)
PTASK_DB=/tmp/tasks.dev.db PTASK_DASH_BIND=127.0.0.1:9519 python3 server.py
# open http://127.0.0.1:9519/  (auth disabled when PTASK_DASH_PASS is unset)
```

## Config (env)

| Var | Default | Purpose |
|-----|---------|---------|
| `PTASK_DB` | `~/puretensor-tasks/tasks.db` | SQLite path (opened read-only) |
| `PTASK_BIN` | `~/.cargo/bin/pt` | pt binary for write delegation |
| `PTASK_DASH_BIND` | `0.0.0.0:9510` | bind address |
| `PTASK_DASH_USER` | `ops` | basic-auth user |
| `PTASK_DASH_PASS` | _(unset)_ | basic-auth pass; **auth disabled if unset** |
| `PTASK_DASH_WWW` | `./www` | static dir |

## Deploy (tensor-core)

```bash
# secrets (not in git)
echo 'PTASK_DASH_PASS=<pass>' > ~/puretensor-tasks/.dashboard.env
chmod 600 ~/puretensor-tasks/.dashboard.env

# user service
cp ptask-dashboard.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now ptask-dashboard
loginctl enable-linger "$USER"          # survive logout
curl -s localhost:9510/healthz          # -> OK
```

Public hostname `ptask.puretensor.ai` is routed via the existing k8s cloudflared
tunnel (token mode) → `http://192.168.4.253:9510`, managed in the Cloudflare
dashboard/API (DNS CNAME `ptask` → `<tunnel-id>.cfargotunnel.com`).

## Rollback

```bash
systemctl --user disable --now ptask-dashboard
rm ~/.config/systemd/user/ptask-dashboard.service
# + remove the Cloudflare DNS record + tunnel hostname rule
```

The canonical `pt serve` and `tasks.db` are never modified — nothing to revert there.

## Notes on data honesty

- Ranking uses composite `priority_score`, not raw P-level (P5 is inflated: 48 tasks).
- Neglect is derived from **task age** (`created_at`); the DB's `score_neglect`
  column is currently unpopulated.
- The heatmap groups by **priority × age bucket** because `cluster_keywords` are
  currently stopword noise, not useful topic labels.
- Dependency DAG is deferred to v2 (`depends_on` is empty across all tasks).

## Version

- **v0.1.1** — proper LCARS elbow-frame styling (shoulder headers, pill rail,
  blocky asymmetric panels), scoped so the other three themes are unchanged.
- **v0.1.0** — initial triage cockpit (4 themes, live poll, critical strip, lanes,
  timeline, neglect heatmap).
