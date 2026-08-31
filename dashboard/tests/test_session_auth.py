import json
import os
import tempfile
import unittest
from pathlib import Path

from session_auth import LoginThrottle, SessionStore, parse_cookie, session_cookie


class SessionStoreTests(unittest.TestCase):
    def test_persists_hashes_only_and_survives_reload(self):
        now = [1_800_000_000]
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "sessions.json"
            store = SessionStore(path, now=lambda: now[0])
            token = store.create()

            payload = path.read_text()
            self.assertNotIn(token, payload)
            self.assertEqual(os.stat(path).st_mode & 0o777, 0o600)
            self.assertTrue(SessionStore(path, now=lambda: now[0]).validate(token))

            now[0] += 24 * 60 * 60 + 1
            self.assertFalse(SessionStore(path, now=lambda: now[0]).validate(token))
            self.assertEqual(json.loads(path.read_text()), {"sessions": {}})

    def test_remove_revokes_session(self):
        with tempfile.TemporaryDirectory() as td:
            store = SessionStore(Path(td) / "sessions.json")
            token = store.create()
            store.remove(token)
            self.assertFalse(store.validate(token))


class LoginThrottleTests(unittest.TestCase):
    def test_five_failures_lock_for_five_minutes(self):
        now = [10.0]
        throttle = LoginThrottle(now=lambda: now[0])
        for _ in range(4):
            self.assertEqual(throttle.failure("127.0.0.1"), 0)
        self.assertEqual(throttle.failure("127.0.0.1"), 300)
        self.assertEqual(throttle.locked_for("127.0.0.1"), 300)
        now[0] += 301
        self.assertEqual(throttle.locked_for("127.0.0.1"), 0)


class CookieTests(unittest.TestCase):
    def test_cookie_contract_matches_dashboard_gate(self):
        header = session_cookie("ptask_session", "opaque", 86400, True)
        self.assertIn("HttpOnly", header)
        self.assertIn("SameSite=Strict", header)
        self.assertIn("Secure", header)
        self.assertEqual(parse_cookie("a=1; ptask_session=opaque; z=2", "ptask_session"), "opaque")


if __name__ == "__main__":
    unittest.main()
