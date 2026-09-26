import http.client
import io
import json
import os
import sqlite3
import tempfile
import threading
import unittest
from datetime import date
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

import server


class BindSafetyTests(unittest.TestCase):
    def test_parse_bind_accepts_host_port(self):
        self.assertEqual(server.parse_bind("127.0.0.1:9510"), ("127.0.0.1", 9510))

    def test_parse_bind_rejects_invalid_port(self):
        with self.assertRaises(ValueError):
            server.parse_bind("127.0.0.1:99999")


def _readme_config_defaults():
    """Parse the Config (env) table in dashboard/README.md into {var: default}."""
    readme = Path(__file__).resolve().parent.parent / "README.md"
    rows = {}
    in_table = False
    for line in readme.read_text().splitlines():
        if line.startswith("| Var | Default |"):
            in_table = True
            continue
        if not in_table:
            continue
        if not line.startswith("|"):
            break
        if line.startswith("|---"):
            continue
        parts = [p.strip() for p in line.strip().strip("|").split("|")]
        if len(parts) < 2:
            continue
        var = parts[0].strip("`")
        default = parts[1].strip("`")
        rows[var] = default
    return rows


class ReadmeDefaultsTests(unittest.TestCase):
    """The config table and module docstring must match the live defaults.

    v3.18.0 moved the bind default to loopback and v0.15.0 retargeted the
    voice fallback; the README table and server.py docstring were left on
    the old values. Following those docs re-exposes the dashboard and
    points STT failover at a seat that no longer listens.
    """

    def test_config_table_matches_server_defaults(self):
        rows = _readme_config_defaults()
        self.assertEqual(rows.get("PTASK_DASH_BIND"), "127.0.0.1:9510")
        self.assertEqual(
            rows.get("PTASK_VOICE_FALLBACK_URL"),
            "http://127.0.0.1:8600/v1/chat/completions",
        )
        self.assertEqual(rows.get("PTASK_VOICE_FALLBACK_MODEL"), "nemotron-lightning")
        src = Path(server.__file__).read_text()
        self.assertIn("PTASK_DASH_BIND  bind addr  (default 127.0.0.1:9510)", src)
        self.assertIn(
            'os.environ.get("PTASK_DASH_BIND", "127.0.0.1:9510")',
            src,
        )
        self.assertIn(
            'os.environ.get("PTASK_VOICE_FALLBACK_URL", "http://127.0.0.1:8600/v1/chat/completions")',
            src,
        )
        self.assertIn(
            'os.environ.get("PTASK_VOICE_FALLBACK_MODEL", "nemotron-lightning")',
            src,
        )


class DeadlineTests(unittest.TestCase):
    def test_date_only_deadline_due_today_is_not_overdue(self):
        today = server._operator_today()
        self.assertEqual(server._parse_deadline(today.isoformat()), (today.isoformat(), 0.0))
        yesterday = date.fromordinal(today.toordinal() - 1)
        self.assertEqual(server._parse_deadline(yesterday.isoformat())[1], -1.0)

    def test_timestamps_still_parse(self):
        self.assertEqual(server._parse_deadline("2026-01-01T09:00:00Z")[0], "2026-01-01")
        self.assertIsNone(server._parse_deadline("not-a-date"))


class JournalCursorTests(unittest.TestCase):
    def test_cursor_tracks_the_event_log_and_tolerates_absence(self):
        con = sqlite3.connect(":memory:")
        self.assertIsNone(server.journal_cursor(con))  # no table yet
        self.assertIsNone(server.journal_cursor(None))
        con.execute("CREATE TABLE pt_event_log (id INTEGER PRIMARY KEY)")
        self.assertEqual(server.journal_cursor(con), 0)
        con.execute("INSERT INTO pt_event_log (id) VALUES (7)")
        self.assertEqual(server.journal_cursor(con), 7)


class ReadJsonBodyTests(unittest.TestCase):
    """POST body length and shape checks run before any route work
    (PT-2121 finding 10)."""

    @staticmethod
    def _handler(length, payload):
        codes = []
        handler = SimpleNamespace(
            headers={"Content-Length": length},
            rfile=io.BytesIO(payload),
            _json=lambda body, code: codes.append(code),
        )
        return handler, codes

    def test_negative_content_length_is_rejected_without_reading(self):
        handler, codes = self._handler("-1", b" " * (server.MAX_POST_BYTES + 1))
        self.assertIsNone(server.Handler._read_json_body(handler))
        self.assertEqual(codes, [400])
        self.assertTrue(handler.rfile.read(), "read(-1) would have drained the body")

    def test_oversized_content_length_is_413(self):
        handler, codes = self._handler(str(server.MAX_POST_BYTES + 1), b"x")
        self.assertIsNone(server.Handler._read_json_body(handler))
        self.assertEqual(codes, [413])

    def test_non_object_or_undecodable_bodies_get_a_400(self):
        for raw in (b"[]", b"1", b"null", b'"\xff"', b"\xff\xfe"):
            handler, codes = self._handler(str(len(raw)), raw)
            self.assertIsNone(server.Handler._read_json_body(handler), raw)
            self.assertEqual(codes, [400], raw)

    def test_object_body_is_returned(self):
        raw = b'{"password":1}'
        handler, codes = self._handler(str(len(raw)), raw)
        self.assertEqual(server.Handler._read_json_body(handler), {"password": 1})
        self.assertEqual(codes, [])

    def test_request_socket_has_a_timeout(self):
        self.assertNotIn(getattr(server.Handler, "timeout", None), (None, 0))


def _empty_dashboard_db(path: Path) -> None:
    con = sqlite3.connect(path)
    con.execute("CREATE TABLE tasks (%s)" % ", ".join(server.TASK_COLS))
    con.execute("CREATE TABLE pt_extensions (task_uuid TEXT, pt_id TEXT)")
    con.execute("CREATE TABLE task_labels (task_uuid TEXT, label TEXT)")
    con.execute(
        """
        CREATE TABLE pt_event_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            uuid TEXT NOT NULL UNIQUE,
            task_uuid TEXT,
            event_type TEXT NOT NULL,
            payload TEXT NOT NULL,
            ts TEXT NOT NULL,
            actor TEXT
        )
        """
    )
    con.commit()
    con.close()


def _header_map(response) -> dict:
    return {k.lower(): v for k, v in response.getheaders()}


class OpenAccessTests(unittest.TestCase):
    """The tailnet is the gate: UI paths return 200 with no credentials."""

    def setUp(self):
        self.td = tempfile.TemporaryDirectory()
        db = Path(self.td.name) / "tasks.db"
        _empty_dashboard_db(db)
        fake = Path(self.td.name) / "pt"
        fake.write_text(
            "#!/usr/bin/env python3\n"
            "import json, sys\n"
            "args = sys.argv[1:]\n"
            "if 'approval' in args and 'ls' in args:\n"
            "    print('[]')\n"
            "else:\n"
            "    print(json.dumps({'ok': True, 'id': 'u-1', 'pt_id': 'PT-1'}))\n"
        )
        fake.chmod(0o755)
        self.saved = (server.DB_PATH, server.PT_BIN)
        server.DB_PATH = str(db)
        server.PT_BIN = str(fake)
        self.httpd = server.ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.httpd.shutdown()
        self.thread.join(timeout=2)
        self.httpd.server_close()
        server.DB_PATH, server.PT_BIN = self.saved
        self.td.cleanup()

    def request(self, method, path, body=None, headers=None, timeout=5):
        connection = http.client.HTTPConnection(
            "127.0.0.1", self.httpd.server_port, timeout=timeout,
        )
        payload = json.dumps(body).encode() if body is not None else None
        merged = dict(headers or {})
        if payload is not None:
            merged.setdefault("Content-Type", "application/json")
        connection.request(method, path, body=payload, headers=merged)
        response = connection.getresponse()
        data = response.read()
        result = response.status, _header_map(response), data
        connection.close()
        return result

    def test_semver(self):
        self.assertRegex(server.VERSION, r"^\d+\.\d+\.\d+$")
        self.assertGreater(server.MAX_POST_BYTES, 400)

    def test_ui_gets_and_stream_are_open_without_credentials(self):
        os.environ["PTASK_DASH_PASS"] = "must-not-gate"
        os.environ["PTASK_DASH_USER"] = "ops"
        try:
            for path in (
                "/",
                "/api/config",
                "/api/stats",
                "/api/tasks",
                "/api/critical",
                "/api/timeline",
                "/api/heatmap",
                "/api/approvals",
                "/version",
                "/apple-touch-icon.png",
                "/icon-192.png",
                "/icon-512.png",
                "/manifest.webmanifest",
                "/api/tasks/00000000-0000-0000-0000-000000000001/events",
            ):
                status, headers, body = self.request("GET", path)
                self.assertEqual(status, 200, path)
                self.assertNotIn("www-authenticate", headers)
                self.assertGreater(len(body), 0, path)
        finally:
            os.environ.pop("PTASK_DASH_PASS", None)
            os.environ.pop("PTASK_DASH_USER", None)

        connection = http.client.HTTPConnection(
            "127.0.0.1", self.httpd.server_port, timeout=2,
        )
        try:
            connection.request("GET", "/api/stream")
            response = connection.getresponse()
            self.assertEqual(response.status, 200)
            self.assertNotIn("www-authenticate", _header_map(response))
            self.assertIn("text/event-stream", response.getheader("Content-Type"))
            chunk = response.read(14)
            self.assertEqual(chunk, b"retry: 15000\n\n")
        finally:
            connection.close()

    def test_login_and_logout_redirect_home(self):
        for path in ("/login", "/logout"):
            status, headers, _ = self.request("GET", path)
            self.assertEqual(status, 302, path)
            self.assertEqual(headers.get("location"), "/")
            self.assertNotIn("www-authenticate", headers)
            status, headers, _ = self.request("POST", path, {})
            self.assertEqual(status, 302, path)
            self.assertEqual(headers.get("location"), "/")

    def test_index_has_no_login_shell(self):
        html = (Path(server.__file__).parent / "www" / "index.html").read_text()
        self.assertNotIn('id="authGate"', html)
        self.assertNotIn('id="authPassword"', html)
        self.assertNotIn('id="authForm"', html)
        self.assertNotIn("face-unlock", html)
        self.assertNotIn("/api/auth/", html)
        self.assertNotIn('id="logout-btn"', html)
        self.assertNotIn('type="password"', html)

    def test_sidecar_source_has_no_human_auth(self):
        src = Path(server.__file__).read_text()
        self.assertNotIn("PTASK_DASH_PASS", src)
        self.assertNotIn("PTASK_DASH_USER", src)
        self.assertNotIn("WWW-Authenticate", src)
        self.assertNotIn("session_auth", src)
        self.assertIn("PTASK_ACTOR", src)

    def test_same_origin_post_without_credentials_is_not_gated(self):
        origin = f"http://127.0.0.1:{self.httpd.server_port}"
        status, headers, body = self.request(
            "POST", "/api/tasks",
            {"title": "tailnet open task"},
            {"Origin": origin},
        )
        self.assertNotIn(status, (401, 403), body)
        self.assertNotIn("www-authenticate", headers)


class EditFailureTests(unittest.TestCase):
    def test_failed_edit_does_not_run_priority_mutation(self):
        httpd = server.ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        connection = http.client.HTTPConnection("127.0.0.1", httpd.server_port)
        try:
            with mock.patch.object(
                server, "pt_exec", side_effect=[(False, "cannot clear recurring deadline"), (True, "priority changed")],
            ) as execute:
                connection.request("POST", "/api/tasks/PT-1/edit", body=json.dumps({
                    "deadline": None, "priority": 4,
                }), headers={"Content-Type": "application/json"})
                response = connection.getresponse()
                response.read()
                self.assertEqual(response.status, 500)
                self.assertEqual(execute.call_count, 1)
                self.assertEqual(execute.call_args.args[0][0], "edit")
        finally:
            connection.close()
            httpd.shutdown()
            thread.join(timeout=2)
            httpd.server_close()


class OriginTests(unittest.TestCase):
    def test_cross_origin_post_is_rejected_before_mutation(self):
        old_pt_exec = server.pt_exec
        calls = []
        httpd = server.ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        server.pt_exec = lambda args: calls.append(args) or (True, "ok")
        thread.start()
        connection = http.client.HTTPConnection("127.0.0.1", httpd.server_port)
        body = b'{"title":"must not be created"}'

        try:
            connection.request(
                "POST",
                "/api/tasks",
                body=body,
                headers={
                    "Content-Type": "application/json",
                    "Host": f"127.0.0.1:{httpd.server_port}",
                    "Origin": "https://attacker.invalid",
                },
            )
            response = connection.getresponse()
            response.read()
            self.assertEqual(response.status, 403)
            self.assertNotIn("www-authenticate", _header_map(response))
            self.assertEqual(calls, [])
        finally:
            connection.close()
            httpd.shutdown()
            thread.join(timeout=2)
            httpd.server_close()
            server.pt_exec = old_pt_exec

    def test_origin_guard_allows_non_browser_same_origin_and_tailnet_host(self):
        handler = object.__new__(server.Handler)
        handler.headers = {"Host": "ptask.example"}
        self.assertTrue(handler._origin_ok())
        handler.headers["Origin"] = "https://ptask.example"
        self.assertTrue(handler._origin_ok())
        handler.headers = {
            "Host": "ptask.tail07f9ef.ts.net",
            "Origin": "https://ptask.tail07f9ef.ts.net",
        }
        self.assertTrue(handler._origin_ok())
        handler.headers["Origin"] = "https://attacker.invalid"
        self.assertFalse(handler._origin_ok())


class QueryLimitTests(unittest.TestCase):
    def test_parse_limit_clamps_negative_and_excessive_values(self):
        self.assertEqual(server.parse_limit("-1", 500, 5000), 1)
        self.assertEqual(server.parse_limit("999999", 500, 5000), 5000)

    def test_parse_limit_rejects_non_integer_values(self):
        with self.assertRaises(ValueError):
            server.parse_limit("many", 500, 5000)


class EventHistoryTests(unittest.TestCase):
    def test_q_task_events_reads_attributed_log(self):
        old_db = server.DB_PATH
        with tempfile.NamedTemporaryFile(suffix=".db") as f:
            con = sqlite3.connect(f.name)
            con.execute(
                """
                CREATE TABLE pt_event_log (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    uuid TEXT NOT NULL UNIQUE,
                    task_uuid TEXT,
                    event_type TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    ts TEXT NOT NULL,
                    actor TEXT
                )
                """
            )
            con.execute(
                """
                INSERT INTO pt_event_log(uuid, task_uuid, event_type, payload, ts, actor)
                VALUES (?, ?, ?, ?, ?, ?)
                """,
                ("evt-1", "PT-1", "task.status.changed", '{"to":"done"}',
                 "2026-07-08T12:00:00+00:00", "hal"),
            )
            con.commit()
            con.close()
            server.DB_PATH = f.name
            try:
                events = server.q_task_events("PT-1")
            finally:
                server.DB_PATH = old_db
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]["actor"], "hal")
        self.assertEqual(events[0]["payload"], {"to": "done"})


class StatsFluxTests(unittest.TestCase):
    def test_q_stats_reports_windowed_flux_split_by_origin(self):
        old_db = server.DB_PATH
        with tempfile.NamedTemporaryFile(suffix=".db") as f:
            con = sqlite3.connect(f.name)
            con.execute(
                """
                CREATE TABLE tasks (
                    id TEXT PRIMARY KEY, title TEXT, priority INTEGER,
                    status TEXT, task_type TEXT, source_type TEXT,
                    created_at TEXT, updated_at TEXT, deadline TEXT
                )
                """
            )
            rows = [
                # added just now: human = operator-typed OR Claude-Code-on-ask.
                ("t1", "manual fresh", 2, "pending", "operational", "manual",
                 "+0 seconds", "+0 seconds"),           # human (operator)
                ("t2", "mcp fresh", 2, "pending", "operational", "mcp",
                 "+0 seconds", "+0 seconds"),           # human (claude/HAL on ask)
                ("t3", "distilled fresh done", 2, "done", "operational",
                 "distilled", "+0 seconds", "+0 seconds"),  # ROBOT (auto)
                # added 3 days ago (robot): in the 7d window, NOT the 24h one
                ("t6", "incident 3d ago", 2, "pending", "operational",
                 "incident", "-3 days", "-3 days"),     # ROBOT (auto)
                # old task completed just now (counts in done for every window)
                ("t4", "old but just done", 2, "done", "operational",
                 "claude_code", "-10 days", "+0 seconds"),
                # old and long done: counts in neither add nor recent-done
                ("t5", "ancient", 2, "done", "operational", "manual",
                 "-10 days", "-9 days"),
            ]
            for tid, title, pri, status, ttype, src, c_at, u_at in rows:
                con.execute(
                    "INSERT INTO tasks VALUES (?,?,?,?,?,?,"
                    "datetime('now', ?), datetime('now', ?), NULL)",
                    (tid, title, pri, status, ttype, src, c_at, u_at),
                )
            con.commit()
            con.close()
            server.DB_PATH = f.name
            try:
                stats = server.q_stats()
            finally:
                server.DB_PATH = old_db
        flux = stats["flux"]
        self.assertEqual(flux["windows"], ["30m", "1h", "6h", "24h", "7d"])
        w24 = flux["by_window"]["24h"]
        self.assertEqual(w24["added"], 3)          # t1,t2,t3 (t6 is 3d old)
        self.assertEqual(w24["added_human"], 2)    # t1 manual, t2 mcp
        self.assertEqual(w24["added_robot"], 1)    # t3 distilled
        self.assertEqual(w24["done"], 2)           # t3,t4
        w7 = flux["by_window"]["7d"]
        self.assertEqual(w7["added"], 4)           # + t6 pulled in by wider window
        self.assertEqual(w7["added_human"], 2)     # still t1,t2
        self.assertEqual(w7["added_robot"], 2)     # t3 distilled + t6 incident
        self.assertEqual(w7["done"], 2)            # t5 still older than 7d


class TaskOrderTests(unittest.TestCase):
    def test_order_created_returns_newest_first_across_statuses(self):
        old_db = server.DB_PATH
        with tempfile.NamedTemporaryFile(suffix=".db") as f:
            con = sqlite3.connect(f.name)
            con.execute("CREATE TABLE tasks (%s)" % ", ".join(server.TASK_COLS))
            con.execute("CREATE TABLE pt_extensions (task_uuid TEXT, pt_id TEXT)")
            con.execute("CREATE TABLE task_labels (task_uuid TEXT, label TEXT)")
            for tid, created, status in (
                ("a", "-2 days", "pending"),
                ("b", "-1 hour", "done"),       # closed tasks stay in the feed
                ("c", "-5 minutes", "pending"),
            ):
                con.execute(
                    "INSERT INTO tasks(id, title, priority, status, created_at,"
                    " priority_score) VALUES (?,?,?,?,datetime('now',?),0.5)",
                    (tid, "task " + tid, 2, status, created),
                )
            con.execute("INSERT INTO task_labels VALUES ('c', 'domain:mgmt')")
            con.execute("INSERT INTO task_labels VALUES ('c', 'finance')")
            con.commit()
            con.close()
            server.DB_PATH = f.name
            try:
                got = server.q_tasks(status="all", limit=10,
                                     order=server.TASK_ORDERS["created"])
            finally:
                server.DB_PATH = old_db
        self.assertEqual([t["id"] for t in got], ["c", "b", "a"])
        self.assertEqual(got[1]["status"], "done")
        # labels arrive as a real array (json_group_array unpacked), and a
        # task with no label rows gets [] rather than null.
        self.assertEqual(sorted(got[0]["labels"]), ["domain:mgmt", "finance"])
        self.assertEqual(got[2]["labels"], [])

    def test_order_whitelist_is_closed(self):
        # The route splices TASK_ORDERS values into SQL; anything outside the
        # whitelist must 400 at the handler, so the map itself is the contract.
        self.assertEqual(sorted(server.TASK_ORDERS),
                         ["created", "score", "severity"])

    def test_default_order_is_severity_first(self):
        # The board fetches /api/tasks with no `order`, so the default decides
        # what the Critical panel and the lanes show. Severity must lead;
        # priority_score is only the within-band tiebreaker.
        self.assertEqual(server.DEFAULT_TASK_ORDER, "severity")
        order = server.TASK_ORDERS[server.DEFAULT_TASK_ORDER]
        self.assertLess(order.index("priority DESC"),
                        order.index("priority_score DESC"))

    def test_severity_order_puts_a_critical_above_a_high_scoring_normal(self):
        rows = [
            # id, priority, priority_score
            ("normal-hot", 2, 0.95),
            ("critical-cold", 5, 0.10),
            ("high-mid", 3, 0.50),
        ]
        old_db = server.DB_PATH
        with tempfile.NamedTemporaryFile(suffix=".db") as f:
            con = sqlite3.connect(f.name)
            con.execute("CREATE TABLE tasks (%s)" % ", ".join(server.TASK_COLS))
            con.execute("CREATE TABLE pt_extensions (task_uuid TEXT, pt_id TEXT)")
            con.execute("CREATE TABLE task_labels (task_uuid TEXT, label TEXT)")
            for tid, prio, score in rows:
                con.execute(
                    "INSERT INTO tasks(id, title, priority, status, created_at,"
                    " priority_score) VALUES (?,?,?,'pending',datetime('now'),?)",
                    (tid, tid, prio, score),
                )
            con.commit()
            con.close()
            server.DB_PATH = f.name
            try:
                got = server.q_tasks(status="pending", limit=10)
            finally:
                server.DB_PATH = old_db
        self.assertEqual([t["id"] for t in got],
                         ["critical-cold", "high-mid", "normal-hot"])


class BuildEditArgsTests(unittest.TestCase):
    def test_full_field_set_builds_pt_edit_argv(self):
        args, err = server.build_edit_args("PT-9", {
            "title": "new title",
            "description": "new body",
            "deadline": "2026-08-15",
            "labels_add": ["domain:mgmt"],
            "labels_remove": ["domain:eng"],
        })
        self.assertIsNone(err)
        self.assertEqual(args, [
            "edit", "PT-9", "--title=new title", "--desc=new body",
            "--deadline=2026-08-15", "--label=domain:mgmt",
            "--unlabel=domain:eng",
        ])

    def test_null_deadline_clears_absent_leaves_untouched(self):
        args, _ = server.build_edit_args("PT-9", {"deadline": None})
        self.assertEqual(args, ["edit", "PT-9", "--clear-deadline"])
        args, err = server.build_edit_args("PT-9", {"title": "just a title"})
        self.assertIsNone(err)
        self.assertNotIn("--clear-deadline", args)

    def test_priority_only_body_returns_no_args_no_error(self):
        # priority is delegated to `pt priority` by the route, not this builder
        args, err = server.build_edit_args("PT-9", {"priority": 4})
        self.assertIsNone(args)
        self.assertIsNone(err)

    def test_bad_labels_rejected(self):
        for bad in (["has space"], [""], ["x" * 65], "notalist", [42],
                    ["ok"] * 17):
            args, err = server.build_edit_args("PT-9", {"labels_add": bad})
            self.assertIsNone(args, f"labels_add={bad!r} should fail")
            self.assertIn("labels_add", err)

    def test_invalid_deadline_rejected(self):
        args, err = server.build_edit_args("PT-9", {"deadline": "tomorrow"})
        self.assertIsNone(args)
        self.assertIn("deadline", err)


class BuildAddArgsTests(unittest.TestCase):
    def test_title_only_is_separated(self):
        args, err = server.build_add_args({"title": "ship it"})
        self.assertIsNone(err)
        self.assertEqual(args, ["add", "--", "ship it"])

    def test_full_payload_builds_explicit_flags(self):
        args, err = server.build_add_args({
            "title": "redesign pNOC",
            "description": "cap CPU",
            "priority": 4,
            "deadline": "2026-07-20",
        })
        self.assertIsNone(err)
        self.assertEqual(args, [
            "add", "--priority=4", "--description=cap CPU",
            "--deadline=2026-07-20", "--", "redesign pNOC",
        ])

    def test_leading_dash_values_stay_after_separator_or_equals(self):
        # hyphen-safe: title via `--`, description via `--opt=value`
        args, err = server.build_add_args(
            {"title": "-weird", "description": "- bullet"})
        self.assertIsNone(err)
        self.assertEqual(args, ["add", "--description=- bullet", "--", "-weird"])

    def test_blank_optional_fields_are_omitted(self):
        args, err = server.build_add_args(
            {"title": "task", "description": "   ", "deadline": ""})
        self.assertIsNone(err)
        self.assertEqual(args, ["add", "--", "task"])

    def test_short_title_rejected(self):
        args, err = server.build_add_args({"title": "ab"})
        self.assertIsNone(args)
        self.assertIn("title", err)

    def test_priority_must_be_int_1_to_5(self):
        for bad in (0, 6, 9, True, "4", 3.5):
            args, err = server.build_add_args({"title": "task", "priority": bad})
            self.assertIsNone(args, f"priority={bad!r} should fail")
            self.assertIn("priority", err)

    def test_invalid_deadline_rejected(self):
        for bad in ("2026-13-99", "2026/07/20", "tomorrow", "20-07-2026"):
            args, err = server.build_add_args({"title": "task", "deadline": bad})
            self.assertIsNone(args, f"deadline={bad!r} should fail")
            self.assertIn("deadline", err)

    def test_overlong_description_rejected(self):
        args, err = server.build_add_args(
            {"title": "task", "description": "x" * 4001})
        self.assertIsNone(args)
        self.assertIn("description", err)


class VoiceJsonTests(unittest.TestCase):
    def test_plain_json(self):
        self.assertEqual(server._extract_json('{"a": 1}'), {"a": 1})

    def test_json_in_markdown_fence(self):
        self.assertEqual(server._extract_json('```json\n{"a": 1}\n```'), {"a": 1})
        self.assertEqual(server._extract_json('```\n{"a": 2}\n```'), {"a": 2})

    def test_json_with_surrounding_prose(self):
        self.assertEqual(
            server._extract_json('Sure! Here it is:\n{"title": "x"}\nHope that helps.'),
            {"title": "x"})

    def test_garbage_returns_empty(self):
        for bad in ("", "no json here", "{not valid}", None):
            self.assertEqual(server._extract_json(bad), {})


class VoiceFieldsTests(unittest.TestCase):
    def test_full_payload_passthrough(self):
        out = server._normalize_voice_fields(
            {"title": "Redesign dashboard.", "description": "do it",
             "priority": 4, "deadline": "2026-07-03", "labels": ["pnoc", "UI!!"]},
            "redesign the dashboard")
        self.assertEqual(out["title"], "Redesign dashboard")   # trailing period stripped
        self.assertEqual(out["description"], "do it")
        self.assertEqual(out["priority"], 4)
        self.assertEqual(out["deadline"], "2026-07-03")
        self.assertEqual(out["labels"], ["pnoc", "ui"])        # sanitized lowercase

    def test_missing_title_falls_back_to_transcript(self):
        out = server._normalize_voice_fields({}, "  fix the broken thing  ")
        self.assertEqual(out["title"], "fix the broken thing")
        self.assertEqual(out["priority"], 2)                   # default NORMAL
        self.assertIsNone(out["deadline"])
        self.assertEqual(out["labels"], [])

    def test_priority_clamped_and_defaulted(self):
        self.assertEqual(server._normalize_voice_fields({"title": "abc", "priority": 9}, "t")["priority"], 5)
        self.assertEqual(server._normalize_voice_fields({"title": "abc", "priority": 0}, "t")["priority"], 1)
        self.assertEqual(server._normalize_voice_fields({"title": "abc", "priority": "x"}, "t")["priority"], 2)

    def test_invalid_deadline_dropped(self):
        for bad in ("2026-13-40", "next friday", "07/03/2026", ""):
            out = server._normalize_voice_fields({"title": "abc", "deadline": bad}, "t")
            self.assertIsNone(out["deadline"], f"deadline={bad!r} should be dropped")

    def test_labels_capped_and_non_dict_safe(self):
        out = server._normalize_voice_fields(
            {"title": "abc", "labels": ["a", "b", "c", "d", "e", "f"]}, "t")
        self.assertLessEqual(len(out["labels"]), 4)
        safe = server._normalize_voice_fields("not a dict", "fallback title here")
        self.assertEqual(safe["title"], "fallback title here")
        self.assertEqual(safe["priority"], 2)


class PublicAssetTests(unittest.TestCase):
    """PWA assets and the board HTML are served without a login gate."""

    def test_touch_icon_manifest_and_board_are_open(self):
        httpd = server.ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        try:
            for path, ctype in (("/apple-touch-icon.png", "image/png"),
                                ("/icon-192.png", "image/png"),
                                ("/icon-512.png", "image/png"),
                                ("/manifest.webmanifest", None)):
                connection = http.client.HTTPConnection("127.0.0.1", httpd.server_port)
                connection.request("GET", path)
                response = connection.getresponse()
                self.assertEqual(response.status, 200, path)
                if ctype:
                    self.assertEqual(response.getheader("Content-Type"), ctype, path)
                self.assertGreater(len(response.read()), 100, path)
                connection.close()
            connection = http.client.HTTPConnection("127.0.0.1", httpd.server_port)
            connection.request("GET", "/")
            response = connection.getresponse()
            body = response.read()
            self.assertEqual(response.status, 200)
            self.assertNotIn("www-authenticate", _header_map(response))
            self.assertNotIn(b'id="authGate"', body)
            connection.close()
        finally:
            httpd.shutdown()
            httpd.server_close()


class VoiceDomainTests(unittest.TestCase):
    """v0.15.0: the extractor also picks the ENG/MGMT hemisphere."""

    def test_domain_accepted_and_normalized(self):
        for raw, want in (("eng", "eng"), ("  MGMT ", "mgmt"), ("Eng", "eng")):
            out = server._normalize_voice_fields({"title": "abc def", "domain": raw}, "t")
            self.assertEqual(out["domain"], want)

    def test_unknown_domain_is_dropped_not_guessed(self):
        # A dropped domain leaves the cockpit's domainOf() heuristic in charge —
        # one classifier for the whole board rather than a second server-side copy.
        for bad in ("engineering", "ops", "", None, 3, "both"):
            out = server._normalize_voice_fields({"title": "abc def", "domain": bad}, "t")
            self.assertIsNone(out["domain"], f"domain={bad!r} should be dropped")

    def test_configured_domains_are_accepted_and_legacy_pair_dropped(self):
        # v0.19 configured the board's hats; voice capture still only honoured
        # eng/mgmt, so a second-tenant dictation could never land on a real hat
        # and would still stamp @domain:eng on a board that does not have one.
        saved = server.DASH_DOMAINS
        server.DASH_DOMAINS = server.parse_domains(
            "puretensor:PureTensor:PT,personal:Personal:ME")
        try:
            out = server._normalize_voice_fields(
                {"title": "abc def", "domain": "personal"}, "t")
            self.assertEqual(out["domain"], "personal")
            out = server._normalize_voice_fields(
                {"title": "abc def", "domain": "  PURETENSOR "}, "t")
            self.assertEqual(out["domain"], "puretensor")
            for bad in ("eng", "mgmt", "ops", None):
                out = server._normalize_voice_fields(
                    {"title": "abc def", "domain": bad}, "t")
                self.assertIsNone(out["domain"], f"domain={bad!r} should be dropped")
        finally:
            server.DASH_DOMAINS = saved

    def test_voice_prompt_lists_configured_keys(self):
        saved = server.DASH_DOMAINS
        server.DASH_DOMAINS = server.parse_domains("personal:Personal:ME")
        try:
            prompt = server.voice_system_prompt("2026-09-11")
            self.assertIn("2026-09-11", prompt)
            self.assertIn('"personal"', prompt)
            self.assertNotIn('"eng"', prompt)
            self.assertNotIn('"mgmt"', prompt)
        finally:
            server.DASH_DOMAINS = saved
        # Unset config keeps the legacy hemisphere prompt byte-for-byte.
        prompt = server.voice_system_prompt("2026-09-11")
        self.assertIn('"eng"', prompt)
        self.assertIn('"mgmt"', prompt)

    def test_reason_collapsed_and_capped(self):
        out = server._normalize_voice_fields(
            {"title": "abc def", "reason": "  a\n  b   c  "}, "t")
        self.assertEqual(out["reason"], "a b c")
        long = server._normalize_voice_fields({"title": "abc def", "reason": "x" * 500}, "t")
        self.assertEqual(len(long["reason"]), 200)


class TranscriptGuardTests(unittest.TestCase):
    """Silence and Whisper artefacts must never reach the create path."""

    def test_real_dictation_passes(self):
        for good in ("rotate the cloudflare api token",
                     "file the VAT return with HMRC",
                     "fix the DNS"):
            self.assertTrue(server.transcript_is_speech(good), good)

    def test_silence_and_artefacts_rejected(self):
        for bad in ("", "   ", "you", "Okay.", "Bye bye", "Thank you.",
                    "Thank you for watching!", "[Music]", "(upbeat music)",
                    "Subtitles by the Amara.org community", "www.mooji.org",
                    "谢谢大家的观看请订阅我的频道"):
            self.assertFalse(server.transcript_is_speech(bad), repr(bad))


class VoiceCreateTests(unittest.TestCase):
    """POST /api/voice/task turns drafted fields into a real `pt add`."""

    def test_created_ids_prefers_json_then_falls_back_to_text(self):
        as_json = '{"id": "abc-123", "pt_id": "PT-9"}'
        self.assertEqual(server._created_ids(as_json), ("PT-9", "abc-123"))
        # a `pt` binary that predates --json on add still prints the human form
        as_text = ("Task created [URGENT]: x\n  PT-42\n  ID: 11112222-3333\n"
                   "  Priority: 4 (urgent)\n")
        self.assertEqual(server._created_ids(as_text), ("PT-42", "11112222-3333"))
        self.assertEqual(server._created_ids("nothing useful"), (None, None))

    def _capture_argv(self, fields, transcript="rotate the token now please"):
        seen = {}

        def fake_exec(args):
            seen["args"] = args
            return True, '{"id": "u-1", "pt_id": "PT-7"}'

        real = server.pt_exec
        server.pt_exec = fake_exec
        try:
            ok, out = server.voice_create_task(fields, transcript)
        finally:
            server.pt_exec = real
        return ok, out, seen["args"]

    def test_domain_rides_as_an_inline_quickadd_token(self):
        ok, out, args = self._capture_argv(
            {"title": "Rotate the API token", "priority": 4, "domain": "eng",
             "reason": "infra work", "description": "", "deadline": None})
        self.assertTrue(ok)
        self.assertEqual((out["pt_id"], out["id"]), ("PT-7", "u-1"))
        self.assertEqual(args[-1], "Rotate the API token @domain:eng")
        self.assertIn("--json", args)
        self.assertIn("--priority=4", args)
        reason = next(a for a in args if a.startswith("--reason="))
        self.assertIn("infra work", reason)
        self.assertIn("voice: rotate the token now please", reason)

    def test_no_domain_means_no_token(self):
        _, _, args = self._capture_argv(
            {"title": "Something ambiguous", "priority": 2, "domain": None,
             "reason": "", "description": "", "deadline": None})
        self.assertEqual(args[-1], "Something ambiguous")
        self.assertFalse(any("@domain:" in a for a in args))

    def test_configured_domain_rides_as_an_inline_quickadd_token(self):
        saved = server.DASH_DOMAINS
        server.DASH_DOMAINS = server.parse_domains("personal:Personal:ME")
        try:
            ok, _, args = self._capture_argv(
                {"title": "File the VAT return", "priority": 4, "domain": "personal",
                 "reason": "tax", "description": "", "deadline": None})
            self.assertTrue(ok)
            self.assertEqual(args[-1], "File the VAT return @domain:personal")
        finally:
            server.DASH_DOMAINS = saved

    def test_token_dropped_rather_than_overflowing_the_title(self):
        title = "x" * 396          # 396 + len(" @domain:eng")=12 -> 408 > 400
        _, _, args = self._capture_argv(
            {"title": title, "priority": 2, "domain": "eng",
             "reason": "", "description": "", "deadline": None})
        self.assertEqual(args[-1], title)

    def test_pt_failure_is_reported_not_swallowed(self):
        real = server.pt_exec
        server.pt_exec = lambda args: (False, "no such column")
        try:
            ok, out = server.voice_create_task(
                {"title": "abc def", "priority": 2, "domain": None,
                 "reason": "", "description": "", "deadline": None}, "t")
        finally:
            server.pt_exec = real
        self.assertFalse(ok)
        self.assertEqual(out["error"], "no such column")


class DomainConfigTests(unittest.TestCase):
    """PTASK_DASH_DOMAINS turns the hardcoded ENG/MGMT hemisphere switch into a
    per-instance list, so a second tenant (a non-engineer) can run the same
    cockpit with their own hats. Unset = the legacy ENG/MGMT heuristic mode,
    byte-for-byte the previous behaviour."""

    def test_unset_or_blank_means_legacy_mode(self):
        self.assertEqual(server.parse_domains(None), [])
        self.assertEqual(server.parse_domains(""), [])
        self.assertEqual(server.parse_domains("  , "), [])

    def test_trailing_and_blank_comma_entries_are_ignored(self):
        # Env files and systemd Environment= lines routinely trail a comma.
        # An empty slot is not a domain key; it must not crash dashboard import.
        got = server.parse_domains("personal,")
        self.assertEqual(got, [{"key": "personal", "label": "Personal", "abbr": "PERS"}])
        got = server.parse_domains("a:A,,b:B,")
        self.assertEqual([d["key"] for d in got], ["a", "b"])
        got = server.parse_domains(",personal")
        self.assertEqual(got[0]["key"], "personal")

    def test_parses_key_label_abbr_triples(self):
        got = server.parse_domains(
            "puretensor:PureTensor:PT, bretalon:Bretalon:BRET,eaglestone:Eaglestone:EAGLE"
        )
        self.assertEqual(got, [
            {"key": "puretensor", "label": "PureTensor", "abbr": "PT"},
            {"key": "bretalon", "label": "Bretalon", "abbr": "BRET"},
            {"key": "eaglestone", "label": "Eaglestone", "abbr": "EAGLE"},
        ])

    def test_label_and_abbr_default_from_key(self):
        got = server.parse_domains("personal")
        self.assertEqual(got, [{"key": "personal", "label": "Personal", "abbr": "PERS"}])
        got = server.parse_domains("diloretio:Diloretio")
        self.assertEqual(got[0]["abbr"], "DILO")

    def test_rejects_reserved_duplicate_and_malformed_keys(self):
        for bad in ("all:Everything", "eng:Engineering,eng:Again", "Bad Key:x",
                    "toolongtoolongtoolongtoolongtoolong:x", ":NoKey", "x:y:z:extra"):
            with self.assertRaises(ValueError, msg=bad):
                server.parse_domains(bad)

    def test_abbr_is_capped_at_five_chars(self):
        with self.assertRaises(ValueError):
            server.parse_domains("k:Label:TOOLONG")

    def test_default_domain_must_be_a_configured_key(self):
        doms = server.parse_domains("a:A,b:B")
        self.assertEqual(server.resolve_default_domain(doms, None), "a")
        self.assertEqual(server.resolve_default_domain(doms, "b"), "b")
        with self.assertRaises(ValueError):
            server.resolve_default_domain(doms, "zzz")
        self.assertIsNone(server.resolve_default_domain([], "anything"))

    def test_blank_requested_default_uses_the_first_key(self):
        # PTASK_DASH_DEFAULT_DOMAIN=  (set-but-empty in an env file) is not
        # "the operator picked a missing hat"; it is "use the first". Raising
        # here aborts dashboard import the same way a trailing comma did.
        doms = server.parse_domains("a:A,b:B")
        self.assertEqual(server.resolve_default_domain(doms, ""), "a")
        self.assertEqual(server.resolve_default_domain(doms, "  "), "a")


class ConfigEndpointTests(unittest.TestCase):
    """GET /api/config is the ONE place the shell learns its brand and domain
    list. It carries nothing secret."""

    def _boot(self):
        httpd = server.ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        return httpd

    def _get(self, httpd, path):
        connection = http.client.HTTPConnection("127.0.0.1", httpd.server_port)
        connection.request("GET", path)
        response = connection.getresponse()
        body = response.read()
        connection.close()
        return response, body

    def test_config_is_public_and_reflects_env(self):
        saved = (server.DASH_TITLE, server.DASH_DOMAINS, server.DASH_DEFAULT_DOMAIN)
        server.DASH_TITLE = "ALAN"
        server.DASH_DOMAINS = server.parse_domains("puretensor:PureTensor:PT,personal:Personal:ME")
        server.DASH_DEFAULT_DOMAIN = server.resolve_default_domain(server.DASH_DOMAINS, "personal")
        httpd = self._boot()
        try:
            response, body = self._get(httpd, "/api/config")
            self.assertEqual(response.status, 200)
            self.assertEqual(response.getheader("Cache-Control"), "no-store")
            self.assertNotIn("www-authenticate", _header_map(response))
            cfg = json.loads(body)
            self.assertEqual(cfg["title"], "ALAN")
            self.assertEqual(cfg["default_domain"], "personal")
            self.assertEqual([d["key"] for d in cfg["domains"]], ["puretensor", "personal"])
            self.assertEqual(cfg["domains"][1]["abbr"], "ME")
            self.assertEqual(cfg["version"], server.VERSION)
            self.assertEqual(set(cfg), {"title", "domains", "default_domain", "version"})
        finally:
            httpd.shutdown()
            httpd.server_close()
            (server.DASH_TITLE, server.DASH_DOMAINS, server.DASH_DEFAULT_DOMAIN) = saved

    def test_legacy_mode_reports_no_domains_and_ptask_title(self):
        saved = (server.DASH_TITLE, server.DASH_DOMAINS, server.DASH_DEFAULT_DOMAIN)
        server.DASH_TITLE, server.DASH_DOMAINS, server.DASH_DEFAULT_DOMAIN = "PTASK", [], None
        httpd = self._boot()
        try:
            _, body = self._get(httpd, "/api/config")
            cfg = json.loads(body)
            self.assertEqual(cfg["title"], "PTASK")
            self.assertEqual(cfg["domains"], [])
            self.assertIsNone(cfg["default_domain"])
        finally:
            httpd.shutdown()
            httpd.server_close()
            server.DASH_TITLE, server.DASH_DOMAINS, server.DASH_DEFAULT_DOMAIN = saved


if __name__ == "__main__":
    os.chdir(os.path.dirname(os.path.dirname(__file__)))
    unittest.main()
