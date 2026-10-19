"""Superadmin permanent delete (dashboard).

The cockpit's drawer offers "Delete permanently" for a task in any status.
`POST /api/tasks/<id>/delete` shells `pt rm --yes -- PT-N` and, like approval
decisions, is operator-only: the tailnet also carries the fleet's agents, so
it needs the X-PTask-Decide-Token. These tests drive the real handler over
HTTP against a scratch DB and a fake `pt` that records its argv.
"""

import http.client
import json
import os
import sqlite3
import stat
import tempfile
import threading
import unittest
from pathlib import Path

import server

FAKE_PT = r"""#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ["FAKE_PT_LOG"], "a") as f:
    f.write(json.dumps({"args": args}) + "\n")
if args[-1] == "PT-13":
    print("error: database is locked", file=sys.stderr)
    sys.exit(1)
print("deleted " + args[-1])
"""

TOKEN = "operator-decide-token-0123456789"
UUID_DONE = "0b6340f6-1618-434b-ba03-46d9a14df49e"


class SuperadminDeleteTests(unittest.TestCase):
    def setUp(self):
        self.td = tempfile.TemporaryDirectory()
        td = Path(self.td.name)
        db = td / "tasks.db"
        con = sqlite3.connect(db)
        con.execute("CREATE TABLE tasks (id TEXT PRIMARY KEY, pt_id TEXT, title TEXT,"
                    " status_v2 TEXT)")
        con.executemany("INSERT INTO tasks VALUES (?,?,?,?)", [
            ("11111111-1111-4111-8111-111111111111", "PT-12", "open task", "pending"),
            (UUID_DONE, "PT-40", "finished task", "done"),
            ("33333333-3333-4333-8333-333333333333", "PT-41", "dismissed task", "dismissed"),
            ("44444444-4444-4444-8444-444444444444", "PT-13", "locked task", "pending"),
        ])
        con.commit()
        con.close()
        self.log = td / "calls.jsonl"
        fake = td / "pt"
        fake.write_text(FAKE_PT)
        fake.chmod(fake.stat().st_mode | stat.S_IEXEC)
        self.saved = (server.PT_BIN, server.DB_PATH, server.DECIDE_TOKEN,
                      os.environ.get("FAKE_PT_LOG"))
        server.PT_BIN, server.DB_PATH, server.DECIDE_TOKEN = str(fake), str(db), TOKEN
        os.environ["FAKE_PT_LOG"] = str(self.log)
        self.httpd = server.ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()
        self.host = f"127.0.0.1:{self.httpd.server_port}"

    def tearDown(self):
        self.httpd.shutdown()
        self.httpd.server_close()
        server.PT_BIN, server.DB_PATH, server.DECIDE_TOKEN, log = self.saved
        if log is None:
            os.environ.pop("FAKE_PT_LOG", None)
        else:
            os.environ["FAKE_PT_LOG"] = log
        self.td.cleanup()

    def request(self, method, path, body=None, origin="self", token=TOKEN):
        conn = http.client.HTTPConnection("127.0.0.1", self.httpd.server_port)
        headers = {}
        if token is not None:
            headers["X-PTask-Decide-Token"] = token
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
        return resp.status, (json.loads(raw) if raw else None)

    def calls(self):
        if not self.log.exists():
            return []
        return [json.loads(line)["args"] for line in self.log.read_text().splitlines()]

    def test_delete_shells_pt_rm_with_the_pt_id(self):
        status, data = self.request("POST", "/api/tasks/PT-12/delete", {})
        self.assertEqual(status, 200, data)
        self.assertTrue(data["ok"])
        self.assertEqual(data["pt_id"], "PT-12")
        self.assertEqual(self.calls(), [["rm", "--yes", "--", "PT-12"]])

    def test_any_status_can_be_deleted_by_uuid_or_pt_id(self):
        # The drawer posts the task uuid; pt rm only resolves a PT-N in any
        # status, so the sidecar must translate. Closed tasks are deletable.
        status, data = self.request("POST", f"/api/tasks/{UUID_DONE}/delete", {})
        self.assertEqual(status, 200, data)
        status, data = self.request("POST", "/api/tasks/PT-41/delete", {})
        self.assertEqual(status, 200, data)
        self.assertEqual(self.calls(), [["rm", "--yes", "--", "PT-40"],
                                        ["rm", "--yes", "--", "PT-41"]])

    def test_delete_needs_the_superadmin_token(self):
        for origin in ("self", None):
            status, data = self.request("POST", "/api/tasks/PT-12/delete", {},
                                        origin=origin, token=None)
            self.assertEqual(status, 403, data)
            self.assertEqual(data["code"], "decide_token_required")
            self.assertIn("permanent deletes", data["error"])
        status, data = self.request("POST", "/api/tasks/PT-12/delete", {},
                                    token="not-the-operator-token-at-all")
        self.assertEqual(status, 403, data)
        self.assertEqual(self.calls(), [])

    def test_delete_is_disabled_without_a_strong_configured_token(self):
        for configured in ("", "short-token"):
            server.DECIDE_TOKEN = configured
            status, data = self.request("POST", "/api/tasks/PT-12/delete", {},
                                        token=configured or None)
            self.assertEqual(status, 403, data)
            self.assertEqual(data["code"], "decide_disabled")
            self.assertIn("pt rm PT-N", data["error"])
        self.assertEqual(self.calls(), [])

    def test_config_reports_superadmin_only_with_a_strong_token(self):
        status, cfg = self.request("GET", "/api/config", origin=None, token=None)
        self.assertEqual(status, 200)
        self.assertIs(cfg["superadmin"], True)
        self.assertNotIn(TOKEN, json.dumps(cfg))
        server.DECIDE_TOKEN = "short-token"
        _, cfg = self.request("GET", "/api/config", origin=None, token=None)
        self.assertIs(cfg["superadmin"], False)

    def test_bad_or_unknown_ids_never_reach_pt(self):
        for bad in ("--yes", "PT-", "x;rm", "AP-7"):
            status, _ = self.request("POST", f"/api/tasks/{bad}/delete", {})
            self.assertIn(status, (400, 404), bad)
        status, data = self.request("POST", "/api/tasks/PT-999/delete", {})
        self.assertEqual(status, 404, data)
        self.assertEqual(self.calls(), [])

    def test_cross_origin_delete_rejected(self):
        status, _ = self.request("POST", "/api/tasks/PT-12/delete", {},
                                 origin="https://evil.example")
        self.assertEqual(status, 403)
        self.assertEqual(self.calls(), [])

    def test_pt_failure_is_reported(self):
        status, data = self.request("POST", "/api/tasks/PT-13/delete", {})
        self.assertEqual(status, 500, data)
        self.assertFalse(data["ok"])
        self.assertIn("database is locked", data["message"])

    def test_drawer_offers_delete_behind_the_config_flag(self):
        html = (Path(server.__file__).parent / "www" / "index.html").read_text()
        self.assertIn('id="d-delete"', html)
        self.assertIn("/delete'", html)
        self.assertIn("cfg.superadmin", html)
        self.assertIn("$('#d-danger').hidden=!_superadmin", html)
        self.assertIn("window.confirm('Permanently delete", html)


if __name__ == "__main__":
    unittest.main()
