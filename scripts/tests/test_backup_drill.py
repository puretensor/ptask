"""ptask-backup.sh against stub ssh/scp.

Everything runs in a temp sandbox: HOME, TMPDIR and the "remote" hosts are
local directories, ssh runs the remote command locally, and hosts listed in
STUB_DOWN_HOSTS refuse connections. sqlite3 is a Python shim (list mode,
URI filenames) so the suite needs no sqlite3 CLI on the runner.
"""

from __future__ import annotations

import datetime as dt
import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]

STUBS = {
    "ssh": r"""#!/usr/bin/env bash
echo "ssh $*" >> "$STUB_LOG"
args=("$@"); host=${args[${#args[@]}-2]}; cmd=${args[${#args[@]}-1]}
for h in $STUB_DOWN_HOSTS; do
    [ "$h" = "$host" ] && { echo "ssh: connect to host $host port 22: Connection timed out" >&2; exit 255; }
done
exec bash -c "$cmd"
""",
    "scp": r"""#!/usr/bin/env bash
echo "scp $*" >> "$STUB_LOG"
src=${@: -2:1}; dst=${@: -1}
for p in "$src" "$dst"; do
    case "$p" in *:*) for h in $STUB_DOWN_HOSTS; do [ "$h" = "${p%%:*}" ] && exit 1; done ;; esac
done
exec cp "${src#*:}" "${dst#*:}"
""",
    "litestream": r"""#!/usr/bin/env bash
out=
while [ $# -gt 0 ]; do case "$1" in -o) out=$2; shift ;; esac; shift; done
exec cp "$STUB_REPLICA" "$out"
""",
    "sqlite3": r"""#!/usr/bin/env python3
import sqlite3, sys
db, sql = sys.argv[1], sys.argv[2]
try:
    c = sqlite3.connect(db, uri=db.startswith("file:"))
    for row in c.execute(sql):
        print("|".join("" if v is None else str(v) for v in row))
except sqlite3.Error as e:
    print(f"Error: {e}", file=sys.stderr)
    sys.exit(1)
""",
}


def ago(**delta: float) -> str:
    """UTC timestamp in pTask's iso_now() shape (+00:00 offset)."""
    t = dt.datetime.now(dt.timezone.utc) - dt.timedelta(**delta)
    return t.isoformat(timespec="microseconds")


def make_db(path: Path, rows: int, updated_at: str, pad: int = 120) -> None:
    c = sqlite3.connect(path)
    c.execute("pragma journal_mode=wal")
    c.execute(
        "create table tasks(id integer primary key, status text, updated_at text, body text)"
    )
    c.executemany(
        "insert into tasks values(?, 'pending', ?, ?)",
        [(i, updated_at, "x" * pad) for i in range(1, rows + 1)],
    )
    c.commit()
    c.close()


def execute(path: Path, sql: str, *params) -> None:
    c = sqlite3.connect(path)
    c.execute(sql, params)
    c.commit()
    c.close()


def backdate(path: Path, **delta: float) -> None:
    """Set mtime explicitly: files written in the same clock tick tie under
    `ls -t`, which then orders them by name."""
    t = (dt.datetime.now() - dt.timedelta(**delta)).timestamp()
    os.utime(path, (t, t))


class Sandbox:
    def __init__(self) -> None:
        self.root = Path(tempfile.mkdtemp(prefix="ptask-ops-test-"))
        self.bin = self.root / "bin"
        self.home = self.root / "home"
        self.log = self.root / "calls.log"
        for d in (self.bin, self.home / "puretensor-tasks", self.root / "tmp"):
            d.mkdir(parents=True)
        for name, body in STUBS.items():
            p = self.bin / name
            p.write_text(body)
            p.chmod(0o755)
        self.log.write_text("")
        self.live = self.home / "puretensor-tasks" / "tasks.db"

    def remote(self, name: str) -> Path:
        d = self.root / name
        d.mkdir(exist_ok=True)
        return d

    def run(self, script: str, **env: str) -> subprocess.CompletedProcess:
        full = {
            "PATH": f"{self.bin}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
            "HOME": str(self.home),
            "TMPDIR": str(self.root / "tmp"),
            "STUB_LOG": str(self.log),
            "STUB_DOWN_HOSTS": "",
            "LC_ALL": "C",
        }
        full.update(env)
        return subprocess.run(
            ["bash", str(SCRIPTS / script)],
            env=full,
            capture_output=True,
            text=True,
            timeout=60,
        )

    def calls(self) -> list[str]:
        return self.log.read_text().splitlines()

    def cleanup(self) -> None:
        shutil.rmtree(self.root, ignore_errors=True)


class BackupLegsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.sb = Sandbox()
        self.addCleanup(self.sb.cleanup)
        make_db(self.sb.live, 200, ago(hours=1))
        self.near = self.sb.remote("near")
        self.dr = self.sb.remote("dr")
        self.env = {
            "PTASK_BACKUP_REMOTE": f"mon1:{self.near}",
            "PTASK_BACKUP_OFFSITE": f"dr:{self.dr}",
        }

    def test_offsite_leg_runs_when_nearby_host_is_down(self):
        r = self.sb.run("ptask-backup.sh", STUB_DOWN_HOSTS="mon1", **self.env)
        self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertTrue(
            any(c.startswith("scp ") and f" dr:{self.dr}/" in c for c in self.sb.calls()),
            self.sb.calls(),
        )
        self.assertEqual(len(list(self.dr.glob("ptask-tasks-*.db"))), 1)
        self.assertIn("FAILED leg(s): nearby", r.stderr)

    def test_nearby_leg_runs_when_offsite_host_is_down(self):
        r = self.sb.run("ptask-backup.sh", STUB_DOWN_HOSTS="dr", **self.env)
        self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(len(list(self.near.glob("ptask-tasks-*.db"))), 1)
        self.assertIn("FAILED leg(s): off-site", r.stderr)

    def test_both_legs_ok(self):
        r = self.sb.run("ptask-backup.sh", **self.env)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        for d in (self.near, self.dr):
            (snap,) = d.glob("ptask-tasks-*.db")
            n = sqlite3.connect(snap).execute("select count(*) from tasks").fetchone()[0]
            self.assertEqual(n, 200)


if __name__ == "__main__":
    unittest.main()
