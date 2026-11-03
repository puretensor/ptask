"""Close chronology is journal time, independent of creation and later edits."""
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import server
from test_server import _empty_dashboard_db


class ClosedChronologyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.db = Path(self.tmp.name) / "closed.db"
        _empty_dashboard_db(self.db)
        self.patch = mock.patch.object(server, "DB_PATH", str(self.db))
        self.patch.start()
        self.addCleanup(self.patch.stop)
        self.seq = 0

    def task(self, tid, status="done", created="2020-01-01T00:00:00Z"):
        with sqlite3.connect(self.db) as con:
            con.execute("INSERT INTO tasks(id,title,priority,status,created_at,updated_at) "
                        "VALUES (?,?,2,?,?,?)", (tid, tid, status, created, "2099-01-01T00:00:00Z"))
            con.execute("INSERT INTO pt_extensions VALUES (?,?)", (tid, "PT-" + tid))
        self.event(tid, "task.created", "2019-01-01T00:00:00Z")

    def event(self, tid, kind, when, payload=None):
        self.seq += 1
        with sqlite3.connect(self.db) as con:
            con.execute("INSERT INTO pt_event_log(uuid,task_uuid,event_type,payload,ts,actor) "
                        "VALUES (?,?,?,?,?,'hal')", (str(self.seq), tid, kind, json.dumps(payload or {}), when))

    def test_old_task_closed_now_leads_and_edits_do_not_move_older_closure(self):
        self.task("old")
        self.task("new", created="2026-11-02T00:00:00Z")
        self.event("new", "task.completed", "2026-11-03T10:00:00Z")
        self.event("old", "task.completed", "2026-11-03T12:00:00Z")
        self.event("new", "task.noted", "2026-11-03T13:00:00Z")
        self.event("new", "task.updated", "2026-11-03T14:00:00Z", {"status": "done", "title": "edited"})
        rows = server.q_closed()
        self.assertEqual([r["id"] for r in rows], ["old", "new"])
        self.assertEqual(rows[0]["closed_at"], "2026-11-03T12:00:00.000Z")
        self.assertEqual(rows[1]["closed_at"], "2026-11-03T10:00:00.000Z")
        self.assertEqual(rows[0]["closed_by"], "hal")
        self.assertEqual(rows[0]["pt_id"], "PT-old")

    def test_reopen_removes_and_reclose_starts_a_new_episode(self):
        self.task("a", "pending")
        self.task("b")
        self.event("a", "task.completed", "2026-11-03T10:00:00Z")
        self.event("b", "task.completed", "2026-11-03T11:00:00Z")
        self.event("a", "task.updated", "2026-11-03T12:00:00Z", {"status": "pending"})
        self.assertEqual([r["id"] for r in server.q_closed()], ["b"])
        with sqlite3.connect(self.db) as con:
            con.execute("UPDATE tasks SET status='done' WHERE id='a'")
        # A task row inconsistent with a journal that ends open has unknown time.
        self.assertIsNone(server.q_closed()[1]["closed_at"])
        self.event("a", "task.completed", "2026-11-03T13:00:00Z")
        rows = server.q_closed()
        self.assertEqual([r["id"] for r in rows], ["a", "b"])
        self.assertEqual(rows[0]["closed_at"], "2026-11-03T13:00:00.000Z")

    def test_offsets_ties_dismissals_and_unknown_times(self):
        for tid in ("a", "b", "c", "legacy", "invalid"):
            self.task(tid, "dismissed" if tid == "a" else "done")
        self.event("a", "task.updated", "2026-11-03T12:00:00+02:00", {"status": "dismissed"})
        self.event("b", "task.completed", "2026-11-03T10:15:00Z")
        self.event("c", "task.completed", "2026-11-03T10:15:00Z")
        self.event("invalid", "task.completed", "not-a-time")
        rows = server.q_closed()
        self.assertEqual([r["id"] for r in rows], ["c", "b", "a", "legacy", "invalid"])
        self.assertEqual(rows[2]["closed_at"], "2026-11-03T10:00:00.000Z")
        self.assertIsNone(rows[3]["closed_at"])
        self.assertIsNone(rows[4]["closed_at"])

    def test_result_limit_does_not_truncate_status_history(self):
        self.task("a")
        self.task("b")
        self.event("a", "task.completed", "2026-11-03T11:00:00Z")
        self.event("b", "task.completed", "2026-11-03T10:00:00Z")
        for i in range(220):
            self.event("b", "task.updated", f"2026-11-04T00:{i // 60:02}:{i % 60:02}Z", {"status": "done"})
        self.assertEqual([r["id"] for r in server.q_closed(limit=1)], ["a"])
        self.assertEqual(server.q_closed()[1]["closed_at"], "2026-11-03T10:00:00.000Z")

