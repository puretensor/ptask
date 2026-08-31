"""Small, stdlib-only browser session primitives for the pTask dashboard."""
from __future__ import annotations

import hashlib
import json
import os
import secrets
import threading
import time
from pathlib import Path


SESSION_MAX_AGE = 24 * 60 * 60
LOGIN_MAX_FAILURES = 5
LOGIN_LOCKOUT = 5 * 60
LOGIN_FAILURE_WINDOW = 15 * 60


def _digest(token: str) -> str:
    return hashlib.sha256(token.encode()).hexdigest()


def parse_cookie(header: str, name: str) -> str | None:
    for part in header.split(";"):
        key, sep, value = part.strip().partition("=")
        if sep and key == name:
            return value
    return None


def session_cookie(name: str, token: str, max_age: int, secure: bool) -> str:
    suffix = "; Secure" if secure else ""
    return (
        f"{name}={token}; HttpOnly; SameSite=Strict; Path=/; "
        f"Max-Age={max_age}{suffix}"
    )


class SessionStore:
    """Persist only SHA-256 session-token digests so the file is not replayable."""

    def __init__(self, path: Path, now=time.time):
        self.path = path
        self._now = now
        self._lock = threading.RLock()
        self._sessions = self._load()
        if self._prune():
            self._persist()

    def create(self) -> str:
        token = secrets.token_hex(32)
        with self._lock:
            self._prune()
            self._sessions[_digest(token)] = int(self._now())
            self._persist()
        return token

    def validate(self, token: str | None) -> bool:
        if not token:
            return False
        with self._lock:
            changed = self._prune()
            valid = _digest(token) in self._sessions
            if changed:
                self._persist()
            return valid

    def remove(self, token: str | None) -> None:
        if not token:
            return
        with self._lock:
            if self._sessions.pop(_digest(token), None) is not None:
                self._persist()

    def _load(self) -> dict[str, int]:
        try:
            data = json.loads(self.path.read_text())
            sessions = data.get("sessions", {})
            return {
                str(key): int(created)
                for key, created in sessions.items()
                if isinstance(key, str)
            }
        except (OSError, ValueError, TypeError, AttributeError):
            return {}

    def _prune(self) -> bool:
        cutoff = int(self._now()) - SESSION_MAX_AGE
        before = len(self._sessions)
        self._sessions = {
            key: created for key, created in self._sessions.items() if created > cutoff
        }
        return len(self._sessions) != before

    def _persist(self) -> None:
        self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        tmp = self.path.with_name(f".{self.path.name}.{os.getpid()}.tmp")
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        try:
            with os.fdopen(fd, "w") as handle:
                json.dump({"sessions": self._sessions}, handle, sort_keys=True)
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(tmp, self.path)
        finally:
            try:
                tmp.unlink()
            except FileNotFoundError:
                pass


class LoginThrottle:
    """Best-effort per-client failed-login lockout matching pureNOC's policy."""

    def __init__(self, now=time.monotonic):
        self._now = now
        self._lock = threading.Lock()
        self._records: dict[str, tuple[int, float, float | None]] = {}

    def locked_for(self, client: str) -> int:
        now = self._now()
        with self._lock:
            self._prune(now)
            record = self._records.get(client)
            if not record or record[2] is None or record[2] <= now:
                return 0
            return max(1, int(record[2] - now + 0.999))

    def failure(self, client: str) -> int:
        now = self._now()
        with self._lock:
            self._prune(now)
            failures, last, locked_until = self._records.get(client, (0, now, None))
            if now - last > LOGIN_FAILURE_WINDOW:
                failures, locked_until = 0, None
            failures += 1
            if failures >= LOGIN_MAX_FAILURES:
                locked_until = now + LOGIN_LOCKOUT
            self._records[client] = (failures, now, locked_until)
            return max(1, int(locked_until - now + 0.999)) if locked_until else 0

    def success(self, client: str) -> None:
        with self._lock:
            self._records.pop(client, None)

    def _prune(self, now: float) -> None:
        self._records = {
            client: record
            for client, record in self._records.items()
            if (record[2] is not None and record[2] > now)
            or now - record[1] <= LOGIN_FAILURE_WINDOW
        }
