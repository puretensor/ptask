"""Finding 2 — RemoteClient must use the supplied key as the /sync UUID.

Pre-fix, every mutation calls Uuid::new_v4(). The server deduplicates only
by that command UUID, so a retried add/done/edit with the same advertised
key executes again.
"""

from __future__ import annotations

import re
import uuid

from source import fn_body, read

PRE_FIX_ADD_UUID = """
    pub fn add(&self, text: &str) -> Result<Task> {
        let cmd_uuid = uuid::Uuid::new_v4().to_string();
        let temp_id = format!("tmp-{cmd_uuid}");
"""


def mutation_command_uuid(key: str | None, suffix: str = "") -> str:
    """Post-fix policy: supplied key is the command UUID; edits get child keys."""
    if key:
        return key if not suffix else f"{key}:{suffix}"
    return str(uuid.uuid4())


def pre_fix_command_uuid(_key: str | None, _suffix: str = "") -> str:
    return str(uuid.uuid4())


def honors_supplied_key(remote_rs: str) -> bool:
    """True when command UUIDs come from the attached idempotency key."""
    try:
        helper = fn_body(remote_rs, "command_uuid")
    except AssertionError:
        return False
    uses_key = "idempotency_key" in helper and "Uuid::new_v4" in helper
    if not uses_key:
        return False
    # Random UUIDs only when no key was supplied.
    if not re.search(r"None\s*=>\s*uuid::Uuid::new_v4", helper):
        return False
    for name in ("add", "done", "priority", "edit", "reopen", "dismiss", "simple_task_command"):
        body = fn_body(remote_rs, name)
        if "Uuid::new_v4" in body:
            return False
        if "self.command_uuid" not in body and "command_uuid(" not in body:
            return False
    return True


def test_pre_fix_retries_bypass_server_dedup():
    """Negative control: fresh UUIDs mean the same key is applied twice."""
    assert "Uuid::new_v4" in PRE_FIX_ADD_UUID
    first = pre_fix_command_uuid("ptask04-retry")
    second = pre_fix_command_uuid("ptask04-retry")
    assert first != second
    # Two distinct command UUIDs → two task_create applications.
    created = {first: "task-a", second: "task-b"}
    assert len(created) == 2


def test_pre_fix_edit_also_mints_independent_uuids():
    retext_a = pre_fix_command_uuid("edit-key", "retext")
    retext_b = pre_fix_command_uuid("edit-key", "retext")
    assert retext_a != retext_b


def test_head_uses_supplied_key_as_command_uuid():
    """Fails if remote.rs is reverted to unconditional new_v4()."""
    remote_rs = read("crates/ptask-cli/src/remote.rs")
    assert honors_supplied_key(
        remote_rs
    ), "RemoteClient mutations must reuse the attached idempotency key"
    assert mutation_command_uuid("ptask04-retry") == mutation_command_uuid("ptask04-retry")
    assert mutation_command_uuid("edit-key", "retext") != mutation_command_uuid(
        "edit-key", "deadline"
    )
    assert mutation_command_uuid("edit-key", "retext") == "edit-key:retext"
    assert mutation_command_uuid(None) != mutation_command_uuid(None)
