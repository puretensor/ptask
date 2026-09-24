"""Approval inbox contract (pTask 3.31.0) — HAL red tests, black-box.

One queue for everything waiting on the operator. Agents REQUEST; only the
operator DECIDES. These tests drive the built `pt` binary against a throwaway
PTASK_DB, a throwaway `pt serve`, `pt mcp` over stdio, and a fake Telegram
API. They pin behaviour, not internals.

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


# --------------------------------------------------------------------------- helpers


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


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

            def log_message(self, *a):  # silence
                pass

        self.port = free_port()
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", self.port), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def base(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def approval_messages(self) -> list[dict]:
        out = []
        for c in self.calls:
            if not c["path"].endswith("/sendMessage"):
                continue
            blob = json.dumps(c["body"])
            if "ptapprove:" in blob:
                out.append(c["body"])
        return out

    def close(self) -> None:
        self.server.shutdown()


@pytest.fixture()
def tg():
    fake = FakeTelegram()
    yield fake
    fake.close()


@pytest.fixture()
def env(tmp_path, tg):
    """Hermetic env: nothing inherited but PATH, never the real DB or bot."""
    e = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(tmp_path),
        "PTASK_DB": str(tmp_path / "tasks.db"),
        "PTASK_ACTOR": "hal",
        "PTASK_TELEGRAM_BOT_TOKEN": "test-token",
        "PTASK_ACCOUNTABILITY_CHAT_ID": "4242",
        "PTASK_TELEGRAM_API_BASE": tg.base,
        "NO_COLOR": "1",
    }
    return e


def run(env: dict, *args: str, check: bool = True, tty: bool = False, **overrides):
    e = dict(env)
    for k, v in overrides.items():
        if v is None:
            e.pop(k, None)
        else:
            e[k] = v
    if tty:
        master, slave = pty.openpty()
        try:
            p = subprocess.run(
                [PT, *args], env=e, stdin=slave, capture_output=True, text=True, timeout=60
            )
        finally:
            os.close(slave)
            os.close(master)
    else:
        p = subprocess.run(
            [PT, *args],
            env=e,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=60,
        )
    if check and p.returncode != 0:
        raise AssertionError(f"pt {args} rc={p.returncode}\nstdout={p.stdout}\nstderr={p.stderr}")
    return p


def pj(env: dict, *args: str, **overrides):
    return json.loads(run(env, "--json", *args, **overrides).stdout)


def request(env: dict, tmp_path: Path, name: str = "draft", content: str = "hello", **kw) -> dict:
    payload = tmp_path / f"{name}.html"
    payload.write_text(content)
    preview = tmp_path / f"{name}.md"
    preview.write_text(f"**To:** a@example.com\n**Subject:** {name}\n\n{content}\n")
    args = [
        "approval",
        "request",
        "--kind",
        kw.pop("kind", "email"),
        "--title",
        kw.pop("title", f"Send {name}"),
        "--body-file",
        str(preview),
        "--payload-file",
        str(payload),
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


# --------------------------------------------------------------------------- CLI: request


def test_request_returns_pending_ap_bound_to_payload_digest(env, tmp_path):
    ap = request(env, tmp_path)
    assert AP_RE.match(ap["id"]), ap
    assert ap["status"] == "pending"
    assert ap["kind"] == "email"
    assert ap["requester"] == "hal"
    assert ap["digest"] == sha256_file(tmp_path / "draft.html")
    assert ap["payload_ref"] == str(tmp_path / "draft.html")
    assert "Subject" in ap["body"]


def test_request_validation(env, tmp_path):
    body = tmp_path / "b.md"
    body.write_text("x")
    good = "a" * 64
    ok = pj(env, "approval", "request", "--kind", "spend", "--title", "t", "--body-file", str(body), "--digest", good)
    assert ok["digest"] == good and ok["kind"] == "spend"
    bad_digest = run(env, "approval", "request", "--kind", "spend", "--title", "t", "--body-file", str(body), "--digest", "xyz", check=False)
    assert bad_digest.returncode != 0
    upper = run(env, "approval", "request", "--kind", "spend", "--title", "t2", "--body-file", str(body), "--digest", "A" * 64, check=False)
    assert upper.returncode != 0, "digest must be lowercase hex"
    no_digest = run(env, "approval", "request", "--kind", "spend", "--title", "t", "--body-file", str(body), check=False)
    assert no_digest.returncode != 0, "one of --digest / --payload-file is required"
    bad_kind = run(env, "approval", "request", "--kind", "yolo", "--title", "t", "--body-file", str(body), "--digest", "b" * 64, check=False)
    assert bad_kind.returncode != 0
    for kind in ("email", "ebay", "spend", "destroy", "external", "budget", "other"):
        r = pj(env, "approval", "request", "--kind", kind, "--title", kind, "--body-file", str(body), "--digest", hashlib.sha256(kind.encode()).hexdigest())
        assert r["kind"] == kind


def test_rerequest_same_digest_while_pending_is_idempotent(env, tmp_path):
    a = request(env, tmp_path, "same", "body")
    b = request(env, tmp_path, "same", "body")
    assert a["id"] == b["id"]
    pending = pj(env, "approval", "ls")
    assert [x["id"] for x in pending] == [a["id"]]


def test_ls_defaults_to_pending_oldest_first(env, tmp_path):
    a = request(env, tmp_path, "one", "1")
    b = request(env, tmp_path, "two", "2")
    c = request(env, tmp_path, "three", "3")
    assert [x["id"] for x in pj(env, "approval", "ls")] == [a["id"], b["id"], c["id"]]
    run(env, "approval", "withdraw", b["id"])
    assert [x["id"] for x in pj(env, "approval", "ls")] == [a["id"], c["id"]]
    all_ids = {x["id"] for x in pj(env, "approval", "ls", "--status", "all")}
    assert all_ids == {a["id"], b["id"], c["id"]}
    assert [x["id"] for x in pj(env, "approval", "ls", "--status", "withdrawn")] == [b["id"]]


def test_task_link(env, tmp_path):
    task = pj(env, "add", "ship the thing")
    ap = request(env, tmp_path, extra={"--task": task["pt_id"]})
    assert show(env, ap["id"])["task"] == task["pt_id"]
    bad = run(env, "approval", "request", "--kind", "other", "--title", "x", "--body-file", str(tmp_path / "draft.md"), "--digest", "c" * 64, "--task", "PT-99999", check=False)
    assert bad.returncode != 0


# --------------------------------------------------------------------------- CLI: decide guardrails


def test_agent_env_cannot_decide_even_via_dashboard(env, tmp_path):
    ap = request(env, tmp_path)
    r = run(env, "approve", ap["id"], "--via", "dashboard", check=False, PTASK_ACTOR="operator-dashboard", CLAUDECODE="1")
    assert r.returncode != 0
    assert "operator" in (r.stdout + r.stderr).lower()
    assert show(env, ap["id"])["status"] == "pending"


def test_no_tty_cannot_decide_without_dashboard_via(env, tmp_path):
    ap = request(env, tmp_path)
    r = run(env, "approve", ap["id"], check=False, PTASK_ACTOR="operator")
    assert r.returncode != 0
    assert show(env, ap["id"])["status"] == "pending"


def test_operator_terminal_can_decide(env, tmp_path):
    ap = request(env, tmp_path)
    run(env, "approve", ap["id"], tty=True, PTASK_ACTOR="operator")
    s = show(env, ap["id"])
    assert s["status"] == "approved"
    assert s["decided_via"] == "cli"
    assert s["decided_by"] == "operator"


def test_dashboard_decision_records_actor_via_note(env, tmp_path):
    ap = request(env, tmp_path)
    dash_decide(env, "approve", ap["id"], note="looks right")
    s = show(env, ap["id"])
    assert s["status"] == "approved"
    assert s["decided_via"] == "dashboard"
    assert s["decided_by"] == "operator-dashboard"
    assert s["note"] == "looks right"
    assert s["decided_at"]


def test_reject_and_long_form_decide(env, tmp_path):
    a = request(env, tmp_path, "r1", "1")
    b = request(env, tmp_path, "r2", "2")
    dash_decide(env, "reject", a["id"], note="no")
    assert show(env, a["id"])["status"] == "rejected"
    run(env, "approval", "decide", b["id"], "approve", "--via", "dashboard", PTASK_ACTOR="operator-dashboard")
    assert show(env, b["id"])["status"] == "approved"


def test_requester_cannot_decide_own_request(env, tmp_path):
    ap = request(env, tmp_path)
    r = dash_decide(env, "approve", ap["id"], check=False, actor="hal")
    assert r.returncode != 0
    assert show(env, ap["id"])["status"] == "pending"


def test_decisions_are_immutable(env, tmp_path):
    ap = request(env, tmp_path)
    dash_decide(env, "approve", ap["id"])
    again = dash_decide(env, "reject", ap["id"], check=False)
    assert again.returncode != 0
    twice = dash_decide(env, "approve", ap["id"], check=False)
    assert twice.returncode != 0
    assert show(env, ap["id"])["status"] == "approved"


def test_withdraw_only_by_requester_and_only_pending(env, tmp_path):
    ap = request(env, tmp_path)
    other = run(env, "approval", "withdraw", ap["id"], check=False, PTASK_ACTOR="nexus")
    assert other.returncode != 0
    assert show(env, ap["id"])["status"] == "pending"
    run(env, "approval", "withdraw", ap["id"])
    assert show(env, ap["id"])["status"] == "withdrawn"
    late = dash_decide(env, "approve", ap["id"], check=False)
    assert late.returncode != 0
    assert show(env, ap["id"])["status"] == "withdrawn"
    done = request(env, tmp_path, "x2", "2")
    dash_decide(env, "approve", done["id"])
    assert run(env, "approval", "withdraw", done["id"], check=False).returncode != 0


# --------------------------------------------------------------------------- verify (the executor gate)


def test_verify_exit_codes(env, tmp_path):
    ap = request(env, tmp_path, "v", "exact bytes")
    payload = tmp_path / "v.html"
    assert run(env, "approval", "verify", ap["id"], "--payload-file", str(payload), check=False).returncode == 3
    dash_decide(env, "approve", ap["id"])
    assert run(env, "approval", "verify", ap["id"], "--payload-file", str(payload), check=False).returncode == 0
    assert run(env, "approval", "verify", ap["id"], "--digest", sha256_file(payload), check=False).returncode == 0
    payload.write_text("swapped after approval")
    assert run(env, "approval", "verify", ap["id"], "--payload-file", str(payload), check=False).returncode == 5
    rej = request(env, tmp_path, "rj", "nope")
    dash_decide(env, "reject", rej["id"])
    assert run(env, "approval", "verify", rej["id"], "--payload-file", str(tmp_path / "rj.html"), check=False).returncode == 4
    assert run(env, "approval", "verify", "AP-99999", "--digest", "d" * 64, check=False).returncode not in (0, 3, 4, 5)


def test_expiry(env, tmp_path):
    ap = request(env, tmp_path, extra={"--expires-in": "1s"})
    assert ap["expires_at"]
    time.sleep(2.2)
    run(env, "approval", "expire")
    assert show(env, ap["id"])["status"] == "expired"
    assert run(env, "approval", "verify", ap["id"], "--payload-file", str(tmp_path / "draft.html"), check=False).returncode == 4
    assert dash_decide(env, "approve", ap["id"], check=False).returncode != 0
    run(env, "approval", "expire")  # idempotent
    assert show(env, ap["id"])["status"] == "expired"


def test_events_journaled_with_actor(env, tmp_path):
    ap = request(env, tmp_path)
    dash_decide(env, "approve", ap["id"])
    events = show(env, ap["id"])["events"]
    by_type = {e["type"]: e["actor"] for e in events}
    assert by_type.get("approval.requested") == "hal"
    assert by_type.get("approval.approved") == "operator-dashboard"


# --------------------------------------------------------------------------- Telegram notify


def test_request_notifies_operator_with_buttons_once(env, tmp_path, tg):
    ap = request(env, tmp_path, "tgmsg", "body", title="Send the Q3 memo")
    msgs = tg.approval_messages()
    assert len(msgs) == 1, tg.calls
    m = msgs[0]
    assert str(m["chat_id"]) == "4242"
    assert ap["id"] in m["text"] and "Send the Q3 memo" in m["text"]
    datas = [b["callback_data"] for row in m["reply_markup"]["inline_keyboard"] for b in row]
    assert f"ptapprove:{ap['id']}" in datas
    assert f"ptreject:{ap['id']}" in datas
    request(env, tmp_path, "tgmsg", "body", title="Send the Q3 memo")  # idempotent re-request
    assert len(tg.approval_messages()) == 1
    assert show(env, ap["id"])["notified_at"]


def test_notify_is_at_least_once_via_sweep(env, tmp_path, tg):
    tg.fail = True
    ap = request(env, tmp_path, "sweep", "body")  # must still succeed
    assert ap["status"] == "pending"
    assert not show(env, ap["id"])["notified_at"]
    tg.fail = False
    tg.calls.clear()
    run(env, "approval", "notify")
    assert len(tg.approval_messages()) == 1
    assert show(env, ap["id"])["notified_at"]
    run(env, "approval", "notify")
    assert len(tg.approval_messages()) == 1, "already-notified requests are not re-sent"


# --------------------------------------------------------------------------- HTTP + /tg/callback


@pytest.fixture()
def server(env):
    tokens = {}
    for client, scope in (("hal", "write"), ("nexus", "write"), ("operator-shared", "admin"), ("scraper", "read")):
        out = run(env, "token", "create", client, "--scope", scope)
        tokens[client] = TOKEN_RE.search(out.stdout + out.stderr).group(0)
    port = free_port()
    e = dict(env, PTASK_API_TOKEN="legacy-" + "z" * 40)
    proc = subprocess.Popen(
        [PT, "serve", "--bind", f"127.0.0.1:{port}"],
        env=e,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
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


def create_via_http(base, token, title="Wire $400 to vendor", digest="e" * 64):
    return call_api(base, "POST", "/api/approvals", token, {"kind": "spend", "title": title, "body": "Pay invoice 17", "digest": digest})


def test_http_create_list_get(server):
    base, t = server
    code, ap = create_via_http(base, t["hal"])
    assert code in (200, 201), ap
    assert AP_RE.match(ap["id"]) and ap["requester"] == "hal" and ap["status"] == "pending"
    code, items = call_api(base, "GET", "/api/approvals", t["scraper"])
    assert code == 200 and [x["id"] for x in items] == [ap["id"]]
    code, one = call_api(base, "GET", f"/api/approvals/{ap['id']}", t["scraper"])
    assert code == 200 and one["digest"] == "e" * 64
    code, _ = call_api(base, "GET", "/api/approvals", None)
    assert code == 401
    code, _ = call_api(base, "POST", "/api/approvals", t["scraper"], {"kind": "spend", "title": "x", "body": "y", "digest": "f" * 64})
    assert code in (401, 403), "read scope cannot request"
    code, _ = call_api(base, "GET", "/api/approvals/AP-99999", t["scraper"])
    assert code == 404


def test_http_decide_requires_admin(server):
    base, t = server
    _, ap = create_via_http(base, t["hal"])
    for who in ("hal", "nexus"):
        code, _ = call_api(base, "POST", f"/api/approvals/{ap['id']}/decide", t[who], {"decision": "approve"})
        assert code in (401, 403), f"{who} (write scope) must not decide"
    _, still = call_api(base, "GET", f"/api/approvals/{ap['id']}", t["scraper"])
    assert still["status"] == "pending"
    code, done = call_api(base, "POST", f"/api/approvals/{ap['id']}/decide", t["operator-shared"], {"decision": "approve", "note": "ok"})
    assert code == 200, done
    assert done["status"] == "approved" and done["decided_via"] == "api" and done["decided_by"] == "operator-shared"
    code, _ = call_api(base, "POST", f"/api/approvals/{ap['id']}/decide", t["operator-shared"], {"decision": "reject"})
    assert code == 409


def test_http_withdraw_only_requester(server):
    base, t = server
    _, ap = create_via_http(base, t["hal"])
    code, _ = call_api(base, "POST", f"/api/approvals/{ap['id']}/withdraw", t["nexus"], {})
    assert code == 403
    code, w = call_api(base, "POST", f"/api/approvals/{ap['id']}/withdraw", t["hal"], {})
    assert code == 200 and w["status"] == "withdrawn"


def test_tg_callback_only_from_forwarder(server):
    base, t = server
    _, a = create_via_http(base, t["hal"], "A", "1" * 64)
    _, b = create_via_http(base, t["hal"], "B", "2" * 64)
    code, _ = call_api(base, "POST", "/tg/callback", t["hal"], {"data": f"ptapprove:{a['id']}", "callback_id": "cb-hal"})
    assert code == 403, "an agent write token must not approve through the Telegram route"
    _, s = call_api(base, "GET", f"/api/approvals/{a['id']}", t["scraper"])
    assert s["status"] == "pending"
    code, _ = call_api(base, "POST", "/tg/callback", t["nexus"], {"data": f"ptapprove:{a['id']}", "callback_id": "cb-1"})
    assert code == 200
    _, s = call_api(base, "GET", f"/api/approvals/{a['id']}", t["scraper"])
    assert s["status"] == "approved" and s["decided_via"] == "telegram"
    code, _ = call_api(base, "POST", "/tg/callback", t["nexus"], {"data": f"ptapprove:{a['id']}", "callback_id": "cb-1"})
    assert code == 200, "a retried forward of the same tap is a no-op, not an error"
    code, _ = call_api(base, "POST", "/tg/callback", t["nexus"], {"data": f"ptreject:{b['id']}", "callback_id": "cb-2"})
    assert code == 200
    _, s = call_api(base, "GET", f"/api/approvals/{b['id']}", t["scraper"])
    assert s["status"] == "rejected" and s["decided_via"] == "telegram"


# --------------------------------------------------------------------------- MCP


def mcp_session(env: dict):
    proc = subprocess.Popen(
        [PT, "mcp"],
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        bufsize=1,
    )

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
        reply = call({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "approval_request", "arguments": {"kind": "external", "title": "Open AWS support case", "body": "Case text", "digest": "9" * 64}}})
        assert "AP-" in json.dumps(reply["result"]), reply
    finally:
        proc.stdin.close()
        proc.wait(timeout=10)
    items = pj(env, "approval", "ls")
    assert len(items) == 1 and items[0]["requester"] == "hal" and items[0]["kind"] == "external"
