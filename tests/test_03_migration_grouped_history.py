"""Finding 3 — migration SQL and history INSERT must commit together.

Ungrouped refinery 0.9.2 runs each statement in its own rusqlite
transaction. An interrupt after ALTER TABLE and before the history
INSERT leaves the schema unretryable (duplicate column on the next open).
"""

from __future__ import annotations

import sqlite3

from source import fn_body, read


def apply_v015(conn: sqlite3.Connection, *, grouped: bool, fail_history: bool) -> None:
    """Replay the V015 shape: ADD COLUMN then record version 15."""
    schema = "ALTER TABLE tasks ADD COLUMN kind TEXT"
    history = "INSERT INTO refinery_schema_history (version) VALUES (15)"
    if grouped:
        conn.execute("BEGIN")
        try:
            conn.execute(schema)
            if fail_history:
                raise sqlite3.OperationalError("injected history failure")
            conn.execute(history)
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        return
    # Pre-fix: each execute auto-commits (rusqlite driver per call).
    conn.execute(schema)
    if fail_history:
        raise sqlite3.OperationalError("injected history failure")
    conn.execute(history)


def fresh_v014(tmp_path) -> sqlite3.Connection:
    db = tmp_path / "tasks.db"
    conn = sqlite3.connect(db)
    conn.execute("CREATE TABLE tasks (id TEXT PRIMARY KEY)")
    conn.execute("CREATE TABLE refinery_schema_history (version INTEGER PRIMARY KEY)")
    conn.execute("INSERT INTO refinery_schema_history (version) VALUES (14)")
    conn.commit()
    return conn


def migrations_are_grouped(src: str) -> bool:
    body = fn_body(src, "run")
    return "set_grouped(true)" in body and ".run(" in body


def test_pre_fix_history_failure_makes_v015_unretryable(tmp_path):
    """Negative control: ungrouped apply leaves the kind column committed."""
    conn = fresh_v014(tmp_path)
    try:
        apply_v015(conn, grouped=False, fail_history=True)
        raise AssertionError("history failure should raise")
    except sqlite3.OperationalError:
        pass
    cols = {row[1] for row in conn.execute("PRAGMA table_info(tasks)")}
    assert "kind" in cols
    versions = {row[0] for row in conn.execute("SELECT version FROM refinery_schema_history")}
    assert 15 not in versions
    try:
        apply_v015(conn, grouped=False, fail_history=False)
        retryable = True
    except sqlite3.OperationalError as e:
        retryable = "duplicate column" not in str(e).lower()
    assert not retryable, "pre-fix retry must hit duplicate column name: kind"


def test_head_groups_migration_sql_with_history(tmp_path):
    """Fails if migrations::run drops set_grouped(true)."""
    src = read("crates/ptask-core/src/migrations.rs")
    assert migrations_are_grouped(
        src
    ), "migrations::run must use runner().set_grouped(true).run(conn)"
    conn = fresh_v014(tmp_path)
    try:
        apply_v015(conn, grouped=True, fail_history=True)
    except sqlite3.OperationalError:
        pass
    cols = {row[1] for row in conn.execute("PRAGMA table_info(tasks)")}
    assert "kind" not in cols, "grouped failure must roll back the schema change"
    apply_v015(conn, grouped=True, fail_history=False)
    cols = {row[1] for row in conn.execute("PRAGMA table_info(tasks)")}
    versions = {row[0] for row in conn.execute("SELECT version FROM refinery_schema_history")}
    assert "kind" in cols and 15 in versions
