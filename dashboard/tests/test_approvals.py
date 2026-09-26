"""Approvals panel contract (dashboard) — HAL red tests.

The dashboard is the operator's surface for the pTask approval inbox. It lists
pending approvals from `pt --json approval ls` and decides them with
`pt approve|reject AP-n --via dashboard`. These tests drive the real handler
over HTTP with a fake `pt` binary that records its argv + env.
"""

import http.client
import json
import os
import stat
import tempfile
import threading
import unittest
from pathlib import Path

import server

CANNED = [
    {
        "id": "AP-7",
        "kind": "email",
        "title": "Send Q3 memo to Alan",
        "preview": "To: alan@example.com\n<script>alert(1)</script>",
        "request_note": "Operator asked for this <b>today</b>",
        "payload_stored": True,
        "payload_name": "memo.html",
        "payload_bytes": 48,
        "requester": "hal",
        "status": "pending",
        "digest": "a" * 64,
        "created_at": "2026-09-24T22:00:00+00:00",
    }
]

FAKE_PT = r"""#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ["FAKE_PT_LOG"], "a") as f:
    f.write(json.dumps({"args": args, "claudecode": os.environ.get("CLAUDECODE"),
                        "actor": os.environ.get("PTASK_ACTOR")}) + "\n")
if "approval" in args and "ls" in args:
    print(json.dumps(CANNED))
    sys.exit(0)
if args[:2] in (["approve", "AP-8"], ["reject", "AP-8"]):
    print("error: AP-8 is already approved", file=sys.stderr)
    sys.exit(1)
print("ok")
""".replace("CANNED", repr(CANNED).replace("'", '"'))


class ApprovalsPanelTests(unittest.TestCase):
    def setUp(self):
        self.td = tempfile.TemporaryDirectory()
        td = Path(self.td.name)
        self.log = td / "calls.jsonl"
        fake = td / "pt"
        fake.write_text(FAKE_PT)
        fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
        self.saved = server.PT_BIN
        self.saved_env = {k: os.environ.get(k) for k in ("FAKE_PT_LOG", "CLAUDECODE", "PTASK_ACTOR")}
        server.PT_BIN = str(fake)
        os.environ["FAKE_PT_LOG"] = str(self.log)
        os.environ.pop("PTASK_ACTOR", None)
        self.httpd = server.ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()
        self.host = f"127.0.0.1:{self.httpd.server_port}"

    def tearDown(self):
        self.httpd.shutdown()
        server.PT_BIN = self.saved
        for k, v in self.saved_env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        self.td.cleanup()

    # ---------------------------------------------------------------- helpers
    def request(self, method, path, body=None, origin="self"):
        conn = http.client.HTTPConnection("127.0.0.1", self.httpd.server_port)
        headers = {}
        if origin == "self":
            headers["Origin"] = f"http://{self.host}"
        elif origin:
            headers["Origin"] = origin
        payload = None
        if body is not None:
            payload = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        conn.request(method, path, body=payload, headers=headers)
        resp = conn.getresponse()
        raw = resp.read()
        conn.close()
        try:
            data = json.loads(raw) if raw else None
        except json.JSONDecodeError:
            data = raw.decode("utf-8", "replace")
        return resp.status, data

    def calls(self):
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text().splitlines() if line.strip()]

    # ---------------------------------------------------------------- list
    def test_list_is_open_without_credentials(self):
        status, data = self.request("GET", "/api/approvals", origin=None)
        self.assertEqual(status, 200, data)
        self.assertEqual(data, CANNED)
        self.assertTrue(self.calls())

    def test_list_returns_pending_from_pt(self):
        status, data = self.request("GET", "/api/approvals", origin=None)
        self.assertEqual(status, 200, data)
        self.assertEqual(data, CANNED)
        args = self.calls()[-1]["args"]
        self.assertIn("--json", args)
        self.assertEqual(args[args.index("approval"):args.index("approval") + 2], ["approval", "ls"])
        self.assertEqual(args[args.index("--status") + 1], "pending")

    def test_list_status_is_whitelisted(self):
        status, _ = self.request("GET", "/api/approvals?status=all", origin=None)
        self.assertEqual(status, 200)
        args = self.calls()[-1]["args"]
        self.assertEqual(args[args.index("--status") + 1], "all")
        before = len(self.calls())
        status, _ = self.request("GET", "/api/approvals?status=--evil", origin=None)
        self.assertEqual(status, 400)
        self.assertEqual(len(self.calls()), before)

    # ---------------------------------------------------------------- decide
    def test_approve_calls_pt_with_dashboard_via_and_note(self):
        status, data = self.request("POST", "/api/approvals/AP-7/approve", {"note": "fine"})
        self.assertEqual(status, 200, data)
        self.assertEqual(self.calls()[-1]["args"], ["approve", "AP-7", "--via", "dashboard", "--note", "fine"])

    def test_reject_without_note(self):
        status, data = self.request("POST", "/api/approvals/AP-7/reject", {})
        self.assertEqual(status, 200, data)
        self.assertEqual(self.calls()[-1]["args"], ["reject", "AP-7", "--via", "dashboard"])

    def test_decision_runs_as_dashboard_actor_without_agent_markers(self):
        os.environ["CLAUDECODE"] = "1"  # dashboard launched from an agent shell in dev
        status, _ = self.request("POST", "/api/approvals/AP-7/approve", {})
        self.assertEqual(status, 200)
        call = self.calls()[-1]
        self.assertIsNone(call["claudecode"], "agent markers must not reach pt from the operator surface")
        self.assertEqual(call["actor"], "dashboard")

    def test_invalid_ids_rejected_before_exec(self):
        for bad in ("PT-1", "AP-", "AP-1x", "--via", "AP-1%3Brm", "ap-1"):
            status, _ = self.request("POST", f"/api/approvals/{bad}/approve", {})
            self.assertIn(status, (400, 404), bad)
        status, _ = self.request("POST", "/api/approvals/AP-7/delete", {})
        self.assertIn(status, (400, 404))
        self.assertEqual(self.calls(), [])

    def test_cross_origin_posts_rejected_same_origin_open(self):
        status, _ = self.request("POST", "/api/approvals/AP-7/approve", {}, origin="https://evil.example")
        self.assertEqual(status, 403)
        self.assertEqual(self.calls(), [])
        status, data = self.request("POST", "/api/approvals/AP-7/approve", {})
        self.assertEqual(status, 200, data)
        self.assertTrue(self.calls())

    def test_note_is_bounded(self):
        status, _ = self.request("POST", "/api/approvals/AP-7/approve", {"note": "x" * 2001})
        self.assertEqual(status, 400)
        status, _ = self.request("POST", "/api/approvals/AP-7/approve", {"note": 5})
        self.assertEqual(status, 400)
        self.assertEqual(self.calls(), [])

    def test_pt_refusal_surfaces_as_conflict(self):
        status, data = self.request("POST", "/api/approvals/AP-8/approve", {})
        self.assertEqual(status, 409)
        self.assertIn("already approved", json.dumps(data))

    # ---------------------------------------------------------------- UI
    def test_index_has_approvals_panel(self):
        html = (Path(server.__file__).parent / "www" / "index.html").read_text()
        self.assertIn('id="approvals"', html)
        self.assertIn("/api/approvals", html)


if __name__ == "__main__":
    unittest.main()
