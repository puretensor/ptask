#!/usr/bin/env python3
"""PTASK Triage Cockpit — read-only sidecar over the canonical pt task DB.

Stdlib-only (no pip). Reads ~/puretensor-tasks/tasks.db READ-ONLY (WAL = safe
concurrent reads alongside the canonical `pt serve`). All writes are delegated
to the `pt` binary so the canonical mutation path is never bypassed.

Endpoints
---------
  GET  /healthz                 -> "OK"               (no auth; tunnel/systemd probe)
  GET  /                        -> www/index.html     (auth)
  GET  /api/stats               -> counts, throughput, neglect buckets
  GET  /api/tasks?status=&limit -> task list + scoring fields
  GET  /api/critical?limit=     -> top pending by priority_score
  GET  /api/timeline            -> pending tasks that have a deadline
  GET  /api/heatmap             -> priority x age-bucket matrix
  POST /api/tasks/<id>/done     -> shells `pt done <id>`
  POST /api/tasks/<id>/priority {level:1..5} -> shells `pt priority <id> <level>`
  POST /api/tasks  {title, description?, priority?, deadline?}
                                -> shells `pt add [--priority=] [--description=]
                                   [--deadline=] -- "<title>"`
  POST /api/voice  (raw audio body) -> Whisper STT + Bedrock Claude draft
                                -> {transcript, fields:{title,description,priority,
                                   deadline,labels}} for the composer to pre-fill

Config (env)
------------
  PTASK_DB         SQLite path (default ~/puretensor-tasks/tasks.db)
  PTASK_BIN        pt binary  (default ~/.cargo/bin/pt)
  PTASK_DASH_BIND  bind addr  (default 0.0.0.0:9510)
  PTASK_DASH_USER  basic-auth user (default "ops")
  PTASK_DASH_PASS  basic-auth pass (default: disabled if unset on localhost,
                   required otherwise). Set in the systemd EnvironmentFile.
  PTASK_DASH_WWW   static dir (default ./www next to this file)
"""
from __future__ import annotations

import base64
import hmac
import json
import os
import re
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.request
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse, parse_qs

HOME = Path.home()
DB_PATH = os.environ.get("PTASK_DB", str(HOME / "puretensor-tasks" / "tasks.db"))
PT_BIN = os.environ.get("PTASK_BIN", str(HOME / ".cargo" / "bin" / "pt"))
WWW_DIR = Path(os.environ.get("PTASK_DASH_WWW", str(Path(__file__).resolve().parent / "www")))
BIND = os.environ.get("PTASK_DASH_BIND", "0.0.0.0:9510")
AUTH_USER = os.environ.get("PTASK_DASH_USER", "ops")
AUTH_PASS = os.environ.get("PTASK_DASH_PASS", "")

VERSION = "0.8.0"

# Operator-entered sources; every other source_type (claude_code, mcp,
# distilled, incident, specola, subtask_promotion, remote-cli, …) is
# machine-generated. Mirrored in the Rust dashboard route and index.html.
HUMAN_SOURCES = ("manual", "voice_memo", "telegram")

# Flux windows the cockpit can switch between: (label, SQLite datetime
# modifier). The backend returns all so switching is a local re-render.
# Mirrored in the Rust dashboard route (FLUX_WINDOWS) and index.html.
FLUX_WINDOWS = (
    ("30m", "-30 minutes"),
    ("1h", "-1 hour"),
    ("6h", "-6 hours"),
    ("24h", "-1 day"),
    ("7d", "-7 days"),
)
MAX_POST_BYTES = 16 * 1024
MAX_AUDIO_BYTES = 12 * 1024 * 1024  # voice clips (webm/opus) — separate, larger cap

# Voice capture → task: speech-to-text + an LLM that drafts the task fields.
# STT defaults to the fleet's local Whisper (instant, no upload); the field
# extraction runs on cloud AWS Bedrock Claude (keyless via ~/.aws), with the
# local vLLM as a fallback. Every endpoint/model is env-overridable so either
# stage can be repointed at any provider without code changes.
AWS_BIN = os.environ.get("PTASK_AWS_BIN", "/usr/local/bin/aws")
STT_URL = os.environ.get("PTASK_STT_URL", "http://127.0.0.1:9000/transcribe")
VOICE_MODEL = os.environ.get("PTASK_VOICE_MODEL", "us.anthropic.claude-haiku-4-5-20251001-v1:0")
VOICE_REGION = os.environ.get("PTASK_VOICE_REGION", os.environ.get("AWS_DEFAULT_REGION", "us-east-1"))
VOICE_FALLBACK_URL = os.environ.get("PTASK_VOICE_FALLBACK_URL", "http://127.0.0.1:8772/v1/chat/completions")
VOICE_FALLBACK_MODEL = os.environ.get("PTASK_VOICE_FALLBACK_MODEL", "mistral-medium-3.5")

# Columns we expose. Kept explicit so a schema change can't leak surprises.
TASK_COLS = [
    "id", "title", "description", "priority", "status",
    "created_at", "updated_at", "deadline", "source_type", "task_type",
    "priority_score", "score_urgency", "score_dependency", "score_neglect",
    "escalation_level", "dismissal_count", "last_reminded", "cluster_keywords",
]

_ID_RE = re.compile(r"^(PT-\d+|[0-9a-fA-F-]{8,36})$")
_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")


def parse_bind(bind: str) -> tuple[str, int]:
    if ":" not in bind:
        raise ValueError("bind must be HOST:PORT")
    host, port_s = bind.rsplit(":", 1)
    host = host or "0.0.0.0"
    port = int(port_s)
    if not (1 <= port <= 65535):
        raise ValueError("port out of range")
    return host, port


def is_loopback_host(host: str) -> bool:
    return host in {"127.0.0.1", "localhost", "::1"}


def parse_limit(raw: str | None, default: int, maximum: int) -> int:
    """Parse an API limit and clamp it to 1..maximum.

    SQLite treats LIMIT -1 as "no limit", so negative values must not pass
    through from query strings.
    """
    if raw in (None, ""):
        return default
    try:
        n = int(raw)
    except (TypeError, ValueError) as e:
        raise ValueError("limit must be an integer") from e
    return max(1, min(n, maximum))


# --------------------------------------------------------------------------- db
def connect():
    """Fresh read-only connection (cheap; safe for the threading server)."""
    uri = "file:%s?mode=ro" % DB_PATH
    con = sqlite3.connect(uri, uri=True, timeout=5.0, check_same_thread=True)
    con.row_factory = sqlite3.Row
    con.execute("PRAGMA busy_timeout=4000")
    return con


def _age_days(created_at: str) -> float | None:
    if not created_at:
        return None
    s = created_at.strip().replace("Z", "+00:00")
    for parse in (
        lambda x: datetime.fromisoformat(x),
        lambda x: datetime.fromisoformat(x[:19]),
        lambda x: datetime.strptime(x[:10], "%Y-%m-%d").replace(tzinfo=timezone.utc),
    ):
        try:
            dt = parse(s)
            if dt.tzinfo is None:
                dt = dt.replace(tzinfo=timezone.utc)
            return (datetime.now(timezone.utc) - dt).total_seconds() / 86400.0
        except (ValueError, TypeError):
            continue
    return None


def _parse_deadline(s: str):
    """Return (date_iso, days_until) for mixed date / ISO-tz formats, else None."""
    if not s:
        return None
    s = s.strip().replace("Z", "+00:00")
    dt = None
    for parse in (
        lambda x: datetime.fromisoformat(x),
        lambda x: datetime.strptime(x[:10], "%Y-%m-%d").replace(tzinfo=timezone.utc),
    ):
        try:
            dt = parse(s)
            break
        except (ValueError, TypeError):
            continue
    if dt is None:
        return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    days = (dt - datetime.now(timezone.utc)).total_seconds() / 86400.0
    return dt.date().isoformat(), round(days, 1)


def _row_to_task(r: sqlite3.Row) -> dict:
    d = {k: r[k] for k in r.keys()}
    age = _age_days(d.get("created_at"))
    d["age_days"] = round(age, 1) if age is not None else None
    dl = _parse_deadline(d.get("deadline"))
    if dl:
        d["deadline_date"], d["days_until"] = dl
    else:
        d["deadline_date"], d["days_until"] = None, None
    ps = d.get("priority_score")
    d["priority_score"] = round(ps, 4) if isinstance(ps, (int, float)) else 0.0
    return d


# ----------------------------------------------------------------------- query
def q_tasks(status="pending", limit=500, order="priority_score DESC, priority DESC"):
    con = connect()
    try:
        # pt_id (PT-N) lives in pt_extensions; it is the handle `pt done` accepts
        # (the bare task UUID is NOT accepted by the CLI). LEFT JOIN so a missing
        # extension row still returns the task.
        cols = ",".join("t." + c for c in TASK_COLS)
        order_q = ", ".join("t." + o.strip() for o in order.split(","))
        base = (f"SELECT {cols}, e.pt_id AS pt_id "
                "FROM tasks t LEFT JOIN pt_extensions e ON e.task_uuid = t.id ")
        if status == "all":
            sql = base + f"ORDER BY {order_q} LIMIT ?"
            rows = con.execute(sql, (limit,)).fetchall()
        else:
            sql = base + f"WHERE t.status=? ORDER BY {order_q} LIMIT ?"
            rows = con.execute(sql, (status, limit)).fetchall()
        return [_row_to_task(r) for r in rows]
    finally:
        con.close()


def q_stats():
    con = connect()
    try:
        by_pri = {int(r[0]): r[1] for r in con.execute(
            "SELECT priority,count(*) FROM tasks WHERE status='pending' GROUP BY priority")}
        by_status = {r[0]: r[1] for r in con.execute(
            "SELECT status,count(*) FROM tasks GROUP BY status")}
        by_type = {(r[0] or "unknown"): r[1] for r in con.execute(
            "SELECT task_type,count(*) FROM tasks WHERE status='pending' GROUP BY task_type")}
        # throughput: done per day, last 21d, by updated_at
        thru = [{"day": r[0], "n": r[1]} for r in con.execute(
            "SELECT substr(updated_at,1,10) d,count(*) n FROM tasks "
            "WHERE status='done' AND updated_at>=date('now','-21 days') "
            "GROUP BY d ORDER BY d")]
        pending = sum(by_pri.values())
        # Flux: added (split by origin) vs completed, across several selectable
        # windows so the cockpit switches range client-side without a refetch.
        # datetime() normalises the mixed T/space ISO forms before comparing.
        ph = ",".join("?" * len(HUMAN_SOURCES))
        flux_by_window = {}
        for label, modifier in FLUX_WINDOWS:
            added_human = added_machine = 0
            for r in con.execute(
                    "SELECT CASE WHEN source_type IN (%s) THEN 'human' ELSE 'machine' END o, "
                    "count(*) FROM tasks WHERE datetime(created_at) >= datetime('now', ?) "
                    "GROUP BY o" % ph, (*HUMAN_SOURCES, modifier)):
                if r[0] == "human":
                    added_human = r[1]
                else:
                    added_machine = r[1]
            done = con.execute(
                "SELECT count(*) FROM tasks WHERE status='done' "
                "AND datetime(updated_at) >= datetime('now', ?)", (modifier,)).fetchone()[0]
            flux_by_window[label] = {
                "added": added_human + added_machine,
                "added_human": added_human,
                "added_machine": added_machine,
                "done": done,
            }
        # deadlines in the next 7 days (count)
        due_soon = 0
        for r in con.execute("SELECT deadline FROM tasks WHERE status='pending' "
                             "AND deadline IS NOT NULL AND deadline!=''"):
            dl = _parse_deadline(r[0])
            if dl and dl[1] is not None and -3650 < dl[1] <= 7:
                due_soon += 1
        # overdue count
        overdue = 0
        for r in con.execute("SELECT deadline FROM tasks WHERE status='pending' "
                             "AND deadline IS NOT NULL AND deadline!=''"):
            dl = _parse_deadline(r[0])
            if dl and dl[1] is not None and dl[1] < 0:
                overdue += 1
        return {
            "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "pending_total": pending,
            "by_priority": by_pri,
            "by_status": by_status,
            "by_type": by_type,
            "throughput": thru,
            "due_within_7d": due_soon,
            "overdue": overdue,
            "flux": {
                "windows": [w[0] for w in FLUX_WINDOWS],
                "by_window": flux_by_window,
            },
            "version": VERSION,
        }
    finally:
        con.close()


def q_timeline():
    items = []
    for t in q_tasks(status="pending", limit=2000):
        if t["deadline_date"]:
            items.append({
                "id": t["id"], "title": t["title"], "priority": t["priority"],
                "priority_score": t["priority_score"],
                "deadline_date": t["deadline_date"], "days_until": t["days_until"],
            })
    items.sort(key=lambda x: x["deadline_date"])
    return items


AGE_BUCKETS = [(0, 7, "0-7d"), (7, 30, "1-4w"), (30, 60, "1-2m"),
               (60, 90, "2-3m"), (90, 10**9, "90d+")]


def q_heatmap():
    """priority (5..1) x age-bucket matrix of pending-task counts."""
    grid = {p: {b[2]: 0 for b in AGE_BUCKETS} for p in range(5, 0, -1)}
    for t in q_tasks(status="pending", limit=5000):
        p = t["priority"]
        age = t["age_days"]
        if p not in grid or age is None:
            continue
        for lo, hi, label in AGE_BUCKETS:
            if lo <= age < hi:
                grid[p][label] += 1
                break
    return {
        "buckets": [b[2] for b in AGE_BUCKETS],
        "rows": [{"priority": p, "cells": grid[p]} for p in range(5, 0, -1)],
    }


def q_task_events(task_uuid: str, limit: int = 60):
    """Return attributed event history for the detail drawer."""
    con = connect()
    try:
        rows = con.execute(
            """
            SELECT uuid, task_uuid, event_type, actor, ts, payload
            FROM pt_event_log
            WHERE task_uuid=?
            ORDER BY ts DESC
            LIMIT ?
            """,
            (task_uuid, limit),
        )
        events = []
        for r in rows:
            try:
                payload = json.loads(r["payload"] or "{}")
            except json.JSONDecodeError:
                payload = {}
            events.append({
                "uuid": r["uuid"],
                "task_uuid": r["task_uuid"],
                "event_type": r["event_type"],
                "actor": r["actor"],
                "ts": r["ts"],
                "payload": payload,
            })
        return events
    finally:
        con.close()


# --------------------------------------------------------------------- writes
def build_add_args(body: dict) -> tuple[list[str] | None, str | None]:
    """Translate a create-task request body into `pt add` argv (or an error).

    Returns (args, None) on success or (None, "message") on validation failure.
    Optional structured fields become explicit `--opt=value` flags and the title
    is placed after a `--` separator, so any value may safely begin with '-'. The
    title still runs through quick-add parsing (inline @label/#project/~2h keep
    working); explicit flags win over inline priority/description.
    """
    title = (body.get("title") or "").strip()
    if not (3 <= len(title) <= 400):
        return None, "title 3-400 chars"
    args = ["add"]
    pri = body.get("priority")
    if pri is not None:
        if isinstance(pri, bool) or not isinstance(pri, int) or not (1 <= pri <= 5):
            return None, "priority must be an integer 1..5"
        args.append(f"--priority={pri}")
    desc = body.get("description")
    if desc is not None:
        if not isinstance(desc, str):
            return None, "description must be a string"
        desc = desc.strip()
        if len(desc) > 4000:
            return None, "description too long (max 4000)"
        if desc:
            args.append(f"--description={desc}")
    dl = body.get("deadline")
    if dl is not None and str(dl).strip():
        dl = str(dl).strip()
        if not _DATE_RE.match(dl):
            return None, "deadline must be ISO date YYYY-MM-DD"
        try:
            datetime.strptime(dl, "%Y-%m-%d")
        except ValueError:
            return None, "deadline is not a valid date"
        args.append(f"--deadline={dl}")
    args += ["--", title]
    return args, None


def pt_exec(args: list[str]) -> tuple[bool, str]:
    env = dict(os.environ)
    env["PATH"] = str(HOME / ".cargo" / "bin") + ":" + env.get("PATH", "")
    try:
        out = subprocess.run([PT_BIN, *args], capture_output=True, text=True,
                             timeout=20, env=env)
        ok = out.returncode == 0
        return ok, (out.stdout + out.stderr).strip()
    except Exception as e:  # noqa: BLE001
        return False, f"exec error: {e}"


# ------------------------------------------------------------------------ voice
VOICE_SYSTEM = (
    "You turn a short spoken note into ONE task, returned as STRICT JSON only — no "
    "prose, no markdown fences. Today is {today} (UTC). Output exactly these keys:\n"
    '  "title": concise imperative summary of the task, <= 80 chars, no trailing period.\n'
    '  "description": helpful prose expanding the note (context, specifics, acceptance '
    'criteria). Empty string if the note adds nothing beyond the title.\n'
    '  "priority": integer 1-5 (5=critical, 4=urgent, 3=high, 2=normal, 1=low). Infer from '
    "urgency/impact words; default 2 if unstated.\n"
    '  "deadline": absolute date "YYYY-MM-DD" resolved from phrases like "today", "tomorrow", '
    '"next Friday", "in 3 days", "end of month" relative to today; null if no date is mentioned.\n'
    '  "labels": array of 0-4 short lowercase tags inferred from the topic; [] if unclear.\n'
    "Speech may contain filler words, false starts, or transcription noise — clean it up and "
    "capture the intent. Return only the JSON object."
)


def _extract_json(text: str) -> dict:
    """Pull the first JSON object out of an LLM reply (tolerates ``` fences / prose)."""
    if not text:
        return {}
    t = text.strip()
    if t.startswith("```"):
        t = re.sub(r"^```[a-zA-Z]*\n?", "", t)
        t = re.sub(r"\n?```$", "", t).strip()
    i, j = t.find("{"), t.rfind("}")
    if i == -1 or j == -1 or j < i:
        return {}
    try:
        return json.loads(t[i:j + 1])
    except json.JSONDecodeError:
        return {}


def voice_transcribe(audio: bytes, suffix: str) -> str:
    """Audio bytes -> transcript via the (local by default) Whisper STT service."""
    with tempfile.NamedTemporaryFile(suffix=suffix, delete=False) as f:
        f.write(audio)
        path = f.name
    try:
        out = subprocess.run(
            ["curl", "-s", "-m", "60", "-X", "POST", STT_URL, "-F", "audio=@" + path],
            capture_output=True, text=True, timeout=70)
        raw = (out.stdout or "").strip()
        try:
            return (json.loads(raw).get("text") or "").strip()
        except json.JSONDecodeError:
            return raw
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass


def _bedrock_extract(transcript: str, today: str) -> dict:
    """Cloud field extraction via AWS Bedrock Claude (keyless, IAM via ~/.aws)."""
    body = json.dumps({
        "anthropic_version": "bedrock-2023-05-31",
        "max_tokens": 600, "temperature": 0,
        "system": VOICE_SYSTEM.format(today=today),
        "messages": [{"role": "user", "content": transcript}],
    })
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as bf:
        bf.write(body)
        bpath = bf.name
    opath = bpath + ".out"
    try:
        r = subprocess.run(
            [AWS_BIN, "bedrock-runtime", "invoke-model", "--region", VOICE_REGION,
             "--model-id", VOICE_MODEL, "--body", "fileb://" + bpath,
             "--cli-binary-format", "raw-in-base64-out", opath],
            capture_output=True, text=True, timeout=40)
        if r.returncode != 0:
            raise RuntimeError((r.stderr or r.stdout).strip()[:200] or "bedrock invoke failed")
        with open(opath) as f:
            payload = json.load(f)
        return _extract_json(payload["content"][0]["text"])
    finally:
        for p in (bpath, opath):
            try:
                os.unlink(p)
            except OSError:
                pass


def _vllm_extract(transcript: str, today: str) -> dict:
    """Local fallback extraction via the on-fleet vLLM (OpenAI-compatible)."""
    data = json.dumps({
        "model": VOICE_FALLBACK_MODEL, "max_tokens": 600, "temperature": 0,
        "messages": [{"role": "system", "content": VOICE_SYSTEM.format(today=today)},
                     {"role": "user", "content": transcript}],
    }).encode()
    req = urllib.request.Request(VOICE_FALLBACK_URL, data=data,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=40) as resp:
        out = json.load(resp)
    return _extract_json(out["choices"][0]["message"]["content"])


def voice_extract(transcript: str) -> tuple[dict, str]:
    """Transcript -> normalized task fields. Cloud Bedrock first, local vLLM fallback."""
    today = datetime.now(timezone.utc).date().isoformat()
    raw, provider = {}, "bedrock"
    try:
        raw = _bedrock_extract(transcript, today)
    except Exception:  # noqa: BLE001 — fall through to local model
        raw = {}
    if not raw:
        try:
            raw, provider = _vllm_extract(transcript, today), "vllm"
        except Exception:  # noqa: BLE001
            raw, provider = {}, "none"
    return _normalize_voice_fields(raw, transcript), provider


def _normalize_voice_fields(d: dict, transcript: str) -> dict:
    """Coerce model output into the exact shape the composer expects (never trust it raw)."""
    d = d if isinstance(d, dict) else {}
    title = str(d.get("title") or "").strip().rstrip(".")[:400]
    if len(title) < 3:
        title = (transcript.strip()[:80] or "Voice task").strip()
    desc = str(d.get("description") or "").strip()[:4000]
    try:
        pri = int(d.get("priority"))
    except (TypeError, ValueError):
        pri = 2
    pri = min(5, max(1, pri))
    deadline = None
    dl = d.get("deadline")
    if isinstance(dl, str) and _DATE_RE.match(dl.strip()):
        s = dl.strip()
        try:
            datetime.strptime(s, "%Y-%m-%d")
            deadline = s
        except ValueError:
            deadline = None
    labels = []
    if isinstance(d.get("labels"), list):
        for x in d["labels"][:4]:
            s = re.sub(r"[^a-z0-9 _-]", "", str(x).strip().lower())[:24].strip()
            if s:
                labels.append(s)
    return {"title": title, "description": desc, "priority": pri,
            "deadline": deadline, "labels": labels}


# ---------------------------------------------------------------------- server
class Handler(BaseHTTPRequestHandler):
    server_version = "ptask-dash/" + VERSION

    def log_message(self, fmt, *args):  # quieter logs
        pass

    # -- helpers --
    def _json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self._security_headers()
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def _text(self, txt, code=200, ctype="text/plain; charset=utf-8"):
        body = txt.encode() if isinstance(txt, str) else txt
        self.send_response(code)
        self._security_headers()
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def _security_headers(self):
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("X-Frame-Options", "DENY")
        self.send_header("Referrer-Policy", "strict-origin-when-cross-origin")
        self.send_header(
            "Content-Security-Policy",
            "default-src 'self'; "
            "img-src 'self' data:; "
            "style-src 'self' 'unsafe-inline'; "
            "script-src 'self' 'unsafe-inline'; "
            "connect-src 'self'; "
            "object-src 'none'; "
            "base-uri 'none'; "
            "form-action 'self'; "
            "frame-ancestors 'none'",
        )

    def _authed(self) -> bool:
        if not AUTH_PASS:  # auth disabled (local dev) when no pass configured
            return True
        hdr = self.headers.get("Authorization", "")
        if hdr.startswith("Basic "):
            try:
                user, _, pw = base64.b64decode(hdr[6:], validate=True).decode().partition(":")
                return hmac.compare_digest(user, AUTH_USER) and hmac.compare_digest(pw, AUTH_PASS)
            except Exception:  # noqa: BLE001
                return False
        return False

    def _need_auth(self):
        self.send_response(401)
        self._security_headers()
        self.send_header("WWW-Authenticate", 'Basic realm="PTASK"')
        self.send_header("Cache-Control", "no-store")
        self.end_headers()

    def _read_json_body(self):
        try:
            n = int(self.headers.get("Content-Length", "0") or 0)
        except ValueError:
            self._json({"error": "bad content length"}, 400)
            return None
        if n > MAX_POST_BYTES:
            self._json({"error": "request body too large"}, 413)
            return None
        raw = self.rfile.read(n) if n else b""
        try:
            return json.loads(raw or b"{}")
        except json.JSONDecodeError:
            self._json({"error": "bad json"}, 400)
            return None

    def _handle_voice(self):
        """POST /api/voice — raw audio body → Whisper STT → Bedrock draft → fields."""
        try:
            n = int(self.headers.get("Content-Length", "0") or 0)
        except ValueError:
            return self._json({"error": "bad content length"}, 400)
        if n <= 0:
            return self._json({"error": "empty audio"}, 400)
        if n > MAX_AUDIO_BYTES:
            return self._json({"error": "audio too large"}, 413)
        audio = self.rfile.read(n)
        ctype = (self.headers.get("Content-Type") or "").lower()
        suffix = (".webm" if "webm" in ctype else ".ogg" if "ogg" in ctype
                  else ".wav" if "wav" in ctype
                  else ".m4a" if ("mp4" in ctype or "m4a" in ctype) else ".webm")
        try:
            transcript = voice_transcribe(audio, suffix)
        except Exception as e:  # noqa: BLE001
            return self._json({"ok": False, "error": f"transcription failed: {e}"}, 502)
        if not transcript:
            return self._json({"ok": False, "error": "no speech detected", "transcript": ""})
        fields, llm = voice_extract(transcript)
        return self._json({"ok": True, "transcript": transcript, "fields": fields,
                           "stt": "whisper", "llm": llm})

    def _serve_static(self, path):
        rel = "index.html" if path in ("/", "") else path.lstrip("/")
        target = (WWW_DIR / rel).resolve()
        if WWW_DIR.resolve() not in target.parents and target != WWW_DIR.resolve():
            return self._text("forbidden", 403)
        if not target.is_file():
            return self._text("not found", 404)
        ctype = {
            ".html": "text/html; charset=utf-8", ".css": "text/css",
            ".js": "application/javascript", ".png": "image/png",
            ".svg": "image/svg+xml", ".ico": "image/x-icon",
        }.get(target.suffix, "application/octet-stream")
        self._text(target.read_bytes(), 200, ctype)

    def _stream(self):
        self.send_response(200)
        self._security_headers()
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Connection", "keep-alive")
        self.end_headers()
        try:
            self.wfile.write(b"retry: 15000\n\n")
            self.wfile.flush()
            while True:
                self.wfile.write(
                    f"event: change\ndata: {datetime.now(timezone.utc).isoformat(timespec='seconds')}\n\n".encode()
                )
                self.wfile.flush()
                time.sleep(15)
        except (BrokenPipeError, ConnectionResetError):
            return

    # -- routes --
    def do_GET(self):
        u = urlparse(self.path)
        path, qs = u.path, parse_qs(u.query)

        if path == "/healthz":
            return self._text("OK")
        if not self._authed():
            return self._need_auth()

        try:
            if path == "/api/stats":
                return self._json(q_stats())
            if path == "/api/tasks":
                status = (qs.get("status", ["pending"])[0])
                limit = parse_limit(qs.get("limit", ["500"])[0], 500, 5000)
                return self._json({"tasks": q_tasks(status=status, limit=limit)})
            if path == "/api/critical":
                limit = parse_limit(qs.get("limit", ["12"])[0], 12, 100)
                return self._json({"tasks": q_tasks(status="pending", limit=limit)})
            if path == "/api/timeline":
                return self._json({"items": q_timeline()})
            if path == "/api/heatmap":
                return self._json(q_heatmap())
            if path == "/api/stream":
                return self._stream()
            m = re.match(r"^/api/tasks/([^/]+)/events$", path)
            if m:
                task_uuid = m.group(1)
                if not _ID_RE.match(task_uuid):
                    return self._json({"error": "bad id"}, 400)
                limit = parse_limit(qs.get("limit", ["60"])[0], 60, 200)
                return self._json({"events": q_task_events(task_uuid, limit=limit)})
            if path == "/version":
                return self._json({"version": VERSION})
            return self._serve_static(path)
        except ValueError as e:
            return self._json({"error": str(e)}, 400)
        except Exception as e:  # noqa: BLE001
            return self._json({"error": str(e)}, 500)

    def do_POST(self):
        if not self._authed():
            return self._need_auth()
        u = urlparse(self.path)
        if u.path == "/api/voice":          # binary audio body, larger cap — own reader
            return self._handle_voice()
        body = self._read_json_body()
        if body is None:
            return

        m = re.match(r"^/api/tasks/([^/]+)/done$", u.path)
        if m:
            tid = m.group(1)
            if not _ID_RE.match(tid):
                return self._json({"error": "bad id"}, 400)
            ok, msg = pt_exec(["done", tid])
            return self._json({"ok": ok, "message": msg}, 200 if ok else 500)

        m = re.match(r"^/api/tasks/([^/]+)/priority$", u.path)
        if m:
            tid = m.group(1)
            if not _ID_RE.match(tid):
                return self._json({"error": "bad id"}, 400)
            level = body.get("level")
            if not isinstance(level, int) or isinstance(level, bool) or not (1 <= level <= 5):
                return self._json({"error": "level must be an integer 1..5"}, 400)
            ok, msg = pt_exec(["priority", tid, str(level)])
            return self._json({"ok": ok, "message": msg}, 200 if ok else 500)

        if u.path == "/api/tasks":
            args, err = build_add_args(body)
            if err:
                return self._json({"error": err}, 400)
            ok, msg = pt_exec(args)
            return self._json({"ok": ok, "message": msg}, 200 if ok else 500)

        return self._json({"error": "not found"}, 404)


def main():
    try:
        host, port = parse_bind(BIND)
    except ValueError as e:
        raise SystemExit(f"Invalid PTASK_DASH_BIND={BIND!r}: {e}") from e
    if not AUTH_PASS and not is_loopback_host(host):
        raise SystemExit(
            "Refusing to serve without PTASK_DASH_PASS on non-loopback bind "
            f"{host}:{port}. Set PTASK_DASH_PASS or bind to 127.0.0.1 for local dev."
        )
    if not Path(DB_PATH).exists():
        raise SystemExit(f"DB not found: {DB_PATH}")
    print(f"ptask-dashboard v{VERSION}  db={DB_PATH}")
    print(f"  bind http://{host}:{port}  www={WWW_DIR}  auth={'on' if AUTH_PASS else 'OFF(dev)'}")
    srv = ThreadingHTTPServer((host, port), Handler)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        srv.shutdown()


if __name__ == "__main__":
    main()
