"""Goal links contract (pTask 3.32.0) — HAL red tests, black-box.

Every task can trace up to the mission: goals form a tree (G-n), a task links
to a goal directly or inherits its parent task's goal. `pt show`, MCP and `pt context` carry the chain so a worker gets the
"why" with the "what".

Run:  PT_BIN=<path to built pt> python3 -m pytest tests/test_12_goals_contract.py -q
"""

from __future__ import annotations

import json
import os
import re
import sqlite3
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
PT = os.environ.get("PT_BIN") or str(ROOT / "target" / "debug" / "pt")
G_RE = re.compile(r"^G-\d+$")


@pytest.fixture()
def env(tmp_path):
    return {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(tmp_path),
        "PTASK_DB": str(tmp_path / "tasks.db"),
        "PTASK_ACTOR": "hal",
        "NO_COLOR": "1",
    }


def run(env, *args, check=True):
    p = subprocess.run([PT, *args], env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)
    if check and p.returncode != 0:
        raise AssertionError(f"pt {args} rc={p.returncode}\nstdout={p.stdout}\nstderr={p.stderr}")
    return p


def pj(env, *args):
    return json.loads(run(env, "--json", *args).stdout)


@pytest.fixture()
def tree(env):
    """mission G-1 → G-2 (revenue) → G-3 (ARGUS); G-4 sibling under G-1."""
    g1 = pj(env, "goal", "add", "Make PureTensor self-sustaining", "--why", "Runway ends in 2027")
    g2 = pj(env, "goal", "add", "Reach $50k MRR", "--parent", g1["id"], "--why", "Covers burn")
    g3 = pj(env, "goal", "add", "Ship ARGUS to 3 paying customers", "--parent", g2["id"], "--why", "Fastest revenue")
    g4 = pj(env, "goal", "add", "Keep the fleet healthy", "--parent", g1["id"])
    return g1, g2, g3, g4


def chain_ids(task_json):
    return [g["id"] for g in task_json["goal_chain"]]


def test_goal_add_shape(env, tree):
    g1, g2, g3, _ = tree
    assert G_RE.match(g1["id"]) and g1["parent"] is None and g1["status"] == "active"
    assert g1["why"] == "Runway ends in 2027"
    assert g3["parent"] == g2["id"]
    bad = run(env, "goal", "add", "orphan", "--parent", "G-999", check=False)
    assert bad.returncode != 0


def test_goal_ls_is_a_tree_in_order(env, tree):
    g1, g2, g3, g4 = tree
    items = pj(env, "goal", "ls")
    ids = [g["id"] for g in items]
    assert ids.index(g1["id"]) < ids.index(g2["id"]) < ids.index(g3["id"])
    assert ids.index(g1["id"]) < ids.index(g4["id"])
    depth = {g["id"]: g["depth"] for g in items}
    assert depth[g1["id"]] == 0 and depth[g2["id"]] == 1 and depth[g3["id"]] == 2 and depth[g4["id"]] == 1


def test_direct_link_gives_leaf_to_root_chain(env, tree):
    g1, g2, g3, _ = tree
    t = pj(env, "add", "Draft ARGUS onboarding email", "--raw")
    run(env, "goal", "link", t["pt_id"], g3["id"])
    shown = pj(env, "show", t["pt_id"])
    assert shown["goal_source"] == "direct"
    assert chain_ids(shown) == [g3["id"], g2["id"], g1["id"]]
    assert shown["goal_chain"][0]["why"] == "Fastest revenue"
    assert shown["goal_chain"][0]["title"] == "Ship ARGUS to 3 paying customers"


def test_subtask_inherits_parent_task_goal(env, tree):
    _, _, g3, _ = tree
    parent = pj(env, "add", "ARGUS customer #2 onboarding", "--raw")
    child = pj(env, "add", "Send welcome pack", "--raw")
    run(env, "goal", "link", parent["pt_id"], g3["id"])
    with sqlite3.connect(env["PTASK_DB"]) as db:
        db.execute("UPDATE tasks SET parent_uuid = ? WHERE id = ?", (parent["id"], child["id"]))
    shown = pj(env, "show", child["pt_id"])
    assert shown["goal_source"] == "parent"
    assert chain_ids(shown)[0] == g3["id"]
    _, _, _, g4 = tree
    run(env, "goal", "link", child["pt_id"], g4["id"])
    shown = pj(env, "show", child["pt_id"])
    assert shown["goal_source"] == "direct" and chain_ids(shown)[0] == g4["id"]
    run(env, "goal", "unlink", child["pt_id"])
    assert pj(env, "show", child["pt_id"])["goal_source"] == "parent"


def test_task_without_goal(env, tree):
    t = pj(env, "add", "Buy printer paper", "--raw")
    shown = pj(env, "show", t["pt_id"])
    assert shown["goal_chain"] == [] and shown["goal_source"] == "none"


def test_cycles_and_unknowns_rejected(env, tree):
    g1, g2, g3, _ = tree
    assert run(env, "goal", "set-parent", g1["id"], g3["id"], check=False).returncode != 0
    assert run(env, "goal", "set-parent", g2["id"], g2["id"], check=False).returncode != 0
    assert pj(env, "goal", "show", g1["id"])["parent"] is None
    t = pj(env, "add", "x", "--raw")
    assert run(env, "goal", "link", t["pt_id"], "G-999", check=False).returncode != 0
    assert run(env, "goal", "link", "PT-999", g1["id"], check=False).returncode != 0
    run(env, "goal", "set-parent", g3["id"], g1["id"])  # legal re-parent
    assert pj(env, "goal", "show", g3["id"])["parent"] == g1["id"]


def test_corrupt_cycle_in_db_cannot_hang_show(env, tree):
    g1, g2, _, _ = tree
    u1 = pj(env, "goal", "show", g1["id"])["uuid"]
    u2 = pj(env, "goal", "show", g2["id"])["uuid"]
    t = pj(env, "add", "y", "--raw")
    run(env, "goal", "link", t["pt_id"], g2["id"])
    with sqlite3.connect(env["PTASK_DB"]) as db:
        db.execute("UPDATE goals SET parent_id = ? WHERE id = ?", (u2, u1))
    shown = pj(env, "show", t["pt_id"])  # must terminate
    assert 1 <= len(shown["goal_chain"]) <= 16
    assert len(set(chain_ids(shown))) == len(chain_ids(shown)), "a cycle must be cut, not repeated"


def test_goal_status_and_ls_filter(env, tree):
    _, g2, _, g4 = tree
    run(env, "goal", "done", g2["id"])
    run(env, "goal", "abandon", g4["id"])
    assert pj(env, "goal", "show", g2["id"])["status"] == "achieved"
    assert pj(env, "goal", "show", g4["id"])["status"] == "abandoned"
    active = {g["id"] for g in pj(env, "goal", "ls")}
    assert g4["id"] not in active and g2["id"] not in active
    every = {g["id"]: g["status"] for g in pj(env, "goal", "ls", "--all")}
    assert every[g2["id"]] == "achieved" and every[g4["id"]] == "abandoned"


def test_goal_show_children_tasks_and_rollup(env, tree):
    g1, g2, g3, g4 = tree
    a = pj(env, "add", "a", "--raw")
    b = pj(env, "add", "b", "--raw")
    c = pj(env, "add", "c", "--raw")
    run(env, "goal", "link", a["pt_id"], g3["id"])
    run(env, "goal", "link", b["pt_id"], g3["id"])
    run(env, "goal", "link", c["pt_id"], g4["id"])
    run(env, "done", b["pt_id"])
    s3 = pj(env, "goal", "show", g3["id"])
    assert sorted(t["pt_id"] for t in s3["tasks"]) == sorted([a["pt_id"], b["pt_id"]])
    assert [g["id"] for g in s3["chain"]] == [g2["id"], g1["id"]]
    s1 = pj(env, "goal", "show", g1["id"])
    assert sorted(g["id"] for g in s1["children"]) == sorted([g2["id"], g4["id"]])
    assert s1["rollup"] == {"open": 2, "done": 1}


def test_orphans_lists_open_tasks_without_effective_goal(env, tree):
    _, _, g3, _ = tree
    linked = pj(env, "add", "linked", "--raw")
    run(env, "goal", "link", linked["pt_id"], g3["id"])
    loose = pj(env, "add", "loose", "--raw")
    closed = pj(env, "add", "closed loose", "--raw")
    run(env, "done", closed["pt_id"])
    child = pj(env, "add", "inherits from linked", "--raw")
    with sqlite3.connect(env["PTASK_DB"]) as db:
        db.execute("UPDATE tasks SET parent_uuid = ? WHERE id = ?", (linked["id"], child["id"]))
    ids = [t["pt_id"] for t in pj(env, "goal", "orphans")]
    assert loose["pt_id"] in ids
    assert linked["pt_id"] not in ids and closed["pt_id"] not in ids and child["pt_id"] not in ids


def test_context_brief_reads_root_to_leaf(env, tree):
    g1, g2, g3, _ = tree
    t = pj(env, "add", "Write ARGUS pricing page", "-d", "One page, three tiers.", "--raw")
    blocker = pj(env, "add", "Decide ARGUS tiers", "--raw")
    run(env, "goal", "link", t["pt_id"], g3["id"])
    run(env, "depend", t["pt_id"], "--on", blocker["pt_id"])
    md = run(env, "context", t["pt_id"]).stdout
    assert "Write ARGUS pricing page" in md and "One page, three tiers." in md
    assert md.index(g1["title"]) < md.index(g2["title"]) < md.index(g3["title"]), "why-chain reads mission first"
    assert "Fastest revenue" in md and "Runway ends in 2027" in md
    assert blocker["pt_id"] in md and "Decide ARGUS tiers" in md
    orphan = pj(env, "add", "no goal here", "--raw")
    assert run(env, "context", orphan["pt_id"]).returncode == 0


def mcp_call(env, calls):
    proc = subprocess.Popen([PT, "mcp"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, text=True, bufsize=1)
    replies = {}

    def send(msg, expect=True):
        proc.stdin.write(json.dumps(msg) + "\n")
        proc.stdin.flush()
        if not expect:
            return None
        while True:
            line = proc.stdout.readline()
            if not line:
                raise AssertionError("pt mcp closed stdout")
            r = json.loads(line)
            if r.get("id") == msg.get("id"):
                return r

    try:
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "contract", "version": "0"}}})
        send({"jsonrpc": "2.0", "method": "notifications/initialized"}, expect=False)
        for i, (method, params) in enumerate(calls, start=10):
            replies[i] = send({"jsonrpc": "2.0", "id": i, "method": method, "params": params})
    finally:
        proc.stdin.close()
        proc.wait(timeout=10)
    return list(replies.values())


def test_mcp_carries_goal_chain_and_can_link(env, tree):
    g1, _, g3, _ = tree
    t = pj(env, "add", "MCP linked task", "--raw")
    tools, linked, shown, nxt = mcp_call(env, [
        ("tools/list", {}),
        ("tools/call", {"name": "goal_link", "arguments": {"task": t["pt_id"], "goal": g3["id"]}}),
        ("tools/call", {"name": "task_show", "arguments": {"id": t["pt_id"]}}),
        ("tools/call", {"name": "task_next", "arguments": {"limit": 5}}),
    ])
    names = {x["name"] for x in tools["result"]["tools"]}
    assert {"goal_list", "goal_show", "goal_link"} <= names
    assert "error" not in linked, linked
    blob = json.dumps(shown["result"])
    assert "goal_chain" in blob and g3["id"] in blob and g1["id"] in blob
    assert "goal_chain" in json.dumps(nxt["result"])
