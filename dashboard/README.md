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
| **LCARS** | Star-Trek ops console, blocky amber/orange/lavender (rail collapses <700px) |
| **Executive** | Light, minimal, whitespace |

## Design tokens (v2.6.8 design pass — see DESIGN_BASELINE.md)

Rules the 2026-07-06 audit added after measuring 276 contrast failures across
the four themes (worst: Exec board — `P3 HIGH` lane label at 1.27:1, heatmap
counts at 1.3:1; Mission statusbar/ages at 2.6-2.9:1):

- **`--ink`** — the only text color allowed on saturated fills (priority
  badges/chips, severity pills, gradient buttons). Dark ink passes ≥4.5:1 on
  every fill and both `--grad` endpoints; white never did on P1/P2/P5/teal.
- **`--p5t…--p1t`, `--accent-t`, `--hdr-muted`, `--live`** — *text variants*:
  same hue as the fill tokens but readable as text on the theme's surfaces.
  Dark themes alias them to the fills; Exec overrides with darkened versions.
  Any new "priority-colored text" must use `--pNt`, never `--pN`.
- **`--heat-base`** — per-theme opaque base the heatmap cells blend against so
  cell ink can be luminance-picked (`renderHeat`), re-rendered on theme switch.
- **`--dim`** must stay ≥4.5:1 on `--bg2`/`--panel2`; it is real content
  (ages, status, hints), not decoration.
- Radii scale is 3-step: 14 (panels) / 10 (cards, inputs) / 6 (chips, badges,
  small buttons). Type floor is 10px. Control icons are the inline SVG sprite
  (`#i-check` etc.), never emoji; toast text may keep emoji.
- Themes are costumes over the same bones: a skin may never cost correctness
  (contrast, overflow, touch targets) at any width — LCARS drops its rail and
  elbow padding below 700px for exactly this reason (320px reflow).

## Interaction model (v2.7.x)

- **Event delegation, no inline handlers.** Rendered controls carry
  `data-act` (+ `data-id`/`data-uuid`/`data-n`/`data-p`); two document-level
  listeners (click + Enter/Space) dispatch every action. There are **no
  inline `on*` attributes** in rendered HTML — the page is CSP-clean and the
  15s re-render doesn't reattach hundreds of handler strings. Add a new
  control by giving it a `data-act` and a branch in the click delegate, never
  an inline `onclick`.
- **Focus return** survives the re-render: overlays remember their invoker by
  a stable `[data-act][data-id]` selector and refocus the re-rendered twin.
- **Modal drawer.** The detail drawer is a real `aria-modal` dialog — scrim
  dims + blocks the board, click-scrim / Escape / close-button all dismiss,
  Tab is trapped inside. Timeline points open it too (`data-act=drawer`).
- **Phone header** (<640px): the five count chips collapse to one compact
  summary line (`#chips-c`); only one of `#chips`/`#chips-c` is displayed so
  screen readers read the counts once.

## Layout

- **Header** — crystal-cube mark, live UTC clock, count chips (crit / urgent /
  overdue / due≤7d / open), **task composer** (title + description + severity +
  optional deadline, with **🎤 speak-to-fill** voice capture), theme switcher
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
| POST | `/api/tasks` `{title, description?, priority?, deadline?}` | shells `pt add [--priority=] [--description=] [--deadline=] -- "<title>"` |
| POST | `/api/voice` (raw audio body) | Whisper STT → Bedrock Claude draft → `{transcript, fields:{title,description,priority,deadline,labels}}` to pre-fill the composer |

## Run locally

```bash
# against a copy of the DB (never the live one for dev)
PTASK_DB=/tmp/tasks.dev.db PTASK_DASH_BIND=127.0.0.1:9519 python3 server.py
# open http://127.0.0.1:9519/  (auth disabled only on loopback when PTASK_DASH_PASS is unset)
```

## Config (env)

| Var | Default | Purpose |
|-----|---------|---------|
| `PTASK_DB` | `~/puretensor-tasks/tasks.db` | SQLite path (opened read-only) |
| `PTASK_BIN` | `~/.cargo/bin/pt` | pt binary for write delegation |
| `PTASK_DASH_BIND` | `0.0.0.0:9510` | bind address |
| `PTASK_DASH_USER` | `ops` | basic-auth user |
| `PTASK_DASH_PASS` | _(unset)_ | basic-auth pass; **required for non-loopback binds** |
| `PTASK_DASH_WWW` | `./www` | static dir |
| `PTASK_STT_URL` | `http://127.0.0.1:9000/transcribe` | voice STT endpoint (local Whisper); accepts `-F audio=@` |
| `PTASK_VOICE_MODEL` | `us.anthropic.claude-haiku-4-5-20251001-v1:0` | Bedrock model for voice→task extraction |
| `PTASK_VOICE_REGION` | `$AWS_DEFAULT_REGION` or `us-east-1` | Bedrock region (keyless, IAM via `~/.aws`) |
| `PTASK_AWS_BIN` | `/usr/local/bin/aws` | aws CLI path for Bedrock invoke |
| `PTASK_VOICE_FALLBACK_URL` | `http://127.0.0.1:8772/v1/chat/completions` | local vLLM fallback if Bedrock errors |
| `PTASK_VOICE_FALLBACK_MODEL` | `mistral-medium-3.5` | fallback model id |

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

- **v0.6.1** — accessibility polish: interactive task titles in Critical Now and
  Priority Lanes now expose at least a 24px hit target with visible focus rings.
- **v0.6.0** — **🎤 speak-to-fill voice capture** in the composer. A mic button
  records (MediaRecorder) and POSTs the clip to `POST /api/voice`, which runs the
  fleet's local Whisper (`large-v3-turbo`, STT) then **AWS Bedrock Claude Haiku 4.5**
  to draft a clean title, description, severity (P1–P5) and deadline from the spoken
  note — the operator just reviews and hits Create. Bedrock is keyless (IAM via
  `~/.aws`); local vLLM is the fallback. Mic needs a secure context, so the UI
  guides http-IP users to the HTTPS host. STT/LLM endpoints are env-overridable.
- **v0.5.0** — full **task composer** replaces the one-line quick-add: the header
  control now opens a modal with title, description, a five-pill severity selector
  (P5…P1), and an optional deadline. `POST /api/tasks` accepts
  `{title, description?, priority?, deadline?}` and shells them to `pt add` as
  explicit `--priority=`/`--description=`/`--deadline=` flags with a `--` title
  separator (hyphen-safe; inline `@label`/`#project`/`~2h` on the title still
  parse). Fast capture preserved — type a title, Enter, ⌘/Ctrl+Enter to create.
- **v0.4.0** — direct severity picker + done-confirmation dialog on the dashboard.
- **v0.3.0** — scrollable priority lanes surface every pending task (no +N dead-end).
- **v0.2.1** — clamp API `limit` query parameters so negative SQLite limits cannot
  become unbounded reads; dashboard unit tests now run in CI.
- **v0.2.0** — promote/demote priority controls (▲/▼ steppers) on Critical-strip
  and lane cards. New `POST /api/tasks/<id>/priority {level:1..5}` endpoint shells
  `pt priority <id> <level>` (requires `pt` ≥ 1.2.0), which re-runs scoring so the
  composite ordering updates immediately.
- **v0.1.4** — Critical Now strip shows the top 12 by composite score (was 9).
- **v0.1.3** — fail closed on non-loopback binds without auth, add security
  headers, cap POST bodies, harden the systemd user unit, and add accessibility
  landmarks / reduced-motion handling.
- **v0.1.2** — fix done buttons to use `pt_id` (`PT-N`) rather than task UUID.
- **v0.1.1** — proper LCARS elbow-frame styling (shoulder headers, pill rail,
  blocky asymmetric panels), scoped so the other three themes are unchanged.
- **v0.1.0** — initial triage cockpit (4 themes, live poll, critical strip, lanes,
  timeline, neglect heatmap).
