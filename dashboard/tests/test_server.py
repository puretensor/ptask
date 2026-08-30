import base64
import http.client
import os
import sqlite3
import tempfile
import threading
import unittest

import server


class BindSafetyTests(unittest.TestCase):
    def test_parse_bind_accepts_host_port(self):
        self.assertEqual(server.parse_bind("127.0.0.1:9510"), ("127.0.0.1", 9510))

    def test_parse_bind_rejects_invalid_port(self):
        with self.assertRaises(ValueError):
            server.parse_bind("127.0.0.1:99999")

    def test_loopback_detection_is_strict(self):
        self.assertTrue(server.is_loopback_host("127.0.0.1"))
        self.assertTrue(server.is_loopback_host("localhost"))
        self.assertFalse(server.is_loopback_host("0.0.0.0"))


class AuthTests(unittest.TestCase):
    def test_compare_digest_auth_helper_importable(self):
        self.assertRegex(server.VERSION, r"^\d+\.\d+\.\d+$")
        self.assertGreater(server.MAX_POST_BYTES, 400)


class OriginTests(unittest.TestCase):
    def test_authenticated_cross_origin_post_is_rejected_before_mutation(self):
        old_user = server.AUTH_USER
        old_pass = server.AUTH_PASS
        old_pt_exec = server.pt_exec
        calls = []
        httpd = server.ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        server.AUTH_USER = "ops"
        server.AUTH_PASS = "test-secret"
        server.pt_exec = lambda args: calls.append(args) or (True, "ok")
        thread.start()
        connection = http.client.HTTPConnection("127.0.0.1", httpd.server_port)
        credentials = base64.b64encode(b"ops:test-secret").decode()
        body = b'{"title":"must not be created"}'

        try:
            connection.request(
                "POST",
                "/api/tasks",
                body=body,
                headers={
                    "Authorization": f"Basic {credentials}",
                    "Content-Type": "application/json",
                    "Host": f"127.0.0.1:{httpd.server_port}",
                    "Origin": "https://attacker.invalid",
                },
            )
            response = connection.getresponse()
            response.read()
            self.assertEqual(response.status, 403)
            self.assertEqual(calls, [])
        finally:
            connection.close()
            httpd.shutdown()
            thread.join(timeout=2)
            httpd.server_close()
            server.AUTH_USER = old_user
            server.AUTH_PASS = old_pass
            server.pt_exec = old_pt_exec

    def test_origin_guard_allows_non_browser_and_same_origin_requests(self):
        handler = object.__new__(server.Handler)
        handler.headers = {"Host": "ptask.example"}
        self.assertTrue(handler._origin_ok())
        handler.headers["Origin"] = "https://ptask.example"
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
        self.assertEqual(sorted(server.TASK_ORDERS), ["created", "score"])


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


if __name__ == "__main__":
    os.chdir(os.path.dirname(os.path.dirname(__file__)))
    unittest.main()


class PublicAssetTests(unittest.TestCase):
    """Home-screen app assets (touch icon, manifest) are served without auth —
    iOS fetches them outside the page's credentialed session. Everything else
    stays behind the Basic-auth gate."""

    def test_touch_icon_and_manifest_are_public_but_index_is_not(self):
        old_user, old_pass = server.AUTH_USER, server.AUTH_PASS
        httpd = server.ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        server.AUTH_USER, server.AUTH_PASS = "ops", "test-secret"
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
            self.assertEqual(connection.getresponse().status, 401)
            connection.close()
        finally:
            httpd.shutdown()
            httpd.server_close()
            server.AUTH_USER, server.AUTH_PASS = old_user, old_pass
