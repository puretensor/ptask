"""Finding 1 — CLI must forward --idempotency-key onto RemoteClient.

Pre-fix, cmd_remote builds a bare RemoteClient and calls add/done/edit.
The advertised retry key never leaves CLI_IDEMPOTENCY, so two invocations
with the same key mint independent command UUIDs and the server cannot
deduplicate them.
"""

from __future__ import annotations

import uuid

from source import fn_body, read

# Mutation verbs that POST a /sync command. Reads (list/show/next/version)
# do not need the key, but forwarding it is harmless.
MUTATIONS = (
    "add",
    "done",
    "priority",
    "edit",
    "reopen",
    "dismiss",
    "start",
    "snooze",
    "depend",
    "rm",
)

# Verbatim pre-fix construction from the review (cmd_remote Add arm).
PRE_FIX_ADD_CLIENT = """
            let client = match a.url {
                Some(u) => remote::RemoteClient::with_url(&u)?,
                None => remote::RemoteClient::from_env()?,
            };
            let task = client.add(&a.text)?;
"""


def command_uuid_for_retry(key: str | None, *, client_received_key: bool) -> str:
    """Command UUID a remote add would send.

    Finding 1's contract is forwarding: if the client never received the key,
    it cannot reuse it and must mint a fresh UUID (the live pre-fix path).
    Once the key is on the client, a later change can honor it; this finding
    only requires that the key is present on the client used for mutations.
    """
    if client_received_key and key:
        return key
    return str(uuid.uuid4())


def client_construction_forwards_cli_key(main_rs: str) -> bool:
    """True when every remote mutation client is built with the CLI key."""
    try:
        helper = fn_body(main_rs, "remote_client")
    except AssertionError:
        return False
    attaches = (
        "with_idempotency_key" in helper
        and ("cli_idempotency_key" in helper or "CLI_IDEMPOTENCY" in helper)
    )
    if not attaches:
        return False
    cmd = fn_body(main_rs, "cmd_remote")
    # Mutation arms must not build a bare RemoteClient.
    for verb in MUTATIONS:
        # Each verb is invoked as client.<verb>( — the client on that path
        # has to come from remote_client(...), not with_url/from_env.
        if f"client.{verb}(" not in cmd and f"client.{verb} (" not in cmd:
            # start/snooze/depend/rm go through methods of those names.
            if verb not in cmd:
                return False
    if "RemoteClient::with_url" in cmd or "RemoteClient::from_env" in cmd:
        return False
    return "remote_client(" in cmd


def test_pre_fix_retry_mints_distinct_command_uuids():
    """Negative control: the reviewed construction never forwards the key."""
    forwarded = (
        "with_idempotency_key" in PRE_FIX_ADD_CLIENT
        or "CLI_IDEMPOTENCY" in PRE_FIX_ADD_CLIENT
    )
    assert not forwarded
    first = command_uuid_for_retry("retry-probe", client_received_key=forwarded)
    second = command_uuid_for_retry("retry-probe", client_received_key=forwarded)
    assert first != second, "pre-fix retries must not share a command UUID"


def test_head_forwards_cli_key_to_remote_mutations():
    """Fails if main.rs is reverted to the bare with_url/from_env construction."""
    main_rs = read("crates/ptask-cli/src/main.rs")
    assert client_construction_forwards_cli_key(
        main_rs
    ), "cmd_remote must attach CLI_IDEMPOTENCY to every remote mutation client"
    first = command_uuid_for_retry("retry-probe", client_received_key=True)
    second = command_uuid_for_retry("retry-probe", client_received_key=True)
    assert first == second == "retry-probe"
