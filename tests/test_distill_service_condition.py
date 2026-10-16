#!/usr/bin/env python3
"""Regression test for the ptask-distill.service ExecCondition lease check.

Runs the real ExecCondition shell (after systemd's $$/%%/backslash
unescaping) with fake curl/sleep on PATH so the provider looks down and the
wait loop is instant, against a temporary $HOME audit log.
"""
import json
import os
import re
import subprocess
import tempfile
import time
import unittest

UNIT = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..", "scripts", "systemd", "ptask-distill.service",
)


def condition_script():
    with open(UNIT) as f:
        for line in f:
            if line.startswith("ExecCondition="):
                value = line.rstrip("\r\n")[len("ExecCondition="):]
                break
        else:
            raise AssertionError("no ExecCondition= in " + UNIT)
    # systemd: "$$" -> "$", "%%" -> "%", then C-style backslash unescaping.
    value = value.replace("$$", "$").replace("%%", "%")
    value = re.sub(r"\\(.)", lambda m: m.group(1), value)
    prefix = "/bin/sh -c '"
    assert value.startswith(prefix) and value.endswith("'"), value
    return value[len(prefix):-1]


class DistillLeaseConditionTest(unittest.TestCase):
    def run_condition(self, events):
        with tempfile.TemporaryDirectory() as home:
            state = os.path.join(home, ".local", "state", "ptcompute")
            os.makedirs(state)
            with open(os.path.join(state, "audit.jsonl"), "w") as f:
                for e in events:
                    f.write(json.dumps(e) + "\n")
            bindir = os.path.join(home, "bin")
            os.makedirs(bindir)
            for name, body in (("curl", "exit 7"), ("sleep", "exit 0")):
                path = os.path.join(bindir, name)
                with open(path, "w") as f:
                    f.write("#!/bin/sh\n" + body + "\n")
                os.chmod(path, 0o755)
            env = {
                "HOME": home,
                "PATH": bindir + os.pathsep + os.environ.get("PATH", "/usr/bin:/bin"),
                "LOCAL_LLM_URL": "http://127.0.0.1:9/v1",
            }
            r = subprocess.run(
                ["/bin/sh", "-c", condition_script()],
                env=env, capture_output=True, text=True, timeout=120,
            )
            return r.returncode, r.stderr

    def test_stale_grant_proceeds_and_fails_closed(self):
        now = time.time()
        code, err = self.run_condition([
            {"event": "grant", "ts": now - 48 * 3600},
        ])
        self.assertEqual(code, 0, err)
        self.assertIn("proceeding", err)

    def test_recent_grant_skips(self):
        code, err = self.run_condition([
            {"event": "grant", "ts": time.time() - 60},
        ])
        self.assertEqual(code, 1, err)
        self.assertIn("skipping", err)

    def test_recent_release_skips(self):
        now = time.time()
        code, err = self.run_condition([
            {"event": "grant", "ts": now - 1800},
            {"event": "release", "ts": now - 600},
        ])
        self.assertEqual(code, 1, err)

    def test_old_release_proceeds(self):
        now = time.time()
        code, err = self.run_condition([
            {"event": "grant", "ts": now - 4 * 3600},
            {"event": "release", "ts": now - 2 * 3600},
        ])
        self.assertEqual(code, 0, err)

    def test_no_lease_events_proceeds(self):
        code, err = self.run_condition([])
        self.assertEqual(code, 0, err)


if __name__ == "__main__":
    unittest.main()
