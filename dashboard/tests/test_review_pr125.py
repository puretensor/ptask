"""Contract tests from the review of puretensor/ptask#125: the sidecar's
per-actor flux (q_flux_by_actor) must agree with what each actor did to the
open-task count. Each test fails on the PR head and passes once the finding
is fixed. The journal rows mirror what the live journal holds."""

import json
import sqlite3
import tempfile
import unittest

import server


class Journal:
    """A bare pt_event_log, as the sidecar reads it."""

    def __init__(self, path):
        self.con = sqlite3.connect(path)
        self.con.execute(
            "CREATE TABLE pt_event_log (id INTEGER PRIMARY KEY, uuid TEXT, task_uuid TEXT,"
            " event_type TEXT, payload TEXT, ts TEXT, actor TEXT)")
        self.n = 0

    def add(self, task, kind, actor, age, **payload):
        """Journal one event `age` ago (an SQLite modifier such as '-5 minutes')."""
        self.n += 1
        body = {"task_uuid": task, "actor": actor, "source": "cli", **payload}
        self.con.execute(
            "INSERT INTO pt_event_log (uuid, task_uuid, event_type, payload, ts, actor)"
            " VALUES (?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%f', 'now', ?) || '+00:00', ?)",
            (f"e{self.n}", task, kind, json.dumps(body), age, actor))

    def flux(self, modifier="-1 day"):
        self.con.commit()
        return {a["actor"]: a for a in server.q_flux_by_actor(self.con, modifier)}


def row(flux, actor):
    """An actor with nothing to count has no row: read it as zeros."""
    return flux.get(actor, {"created": 0, "done": 0, "dismissed": 0, "reopened": 0, "net": 0})


class ReviewPr125FluxTests(unittest.TestCase):

    def setUp(self):
        self.tmp = tempfile.NamedTemporaryFile(suffix=".db")
        self.j = Journal(self.tmp.name)

    def tearDown(self):
        self.j.con.close()
        self.tmp.close()

    def test_deleting_open_tasks_counts_as_closing_them(self):
        j = self.j
        for t in ("t1", "t2", "t3", "t4"):
            j.add(t, "task.created", "agent-a", "-10 minutes", status="pending")
        # t3 (an undone create) and t1 are deleted while open; t4 is done,
        # then deleted, which changes nothing that is open. Tombstones carry
        # what the current code writes: no status.
        j.add("t3", "task.deleted", "agent-a", "-9 minutes", pt_id="PT-3")
        j.add("t1", "task.deleted", "agent-a", "-8 minutes", pt_id="PT-1")
        j.add("t4", "task.completed", "agent-a", "-7 minutes", pt_id="PT-4")
        j.add("t4", "task.deleted", "agent-a", "-6 minutes", pt_id="PT-4")
        got = row(j.flux(), "agent-a")
        self.assertEqual(
            got["net"], 1,
            f"agent-a left one task open (t2) but the sidecar reports net {got['net']}: {got}")

    def test_only_real_open_close_transitions_count(self):
        j = self.j
        j.add("p1", "task.created", "agent-a", "-30 minutes", status="pending")
        j.add("p2", "task.created", "agent-a", "-30 minutes", status="pending")
        j.add("p2", "task.completed", "agent-a", "-25 minutes")
        # p3 was created and closed before the window.
        j.add("p3", "task.created", "agent-a", "-3 days", status="pending")
        j.add("p3", "task.completed", "agent-a", "-3 days")
        # agent-b blocks and un-blocks p1, which never leaves the open set.
        j.add("p1", "task.updated", "agent-b", "-20 minutes", status="blocked")
        j.add("p1", "task.updated", "agent-b", "-19 minutes", status="pending",
              reason="lift an automatic block")
        # agent-c completes p2 again although it is already done.
        j.add("p2", "task.completed", "agent-c", "-15 minutes")
        # Real reopens count, including p3, closed before the window.
        j.add("p2", "task.updated", "agent-d", "-10 minutes", status="pending")
        j.add("p3", "task.updated", "agent-d", "-10 minutes", status="pending")
        got = j.flux()
        self.assertEqual(row(got, "agent-b")["reopened"], 0,
                         f"an un-block of open p1 counted as a reopen: {got}")
        self.assertEqual(row(got, "agent-c")["done"], 0,
                         f"a second completion of done p2 counted as a close: {got}")
        self.assertEqual(row(got, "agent-d")["reopened"], 2,
                         f"both real reopens count, including p3 closed before the window: {got}")
        # In the window the open count went from 0 to 3 (p1, p2, p3).
        self.assertEqual(sum(a["net"] for a in got.values()), 3, got)


if __name__ == "__main__":
    unittest.main()
