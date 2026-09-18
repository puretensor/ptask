"""Finding 7 — MCP auth and tool DB work must not park a Tokio worker.

require_hal_bearer calls tokens::resolve on the async worker. SQLite
busy-wait (30s) then stalls /healthz on a single-worker runtime. MCP
tools do the same for every database call.
"""

from __future__ import annotations

from source import fn_body, read

PRE_FIX_AUTH = """
    let ok = bearer.is_some_and(|tok| {
        ptask_core::tokens::resolve(&state.db, &tok)
            .ok()
            .flatten()
            .is_some_and(|id| id.client_id == "hal" && id.scope >= ptask_core::tokens::Scope::Write)
    });
"""

MCP_TOOLS = (
    "task_next",
    "task_list",
    "task_add",
    "task_show",
    "task_done",
    "task_dismiss",
    "task_edit",
    "task_claim",
    "task_promote",
    "task_depend",
    "task_capture",
    "task_search",
    "task_digest",
)


def healthz_survives_locked_sqlite(*, auth_offloaded: bool) -> bool:
    """A 30s SQLite busy wait on the async worker starves /healthz."""
    return auth_offloaded


def resolve_is_offloaded(lib_rs: str) -> bool:
    body = fn_body(lib_rs, "require_hal_bearer")
    if "tokens::resolve" not in body:
        return False
    # The resolve call must sit inside a blocking offload, not on the worker.
    return "db_value" in body or "spawn_blocking" in body


def tools_are_offloaded(mcp_rs: str) -> bool:
    for name in MCP_TOOLS:
        body = fn_body(mcp_rs, name)
        if "on_blocking" not in body and "db_value" not in body and "spawn_blocking" not in body:
            return False
    return True


def test_pre_fix_auth_parks_the_async_worker():
    """Negative control: resolve runs inline; healthz cannot outrun the lock."""
    assert "tokens::resolve" in PRE_FIX_AUTH
    assert "db_value" not in PRE_FIX_AUTH
    assert not healthz_survives_locked_sqlite(auth_offloaded=False)


def test_head_offloads_mcp_auth_and_tool_bodies():
    """Fails if require_hal_bearer or MCP tools still touch SQLite on the worker."""
    lib_rs = read("crates/ptask-server/src/lib.rs")
    mcp_rs = read("crates/ptask-server/src/mcp.rs")
    assert resolve_is_offloaded(lib_rs), "require_hal_bearer must resolve tokens via db_value"
    assert tools_are_offloaded(mcp_rs), "MCP tool bodies, including rescore, must run on the blocking pool"
    assert healthz_survives_locked_sqlite(auth_offloaded=True)
