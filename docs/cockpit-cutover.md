# Triage Cockpit cutover (v2.3.0) — operator runbook

As of v2.3.0 the cockpit's entire API + UI is served by `pt serve` on
`100.121.42.54:9501`. The Python sidecar on `:9510` remains ONLY as the voice
shim (`/api/voice` → Whisper STT + field extraction); `pt serve` proxies to it.
Parity was verified endpoint-by-endpoint at cutover (156/156 tasks, identical
ordering, stats/heatmap/timeline byte-equal modulo the intentional
`ai_reasoning` addition).

## Already done (no action)
- `~/puretensor-tasks/.env` carries `PTASK_DASH_USER`/`PTASK_DASH_PASS`
  (copied from `.dashboard.env`) — same Basic credentials on :9501.
- `PTASK_DASH_WWW` defaults to `~/ptask/dashboard/www` — the UI ships from the
  repo; a redeploy of the binary + `git pull` updates both halves.

## Manual step 1 — restore the public hostname (tunnel is DEAD)
`ptask.puretensor.ai` is a CNAME to cloudflared tunnel `84959b03-…` (created
2026-05-30, comment says "→ tensor-core:9510") but **no cloudflared daemon,
config, or credential exists anywhere on tensor-core** — the public host has
been 501ing at the edge for an unknown period, independent of this migration.
Tracked as a PT task. To restore (needs interactive browser login):

```bash
cloudflared tunnel login                      # browser → authorize zone puretensor.ai
cloudflared tunnel create ptask               # new tunnel (old credential is lost)
cat > ~/.cloudflared/config.yml <<'EOF'
tunnel: <NEW-TUNNEL-UUID>
credentials-file: /home/puretensorai/.cloudflared/<NEW-TUNNEL-UUID>.json
ingress:
  - hostname: ptask.puretensor.ai
    service: http://100.121.42.54:9501        # v2.3.0: the Rust server, NOT :9510
  - service: http_status:404
EOF
cloudflared tunnel route dns -f ptask ptask.puretensor.ai   # repoints the CNAME
sudo cloudflared service install && sudo systemctl enable --now cloudflared
```

## Manual step 2 — Cloudflare Access in front of the public host
Zero Trust → Access → Applications → Add "ptask.puretensor.ai", allow-list the
operator identity (Google Workspace ops@/heimir), session 24h. Basic auth
stays underneath as the second factor.

## Manual step 3 — repoint specola (do this LAST, after a week of parallel ops)
`specola/backend/config.py:54` reads the ptask base URL — change `:9510` →
`:9501`. Shapes are identical; nothing else changes. Roll back by reverting
the one line.

## Optional cleanup (after specola repoint + a quiet week)
Shrink `dashboard/server.py` to the voice endpoints only (delete q_*/pt_exec/
static serving — ~450 lines) and rename the unit to `ptask-voice-shim`. The
`:9510` bind then serves `/api/voice` and `/healthz` alone. Until then the
sidecar keeps running untouched as a warm fallback: point the tunnel ingress
back at `:9510` to fully roll back the cutover.
