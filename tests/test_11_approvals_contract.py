"""Approval inbox contract (pTask 3.31.0) — HAL red tests, black-box.

One queue for everything waiting on the operator. Agents REQUEST; only the
operator DECIDES. The approval binds to an exact payload that pTask itself
stores and hashes, and the operator's preview is rendered FROM that payload —
so what was seen is what can be executed. Executors `consume` an approval
exactly once.

These tests drive the built `pt` binary against a throwaway PTASK_DB, a
throwaway `pt serve`, `pt mcp` over stdio, and a fake Telegram API. They pin
behaviour, not internals (except the approvals table name/columns used by the
tamper tests).

Run:  PT_BIN=<path to built pt> python3 -m pytest tests/test_11_approvals_contract.py -q
"""

from __future__ import annotations

import hashlib
import http.server
import json
import os
import pty
import re
import socket
import sqlite3
import subprocess
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
PT = os.environ.get("PT_BIN") or str(ROOT / "target" / "debug" / "pt")
AP_RE = re.compile(r"^AP-\d+$")
TOKEN_RE = re.compile(r"pt_[0-9a-f]{64}")
OPERATOR_CHAT = "4242"


# --------------------------------------------------------------------------- helpers


def sha256_bytes(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def canonical_json(obj) -> bytes:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class FakeTelegram:
    """Records every Bot API call; answers ok (or 500 when `fail` is set)."""

    def __init__(self) -> None:
        self.calls: list[dict] = []
        self.fail = False
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):  # noqa: N802
                n = int(self.headers.get("Content-Length", "0") or 0)
                raw = self.rfile.read(n) if n else b""
                try:
                    body = json.loads(raw or b"{}")
                except json.JSONDecodeError:
                    body = {"_raw": raw.decode("utf-8", "replace")}
                outer.calls.append({"path": self.path, "body": body})
                if outer.fail:
                    self.send_response(500)
                    self.end_headers()
                    self.wfile.write(b'{"ok":false}')
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(b'{"ok":true,"result":{"message_id":1}}')

            def log_message(self, *a):
                pass

        self.port = free_port()
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", self.port), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    @property
    def base(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def messages_about(self, ap_id: str) -> list[dict]:
        return [
            c["body"]
            for c in self.calls
            if c["path"].endswith("/sendMessage") and ap_id in json.dumps(c["body"])
        ]

    def close(self) -> None:
        self.server.shutdown()


def buttons(msg: dict) -> list[dict]:
    return [b for row in (msg.get("reply_markup") or {}).get("inline_keyboard", []) for b in row]


@pytest.fixture()
def tg():
    fake = FakeTelegram()
    yield fake
    fake.close()


@pytest.fixture()
def env(tmp_path, tg):
    """Hermetic env: nothing inherited but PATH, never the real DB or bot."""
    return {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(tmp_path),
        "PTASK_DB": str(tmp_path / "tasks.db"),
        "PTASK_ACTOR": "hal",
        "PTASK_TELEGRAM_BOT_TOKEN": "test-token",
        "PTASK_ACCOUNTABILITY_CHAT_ID": OPERATOR_CHAT,
        "PTASK_TELEGRAM_API_BASE": tg.base,
        "PTASK_DASH_URL": "https://ptask.example.test",
        "NO_COLOR": "1",
    }


def run(env: dict, *args: str, check: bool = True, tty: bool = False, raw: bool = False, **overrides):
    e = dict(env)
    for k, v in overrides.items():
        if v is None:
            e.pop(k, None)
        else:
            e[k] = v
    kw = dict(env=e, capture_output=True, timeout=60)
    if not raw:
        kw["text"] = True
    if tty:
        master, slave = pty.openpty()
        try:
            p = subprocess.run([PT, *args], stdin=slave, **kw)
        finally:
            os.close(slave)
            os.close(master)
    else:
        p = subprocess.run([PT, *args], stdin=subprocess.DEVNULL, **kw)
    if check and p.returncode != 0:
        raise AssertionError(f"pt {args} rc={p.returncode}\nstdout={p.stdout}\nstderr={p.stderr}")
    return p


def pj(env: dict, *args: str, **overrides):
    return json.loads(run(env, "--json", *args, **overrides).stdout)


def request(env: dict, tmp_path: Path, name: str = "draft", content: str = "Dear Alan, hello.", **kw) -> dict:
    payload = tmp_path / f"{name}.html"
    payload.write_text(content)
    note = tmp_path / f"{name}.note.md"
    note.write_text(f"Reply to Alan about {name}; operator asked for it this morning.")
    args = [
        "approval", "request",
        "--kind", kw.pop("kind", "email"),
        "--title", kw.pop("title", f"Send {name}"),
        "--note-file", str(note),
        "--payload-file", str(payload),
    ]
    for flag, value in kw.pop("extra", {}).items():
        args += [flag, value]
    return pj(env, *args, **kw)


def show(env: dict, ap: str) -> dict:
    return pj(env, "approval", "show", ap)


def dash_decide(env: dict, verb: str, ap: str, note: str | None = None, check=True, actor="operator-dashboard"):
    args = [verb, ap, "--via", "dashboard"]
    if note:
        args += ["--note", note]
    return run(env, *args, check=check, PTASK_ACTOR=actor)


def exit_of(env, *args) -> int:
    return run(env, *args, check=False).returncode


# --------------------------------------------------------------------------- request: payload binding


def test_request_stores_payload_and_binds_digest(env, tmp_path):
    ap = request(env, tmp_path, "memo", "Dear Alan, the Q3 numbers are attached.")
    assert AP_RE.match(ap["id"]), ap
    assert ap["status"] == "pending" and ap["kind"] == "email" and ap["requester"] == "hal"
    raw = (tmp_path / "memo.html").read_bytes()
    assert ap["digest"] == sha256_bytes(raw)
    assert ap["payload_stored"] is True
    assert ap["payload_kind"] == "file"
    assert ap["payload_name"] == "memo.html"
    assert ap["payload_bytes"] == len(raw)
    s = show(env, ap["id"])
    assert "the Q3 numbers are attached" in s["preview"], "preview is rendered from the stored payload"
    assert "operator asked for it" in s["request_note"], "agent prose is a separate, labelled note"
    fetched = run(env, "approval", "payload", ap["id"], raw=True).stdout
    assert fetched == raw


def test_json_payload_is_canonicalised(env, tmp_path):
    note = tmp_path / "n.md"
    note.write_text("Vendor invoice 17")
    obj = {"to": "ACME Ltd", "amount_gbp": 400, "ref": "INV-17"}
    ap = pj(env, "approval", "request", "--kind", "spend", "--title", "Pay ACME", "--note-file", str(note),
            "--payload-json", json.dumps(obj, indent=4))
    assert ap["digest"] == sha256_bytes(canonical_json(obj))
    assert ap["payload_kind"] == "json"
    assert "ACME Ltd" in show(env, ap["id"])["preview"]
    assert run(env, "approval", "payload", ap["id"], raw=True).stdout == canonical_json(obj)
    bad = run(env, "approval", "request", "--kind", "spend", "--title", "x", "--payload-json", "{not json", check=False)
    assert bad.returncode != 0


def test_digest_only_request_is_marked_unstored(env, tmp_path):
    ap = pj(env, "approval", "request", "--kind", "ebay", "--title", "List GPU", "--note", "photos too big", "--digest", "a" * 64)
    assert ap["payload_stored"] is False and ap["digest"] == "a" * 64
    assert run(env, "approval", "payload", ap["id"], check=False).returncode != 0


def test_request_validation(env, tmp_path):
    p = tmp_path / "p.txt"
    p.write_text("x")
    base = ["approval", "request", "--kind", "spend", "--title", "t"]
    assert run(env, *base, "--digest", "xyz", check=False).returncode != 0
    assert run(env, *base, "--digest", "A" * 64, check=False).returncode != 0, "lowercase hex only"
    assert run(env, *base, check=False).returncode != 0, "one payload source is required"
    assert run(env, *base, "--digest", "b" * 64, "--payload-file", str(p), check=False).returncode != 0, "exactly one"
    assert run(env, "approval", "request", "--kind", "yolo", "--title", "t", "--digest", "b" * 64, check=False).returncode != 0
    big = tmp_path / "big.bin"
    big.write_bytes(b"x" * (256 * 1024 + 1))
    r = run(env, *base, "--payload-file", str(big), check=False)
    assert r.returncode != 0 and "--digest" in (r.stdout + r.stderr), "oversize payloads must point at --digest"
    for kind in ("email", "ebay", "spend", "destroy", "external", "budget", "other"):
        r = pj(env, "approval", "request", "--kind", kind, "--title", kind, "--digest", sha256_bytes(kind.encode()))
        assert r["kind"] == kind


def test_rerequest_same_payload_while_pending_is_idempotent(env, tmp_path):
    a = request(env, tmp_path, "same", "body")
    b = request(env, tmp_path, "same", "body")
    assert a["id"] == b["id"]
    assert [x["id"] for x in pj(env, "approval", "ls")] == [a["id"]]


def test_ls_defaults_to_pending_oldest_first(env, tmp_path):
    a = request(env, tmp_path, "one", "1")
    b = request(env, tmp_path, "two", "2")
    c = request(env, tmp_path, "three", "3")
    assert [x["id"] for x in pj(env, "approval", "ls")] == [a["id"], b["id"], c["id"]]
    run(env, "approval", "withdraw", b["id"])
    assert [x["id"] for x in pj(env, "approval", "ls")] == [a["id"], c["id"]]
    assert {x["id"] for x in pj(env, "approval", "ls", "--status", "all")} == {a["id"], b["id"], c["id"]}
    assert [x["id"] for x in pj(env, "approval", "ls", "--status", "withdrawn")] == [b["id"]]


def test_task_link(env, tmp_path):
    task = pj(env, "add", "ship the thing", "--raw")
    ap = request(env, tmp_path, extra={"--task": task["pt_id"]})
    assert show(env, ap["id"])["task"] == task["pt_id"]
    bad = run(env, "approval", "request", "--kind", "other", "--title", "x", "--digest", "c" * 64, "--task", "PT-99999", check=False)
    assert bad.returncode != 0


# --------------------------------------------------------------------------- decide guardrails


def test_agent_env_cannot_decide_even_via_dashboard(env, tmp_path):
    ap = request(env, tmp_path)
    r = run(env, "approve", ap["id"], "--via", "dashboard", check=False, PTASK_ACTOR="operator-dashboard", CLAUDECODE="1")
    assert r.returncode != 0
    assert "operator" in (r.stdout + r.stderr).lower()
    assert show(env, ap["id"])["status"] == "pending"


def test_no_tty_cannot_decide_without_dashboard_via(env, tmp_path):
    ap = request(env, tmp_path)
    assert run(env, "approve", ap["id"], check=False, PTASK_ACTOR="operator").returncode != 0
    assert show(env, ap["id"])["status"] == "pending"


def test_operator_terminal_can_decide(env, tmp_path):
    ap = request(env, tmp_path)
    run(env, "approve", ap["id"], tty=True, PTASK_ACTOR="operator")
    s = show(env, ap["id"])
    assert s["status"] == "approved" and s["decided_via"] == "cli" and s["decided_by"] == "operator"


def test_dashboard_decision_records_actor_via_note(env, tmp_path):
    ap = request(env, tmp_path)
    dash_decide(env, "approve", ap["id"], note="looks right")
    s = show(env, ap["id"])
    assert s["status"] == "approved" and s["decided_via"] == "dashboard"
    assert s["decided_by"] == "operator-dashboard" and s["decision_note"] == "looks right" and s["decided_at"]


def test_reject_and_long_form_decide(env, tmp_path):
    a = request(env, tmp_path, "r1", "1")
    b = request(env, tmp_path, "r2", "2")
    dash_decide(env, "reject", a["id"], note="no")
    assert show(env, a["id"])["status"] == "rejected"
    run(env, "approval", "decide", b["id"], "approve", "--via", "dashboard", PTASK_ACTOR="operator-dashboard")
    assert show(env, b["id"])["status"] == "approved"


def test_requester_cannot_decide_own_request(env, tmp_path):
    ap = request(env, tmp_path)
    assert dash_decide(env, "approve", ap["id"], check=False, actor="hal").returncode != 0
    assert show(env, ap["id"])["status"] == "pending"


def test_decisions_are_immutable(env, tmp_path):
    ap = request(env, tmp_path)
    dash_decide(env, "approve", ap["id"])
    assert dash_decide(env, "reject", ap["id"], check=False).returncode != 0
    assert dash_decide(env, "approve", ap["id"], check=False).returncode != 0
    assert show(env, ap["id"])["status"] == "approved"


def test_database_refuses_tampering(env, tmp_path):
    ap = request(env, tmp_path, "tamper", "original")
    uuid = show(env, ap["id"])["uuid"]
    with pytest.raises(sqlite3.DatabaseError):
        with sqlite3.connect(env["PTASK_DB"]) as db:
            db.execute("UPDATE approvals SET digest = ? WHERE id = ?", ("0" * 64, uuid))
    dash_decide(env, "approve", ap["id"])
    with pytest.raises(sqlite3.DatabaseError):
        with sqlite3.connect(env["PTASK_DB"]) as db:
            db.execute("UPDATE approvals SET status = 'rejected' WHERE id = ?", (uuid,))
    s = show(env, ap["id"])
    assert s["status"] == "approved" and s["digest"] == sha256_bytes(b"original")


def test_withdraw_only_by_requester_and_only_pending(env, tmp_path):
    ap = request(env, tmp_path)
    assert run(env, "approval", "withdraw", ap["id"], check=False, PTASK_ACTOR="nexus").returncode != 0
    assert show(env, ap["id"])["status"] == "pending"
    run(env, "approval", "withdraw", ap["id"])
    assert show(env, ap["id"])["status"] == "withdrawn"
    assert dash_decide(env, "approve", ap["id"], check=False).returncode != 0
    done = request(env, tmp_path, "x2", "2")
    dash_decide(env, "approve", done["id"])
    assert run(env, "approval", "withdraw", done["id"], check=False).returncode != 0


# --------------------------------------------------------------------------- the executor gate


def test_verify_exit_codes(env, tmp_path):
    ap = request(env, tmp_path, "v", "exact bytes")
    payload = tmp_path / "v.html"
    assert exit_of(env, "approval", "verify", ap["id"], "--payload-file", str(payload)) == 3
    dash_decide(env, "approve", ap["id"])
    assert exit_of(env, "approval", "verify", ap["id"], "--payload-file", str(payload)) == 0
    assert exit_of(env, "approval", "verify", ap["id"], "--digest", sha256_bytes(payload.read_bytes())) == 0
    payload.write_text("swapped after approval")
    assert exit_of(env, "approval", "verify", ap["id"], "--payload-file", str(payload)) == 5
    rej = request(env, tmp_path, "rj", "nope")
    dash_decide(env, "reject", rej["id"])
    assert exit_of(env, "approval", "verify", rej["id"], "--payload-file", str(tmp_path / "rj.html")) == 4
    assert exit_of(env, "approval", "verify", "AP-99999", "--digest", "d" * 64) not in (0, 3, 4, 5, 6)


def test_consume_is_one_shot(env, tmp_path):
    ap = request(env, tmp_path, "once", "send exactly once")
    payload = tmp_path / "once.html"
    assert exit_of(env, "approval", "consume", ap["id"], "--payload-file", str(payload)) == 3, "pending"
    dash_decide(env, "approve", ap["id"])
    wrong = tmp_path / "wrong.html"
    wrong.write_text("something else")
    assert exit_of(env, "approval", "consume", ap["id"], "--payload-file", str(wrong)) == 5
    assert not show(env, ap["id"])["consumed_at"], "a failed consume must not latch"
    assert exit_of(env, "approval", "consume", ap["id"], "--payload-file", str(payload)) == 0
    s = show(env, ap["id"])
    assert s["consumed_at"] and s["consumed_by"] == "hal"
    assert exit_of(env, "approval", "consume", ap["id"], "--payload-file", str(payload)) == 6
    assert exit_of(env, "approval", "verify", ap["id"], "--payload-file", str(payload)) == 6


def test_consume_json_payload(env, tmp_path):
    obj = {"amount_gbp": 400, "to": "ACME Ltd"}
    ap = pj(env, "approval", "request", "--kind", "spend", "--title", "Pay", "--payload-json", json.dumps(obj))
    dash_decide(env, "approve", ap["id"])
    assert exit_of(env, "approval", "consume", ap["id"], "--payload-json", json.dumps({"to": "ACME Ltd", "amount_gbp": 400})) == 0


def test_expiry(env, tmp_path):
    ap = request(env, tmp_path, extra={"--expires-in": "1s"})
    assert ap["expires_at"]
    time.sleep(2.2)
    run(env, "approval", "expire")
    assert show(env, ap["id"])["status"] == "expired"
    assert exit_of(env, "approval", "verify", ap["id"], "--payload-file", str(tmp_path / "draft.html")) == 4
    assert dash_decide(env, "approve", ap["id"], check=False).returncode != 0
    run(env, "approval", "expire")
    assert show(env, ap["id"])["status"] == "expired"


def test_events_journaled_with_actor(env, tmp_path):
    ap = request(env, tmp_path)
    dash_decide(env, "approve", ap["id"])
    run(env, "approval", "consume", ap["id"], "--payload-file", str(tmp_path / "draft.html"))
    by_type = {e["type"]: e["actor"] for e in show(env, ap["id"])["events"]}
    assert by_type.get("approval.requested") == "hal"
    assert by_type.get("approval.approved") == "operator-dashboard"
    assert by_type.get("approval.consumed") == "hal"


# --------------------------------------------------------------------------- Telegram notify


def test_request_notifies_operator_once_with_dashboard_link(env, tmp_path, tg):
    ap = request(env, tmp_path, "tgmsg", "Dear Alan, Q3 memo body.", title="Send the Q3 memo")
    msgs = tg.messages_about(ap["id"])
    assert len(msgs) == 1, tg.calls
    m = msgs[0]
    assert str(m["chat_id"]) == OPERATOR_CHAT
    assert "Send the Q3 memo" in m["text"] and "Q3 memo body" in m["text"], "preview comes from the payload"
    urls = [b.get("url", "") for b in buttons(m)]
    assert any(u.startswith("https://ptask.example.test") and "approvals" in u for u in urls), buttons(m)
    assert not any("callback_data" in b for b in buttons(m)), "no tap-to-decide buttons until a forwarder is proven"
    request(env, tmp_path, "tgmsg", "Dear Alan, Q3 memo body.", title="Send the Q3 memo")
    assert len(tg.messages_about(ap["id"])) == 1
    assert show(env, ap["id"])["notified_at"]


def test_callback_buttons_only_when_enabled(env, tmp_path, tg):
    ap = request(env, tmp_path, "btn", "body", PTASK_TG_APPROVAL_BUTTONS="1")
    [m] = tg.messages_about(ap["id"])
    datas = [b.get("callback_data") for b in buttons(m)]
    assert f"ptapprove:{ap['id']}" in datas and f"ptreject:{ap['id']}" in datas


def test_notify_is_at_least_once_via_sweep(env, tmp_path, tg):
    tg.fail = True
    ap = request(env, tmp_path, "sweep", "body")
    assert ap["status"] == "pending"
    assert not show(env, ap["id"])["notified_at"]
    tg.fail = False
    tg.calls.clear()
    run(env, "approval", "notify")
    assert len(tg.messages_about(ap["id"])) == 1
    assert show(env, ap["id"])["notified_at"]
    run(env, "approval", "notify")
    assert len(tg.messages_about(ap["id"])) == 1, "already-notified requests are not re-sent"


# --------------------------------------------------------------------------- HTTP + /tg/callback


@pytest.fixture()
def server(env):
    tokens = {}
    for client, scope in (("hal", "write"), ("nexus", "write"), ("operator-shared", "admin"), ("scraper", "read")):
        out = run(env, "token", "create", client, "--scope", scope)
        tokens[client] = TOKEN_RE.search(out.stdout + out.stderr).group(0)
    port = free_port()
    e = dict(env, PTASK_API_TOKEN="legacy-" + "z" * 40)
    proc = subprocess.Popen([PT, "serve", "--bind", f"127.0.0.1:{port}"], env=e,
                            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    base = f"http://127.0.0.1:{port}"
    for _ in range(100):
        try:
            urllib.request.urlopen(base + "/healthz", timeout=1)
            break
        except Exception:
            time.sleep(0.1)
    else:
        proc.kill()
        raise AssertionError("pt serve did not come up: " + proc.stdout.read().decode())
    yield base, tokens
    proc.terminate()
    proc.wait(timeout=10)


def call_api(base: str, method: str, path: str, token: str | None, body: dict | None = None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(base + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            raw = r.read()
            return r.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as err:
        raw = err.read()
        try:
            return err.code, json.loads(raw) if raw else None
        except json.JSONDecodeError:
            return err.code, raw.decode("utf-8", "replace")


def create_via_http(base, token, title="Wire £400 to ACME", obj=None):
    obj = obj if obj is not None else {"to": "ACME Ltd", "amount_gbp": 400}
    return call_api(base, "POST", "/api/approvals", token,
                    {"kind": "spend", "title": title, "note": "Invoice 17", "payload_json": obj})


def test_http_create_list_get(server):
    base, t = server
    code, ap = create_via_http(base, t["hal"])
    assert code in (200, 201), ap
    assert AP_RE.match(ap["id"]) and ap["requester"] == "hal" and ap["status"] == "pending"
    assert ap["digest"] == sha256_bytes(canonical_json({"to": "ACME Ltd", "amount_gbp": 400}))
    code, items = call_api(base, "GET", "/api/approvals", t["scraper"])
    assert code == 200 and [x["id"] for x in items] == [ap["id"]]
    code, one = call_api(base, "GET", f"/api/approvals/{ap['id']}", t["scraper"])
    assert code == 200 and "ACME Ltd" in one["preview"]
    code, text_ap = call_api(base, "POST", "/api/approvals", t["hal"],
                             {"kind": "email", "title": "Mail", "payload": "Dear Alan", "payload_name": "m.txt"})
    assert code in (200, 201) and text_ap["digest"] == sha256_bytes(b"Dear Alan")
    assert call_api(base, "GET", "/api/approvals", None)[0] == 401
    code, _ = call_api(base, "POST", "/api/approvals", t["scraper"], {"kind": "spend", "title": "x", "digest": "f" * 64})
    assert code in (401, 403), "read scope cannot request"
    assert call_api(base, "GET", "/api/approvals/AP-99999", t["scraper"])[0] == 404


def test_http_decide_requires_admin(server):
    base, t = server
    _, ap = create_via_http(base, t["hal"])
    for who in ("hal", "nexus"):
        code, _ = call_api(base, "POST", f"/api/approvals/{ap['id']}/decide", t[who], {"decision": "approve"})
        assert code in (401, 403), f"{who} (write scope) must not decide"
    assert call_api(base, "GET", f"/api/approvals/{ap['id']}", t["scraper"])[1]["status"] == "pending"
    code, done = call_api(base, "POST", f"/api/approvals/{ap['id']}/decide", t["operator-shared"], {"decision": "approve", "note": "ok"})
    assert code == 200, done
    assert done["status"] == "approved" and done["decided_via"] == "api" and done["decided_by"] == "operator-shared"
    assert call_api(base, "POST", f"/api/approvals/{ap['id']}/decide", t["operator-shared"], {"decision": "reject"})[0] == 409


def test_http_withdraw_only_requester(server):
    base, t = server
    _, ap = create_via_http(base, t["hal"])
    assert call_api(base, "POST", f"/api/approvals/{ap['id']}/withdraw", t["nexus"], {})[0] == 403
    code, w = call_api(base, "POST", f"/api/approvals/{ap['id']}/withdraw", t["hal"], {})
    assert code == 200 and w["status"] == "withdrawn"


def test_tg_callback_requires_forwarder_and_operator_from_id(server):
    base, t = server
    _, a = create_via_http(base, t["hal"], "A", {"n": 1})
    _, b = create_via_http(base, t["hal"], "B", {"n": 2})

    def tap(token, data, cb, from_id=None):
        body = {"data": data, "callback_id": cb}
        if from_id is not None:
            body["from_id"] = from_id
        return call_api(base, "POST", "/tg/callback", token, body)[0]

    def status(ap):
        return call_api(base, "GET", f"/api/approvals/{ap['id']}", t["scraper"])[1]

    assert tap(t["hal"], f"ptapprove:{a['id']}", "cb-hal", int(OPERATOR_CHAT)) == 403, "agent token, even with the right from_id"
    assert tap(t["nexus"], f"ptapprove:{a['id']}", "cb-nofrom") == 403, "forwarder must say who tapped"
    assert tap(t["nexus"], f"ptapprove:{a['id']}", "cb-stranger", 999) == 403, "only the operator's taps count"
    assert status(a)["status"] == "pending"
    assert tap(t["nexus"], f"ptapprove:{a['id']}", "cb-1", int(OPERATOR_CHAT)) == 200
    s = status(a)
    assert s["status"] == "approved" and s["decided_via"] == "telegram"
    assert tap(t["nexus"], f"ptapprove:{a['id']}", "cb-1", int(OPERATOR_CHAT)) == 200, "a replayed tap is a no-op"
    assert tap(t["nexus"], f"ptreject:{b['id']}", "cb-2", int(OPERATOR_CHAT)) == 200
    assert status(b)["status"] == "rejected"


# --------------------------------------------------------------------------- MCP


def mcp_session(env: dict):
    proc = subprocess.Popen([PT, "mcp"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, text=True, bufsize=1)

    def call(msg: dict, expect_reply: bool = True):
        proc.stdin.write(json.dumps(msg) + "\n")
        proc.stdin.flush()
        if not expect_reply:
            return None
        while True:
            line = proc.stdout.readline()
            if not line:
                raise AssertionError("pt mcp closed stdout")
            reply = json.loads(line)
            if reply.get("id") == msg.get("id"):
                return reply

    call({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "contract", "version": "0"}}})
    call({"jsonrpc": "2.0", "method": "notifications/initialized"}, expect_reply=False)
    return proc, call


def test_mcp_can_request_but_has_no_decide_tool(env):
    proc, call = mcp_session(env)
    try:
        names = [t["name"] for t in call({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})["result"]["tools"]]
        for required in ("approval_request", "approval_list", "approval_status", "approval_withdraw"):
            assert required in names, names
        leaky = [n for n in names if re.search(r"approve|reject|decide", n)]
        assert leaky == [], f"agents must not be able to decide: {leaky}"
        reply = call({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "approval_request", "arguments": {
            "kind": "external", "title": "Open AWS support case", "note": "quota", "payload_json": {"case": "raise g6 quota"}}}})
        assert "AP-" in json.dumps(reply["result"]), reply
    finally:
        proc.stdin.close()
        proc.wait(timeout=10)
    [item] = pj(env, "approval", "ls")
    assert item["requester"] == "hal" and item["kind"] == "external" and item["payload_stored"] is True
    assert item["digest"] == sha256_bytes(canonical_json({"case": "raise g6 quota"}))
