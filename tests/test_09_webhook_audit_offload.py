"""Finding 9 — outbound webhook audit INSERT must not park a Tokio worker.

dispatch is awaited on /sync. record() acquires an r2d2 connection and
INSERTs on the async worker. A 30s SQLite busy wait then starves /healthz
even though the HTTP client timeout has already elapsed.
"""

from __future__ import annotations

from source import fn_body, read

PRE_FIX_RECORD = "let _ = record(&state.db, Direction::Out, url, &envelope, outcome);"


def healthz_survives_audit_lock(*, audit_offloaded: bool) -> bool:
    return audit_offloaded


def audit_is_offloaded(src: str) -> bool:
    body = fn_body(src, "dispatch")
    if "record(" not in body:
        return False
    if "let _ = record(" in body:
        return False
    return "db_value" in body or "spawn_blocking" in body


def test_pre_fix_audit_write_parks_the_worker():
    """Negative control: the reviewed record() call runs on the async worker."""
    assert "let _ = record(" in PRE_FIX_RECORD
    assert "db_value" not in PRE_FIX_RECORD
    assert not healthz_survives_audit_lock(audit_offloaded=False)


def test_head_offloads_and_reports_outbound_audit_writes():
    """Fails if dispatch still records the audit on the async worker."""
    src = read("crates/ptask-server/src/webhooks.rs")
    assert audit_is_offloaded(src), "dispatch must await record() on the blocking pool"
    body = fn_body(src, "dispatch")
    assert "let _ = record(" not in body, "audit-write failures must be reported"
    assert "audit write" in body.lower() or "outbound audit" in body.lower()
    assert healthz_survives_audit_lock(audit_offloaded=True)
