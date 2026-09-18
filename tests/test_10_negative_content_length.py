"""Finding 10 — reject negative Content-Length before reading the body.

Content-Length: -1 passes the upper-bound check and reaches read(-1),
which consumes until EOF and bypasses MAX_POST_BYTES. Login reads the
body before auth, so an unauthenticated client can buffer without limit.
"""

from __future__ import annotations

import io
import json
import sys
from pathlib import Path
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[1]
DASH = ROOT / "dashboard"
if str(DASH) not in sys.path:
    sys.path.insert(0, str(DASH))

import server  # noqa: E402


def pre_fix_read_json_body(handler):
    """Reviewed reader: only rejects n > MAX_POST_BYTES."""
    try:
        n = int(handler.headers.get("Content-Length", "0") or 0)
    except ValueError:
        handler._json({"error": "bad content length"}, 400)
        return None
    if n > server.MAX_POST_BYTES:
        handler._json({"error": "request body too large"}, 413)
        return None
    raw = handler.rfile.read(n) if n else b""
    try:
        return json.loads(raw or b"{}")
    except json.JSONDecodeError:
        handler._json({"error": "bad json"}, 400)
        return None


def _handler(length: str, payload: bytes):
    responses = []
    handler = SimpleNamespace(
        headers={"Content-Length": length},
        rfile=io.BytesIO(payload),
        _json=lambda body, code: responses.append(code),
    )
    return handler, responses


def test_pre_fix_negative_length_consumes_the_oversized_body():
    """Negative control: read(-1) returns {} after slurping past MAX_POST_BYTES."""
    payload = b" " * (server.MAX_POST_BYTES + 1) + b"{}"
    handler, responses = _handler("-1", payload)
    result = pre_fix_read_json_body(handler)
    assert result == {}
    assert responses == []
    assert handler.rfile.read() == b"", "pre-fix consumed the entire stream"


def test_head_rejects_negative_content_length():
    """Fails if _read_json_body still calls read(-1)."""
    payload = b" " * (server.MAX_POST_BYTES + 1) + b"{}"
    handler, responses = _handler("-1", payload)
    result = server.Handler._read_json_body(handler)
    assert result is None
    assert responses == [400]
    unread = handler.rfile.read()
    assert unread, "must not drain the body after a negative Content-Length"
    assert getattr(server.Handler, "timeout", None) not in (None, 0)


def test_head_still_enforces_max_post_bytes():
    handler, responses = _handler(str(server.MAX_POST_BYTES + 1), b"x")
    assert server.Handler._read_json_body(handler) is None
    assert responses == [413]
