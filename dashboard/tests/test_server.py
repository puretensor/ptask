import os
import sqlite3
import tempfile
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
                # added just now: 1 operator (manual) + 2 machine
                ("t1", "manual fresh", 2, "pending", "operational", "manual",
                 "+0 seconds", "+0 seconds"),
                ("t2", "mcp fresh", 2, "pending", "operational", "mcp",
                 "+0 seconds", "+0 seconds"),
                ("t3", "distilled fresh done", 2, "done", "operational",
                 "distilled", "+0 seconds", "+0 seconds"),
                # added 3 days ago (machine): in the 7d window, NOT the 24h one
                ("t6", "machine 3d ago", 2, "pending", "operational",
                 "claude_code", "-3 days", "-3 days"),
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
        self.assertEqual(w24["added_human"], 1)    # t1
        self.assertEqual(w24["added_machine"], 2)  # t2,t3
        self.assertEqual(w24["done"], 2)           # t3,t4
        w7 = flux["by_window"]["7d"]
        self.assertEqual(w7["added"], 4)           # + t6 pulled in by wider window
        self.assertEqual(w7["added_machine"], 3)   # t2,t3,t6
        self.assertEqual(w7["done"], 2)            # t5 still older than 7d


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
