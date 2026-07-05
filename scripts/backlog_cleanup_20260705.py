#!/usr/bin/env python3
"""One-time ptask backlog cleanup — Quiet Cockpit Phase 3 (2026-07-05).

Reversible by construction: every action is a soft transition through the
`pt` CLI (event-logged, `pt reopen`-able) plus a `duplicate_of` link where a
duplicate is collapsed. Human-authored tasks are NEVER touched; matches in
the 0.82–0.90 semantic band go to the operator eyeball list, not the axe.

Actions:
  1. Storm closures: the 2026-07-02 incident tasks (PT-869..878) whose
     conditions are live-verified resolved (Ceph HEALTH_OK, sites 200, fox
     off-by-design) → `pt done` with a resolution note.
  2. Known duplicate closures (verified pairs) → `pt dismiss` + duplicate_of.
  3. Semantic clustering over ALL pending (MiniLM, the same model ptask's
     dedup uses): >=0.90 pairs where BOTH are machine-created (distilled/
     incident) and same source → dismiss the newer, link duplicate_of.
  4. Operator eyeball list: 0.82–0.90 matches + anything excluded by the
     guardrails → report only.

Usage: backlog_cleanup_20260705.py [--execute]   (default is dry-run)
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import sqlite3
import subprocess
import sys
import uuid as uuidlib

DB = pathlib.Path.home() / "puretensor-tasks/tasks.db"
PT = pathlib.Path.home() / ".cargo/bin/pt"
REPORT = pathlib.Path.home() / f"reports/cc/{dt.datetime.now(dt.UTC).strftime('%Y-%m-%d_%H-%M')}_backlog-cleanup.md"

STORM_DONE = {
    "PT-869": "2026-07-02 site-timeout storm; sites live-verified 200 on 2026-07-05",
    "PT-872": "duplicate wording of the same 2026-07-02 site-timeout storm (resolved)",
    "PT-873": "duplicate wording of the same 2026-07-02 site-timeout storm (resolved)",
    "PT-876": "duplicate wording of the same 2026-07-02 site-timeout storm (resolved)",
    "PT-877": "duplicate wording of the same 2026-07-02 site-timeout storm (resolved)",
    "PT-874": "2026-07-02 Ceph CSI mount storm; ceph health live-verified HEALTH_OK 2026-07-05",
    "PT-878": "duplicate wording of the same 2026-07-02 CSI storm (resolved)",
    "PT-875": "2026-07-02 Ceph HEALTH_ERR episode; live-verified HEALTH_OK 2026-07-05",
    "PT-870": "fox-n1 offline is off-by-design (fleet-intent fox-tier-adhoc); suppressed at source since puresentinel 0.10.0",
    "PT-871": "fox-n0 offline is off-by-design (fleet-intent fox-tier-adhoc); suppressed at source since puresentinel 0.10.0",
}

# (dismiss, canonical, why)
KNOWN_DUPS = [
    ("PT-745", "PT-724", "distilled re-creation of a dismissed task (multi-arch puresentinel image)"),
    ("PT-746", "PT-725", "distilled re-creation of a dismissed task (puresentinel image tag rollout)"),
    ("PT-747", "PT-726", "distilled re-creation of a dismissed task (amd64 guardrail — INTENTIONAL per pureMind)"),
    ("PT-748", "PT-687", "distilled re-creation of a dismissed task (DWD heartbeat read-path port)"),
    ("PT-1039", "PT-781", "distilled paraphrase of an open subtask (App-o-Rama after ITIN)"),
    ("PT-1038", "PT-780", "distilled paraphrase of an open subtask (IBKR account)"),
]

MACHINE_SOURCES = {"distilled", "incident"}
AUTO_T = 0.90
EYEBALL_T = 0.82


def q(sql: str, args: tuple = ()) -> list[tuple]:
    con = sqlite3.connect(DB)
    try:
        return con.execute(sql, args).fetchall()
    finally:
        con.close()


def pt(args: list[str], execute: bool) -> str:
    if not execute:
        return f"DRY: pt {' '.join(args)}"
    r = subprocess.run([str(PT), *args], capture_output=True, text=True, timeout=60)
    return (r.stdout + r.stderr).strip().splitlines()[0] if (r.stdout or r.stderr) else "ok"


def add_dup_link(from_pt: str, to_pt: str, execute: bool) -> None:
    if not execute:
        return
    rows = q("SELECT id, pt_id FROM tasks WHERE pt_id IN (?, ?)", (from_pt, to_pt))
    ids = {ptid: uid for uid, ptid in rows}
    if from_pt not in ids or to_pt not in ids:
        return
    con = sqlite3.connect(DB)
    try:
        now = dt.datetime.now(dt.UTC).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "+00:00"
        con.execute(
            "INSERT OR IGNORE INTO task_links (from_uuid, to_uuid, kind, created_at) VALUES (?,?,?,?)",
            (ids[from_pt], ids[to_pt], "duplicate_of", now),
        )
        con.execute(
            "INSERT OR IGNORE INTO pt_event_log (uuid, task_uuid, event_type, payload, ts, actor) VALUES (?,?,?,?,?,?)",
            (
                f"cleanup-dup:{ids[from_pt]}",
                ids[from_pt],
                "task.duplicate_link",
                json.dumps({"duplicate_of": to_pt, "source": "backlog-cleanup-20260705", "actor": "hal"}),
                now,
                "hal",
            ),
        )
        con.commit()
    finally:
        con.close()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--execute", action="store_true")
    args = ap.parse_args()
    ex = args.execute
    log: list[str] = []
    eyeball: list[str] = []

    before = q("SELECT COUNT(*) FROM tasks WHERE status_v2 NOT IN ('done','dismissed')")[0][0]
    log.append(f"pending before: {before}")

    # 1. storm closures
    for ptid, note in STORM_DONE.items():
        row = q("SELECT status_v2 FROM tasks WHERE pt_id = ?", (ptid,))
        if not row or row[0][0] in ("done", "dismissed"):
            log.append(f"[SKIP] {ptid} already closed")
            continue
        out = pt(["done", ptid], ex)
        log.append(f"[DONE] {ptid} — {note} ({out})")

    # 2. known duplicate pairs
    for dup, canon, why in KNOWN_DUPS:
        row = q("SELECT status_v2 FROM tasks WHERE pt_id = ?", (dup,))
        if not row or row[0][0] in ("done", "dismissed"):
            log.append(f"[SKIP] {dup} already closed")
            continue
        out = pt(["dismiss", dup], ex)
        add_dup_link(dup, canon, ex)
        log.append(f"[DISMISS] {dup} duplicate_of {canon} — {why} ({out})")

    # 3. semantic clustering over remaining pending
    rows = q(
        "SELECT pt_id, id, title, source_type, created_at, status_v2 FROM tasks "
        "WHERE status_v2 NOT IN ('done','dismissed') AND triage_reason IS NULL "
        "ORDER BY created_at ASC"
    )
    from sentence_transformers import SentenceTransformer
    import numpy as np

    model = SentenceTransformer("sentence-transformers/all-MiniLM-L6-v2")
    titles = [r[2] for r in rows]
    emb = model.encode(titles, normalize_embeddings=True, batch_size=64, show_progress_bar=False)
    sims = np.matmul(emb, emb.T)

    closed_idx: set[int] = set()
    for i in range(len(rows)):
        if i in closed_idx:
            continue
        for j in range(i + 1, len(rows)):
            if j in closed_idx:
                continue
            s = float(sims[i][j])
            if s < EYEBALL_T:
                continue
            pi, pj = rows[i], rows[j]
            pair = f"{pj[0]} ~ {pi[0]} ({s:.3f}): '{pj[2][:70]}' ~ '{pi[2][:70]}'"
            if s >= AUTO_T and pi[3] in MACHINE_SOURCES and pj[3] in MACHINE_SOURCES and pi[3] == pj[3]:
                # dismiss the NEWER (j), keep the older canonical (i)
                if pj[5] == "in_progress" or pi[5] == "in_progress":
                    eyeball.append(f"[claimed] {pair}")
                    continue
                out = pt(["dismiss", pj[0]], ex)
                add_dup_link(pj[0], pi[0], ex)
                closed_idx.add(j)
                log.append(f"[DISMISS] {pair} ({out})")
            else:
                eyeball.append(pair)

    after = q("SELECT COUNT(*) FROM tasks WHERE status_v2 NOT IN ('done','dismissed')")[0][0]
    log.append(f"pending after: {after}")

    mode = "EXECUTED" if ex else "DRY-RUN"
    body = [
        "---",
        f"title: pTask Backlog Cleanup — one-time pass ({mode})",
        f"date: {dt.datetime.now(dt.UTC).strftime('%Y-%m-%d %H-%M')} UTC",
        "node: tensor-core",
        "author: HAL",
        "status: Verified" if ex else "status: In Progress",
        "classification: Internal · Fleet Ops",
        "kpis:",
        f'  - "{before} | pending before"',
        f'  - "{after if ex else "?"} | pending after"',
        f'  - "{sum(1 for l in log if l.startswith("[DONE]") or l.startswith("[DISMISS]"))} | closed (reversible)"',
        f'  - "{len(eyeball)} | operator eyeball items"',
        "---",
        f"# pTask Backlog Cleanup — {mode}",
        "",
        "Every action is reversible: `pt reopen <PT-N>`. Human-authored tasks untouched by policy.",
        "",
        "## Actions Taken",
    ]
    body += [f"{i + 1}. {l}" for i, l in enumerate(log)]
    body += ["", "## Operator Eyeball List (NOT touched — review at leisure)"]
    body += [f"- [OPEN] {e}" for e in eyeball] or ["- [OK] none"]
    body += ["", "## Next Steps", "- Reaper (`ptask-reaper.timer`, daily 05:20) bounds future staleness.", ""]
    REPORT.write_text("\n".join(body), encoding="utf-8")
    print(f"report: {REPORT}")
    print(f"pending: {before} -> {after if ex else '(dry-run)'} | closed={sum(1 for l in log if l.startswith('[D'))} eyeball={len(eyeball)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
