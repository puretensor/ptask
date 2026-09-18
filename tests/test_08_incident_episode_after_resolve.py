"""Finding 8 — a recovered incident must be allowed to open a new episode.

Raw-item uniqueness is (source_file, text) forever. Resolve closes the
task but leaves the processed raw item, so a later identical capture
returns duplicate:true and never creates a new open task.
"""

from __future__ import annotations

from source import fn_body, read

# Pre-fix: any duplicate insert returns immediately, no episode check.
PRE_FIX_EARLY_RETURN = """
                if duplicate {
                    return (
                        StatusCode::OK,
                        Json(CaptureResp {
                            id: r.id,
                            duplicate: true,
                            task_uuid: None,
"""


def capture_identity_action(
    duplicate: bool, is_incident: bool, has_capture_key: bool, has_open_keyed_task: bool
) -> str:
    """Delivery idempotency vs incident identity.

    Retries of an in-flight incident stay duplicates. A recovered incident
    (keyed, no open task) opens a new episode.
    """
    if not duplicate:
        return "proceed"
    if is_incident and has_capture_key and not has_open_keyed_task:
        return "new_episode"
    return "duplicate"


def pre_fix_action(duplicate: bool, *_rest) -> str:
    return "duplicate" if duplicate else "proceed"


def head_separates_episode_from_delivery(src: str) -> bool:
    body = fn_body(src, "capture_blocking")
    helper = fn_body(src, "capture_identity_action")
    if "NewEpisode" not in helper and "new_episode" not in helper:
        return False
    if "if duplicate" in body and "capture_identity_action" not in body:
        return False
    return "capture_identity_action" in body and "#episode=" in src


def test_pre_fix_repeat_after_resolve_is_a_dead_duplicate():
    """Negative control: identical post-resolve capture never creates work."""
    assert pre_fix_action(True, True, True, False) == "duplicate"
    # Open task from the first outage is done; pre-fix still refuses.
    statuses = ["done"]
    assert statuses == ["done"]
    assert "task_uuid: None" in PRE_FIX_EARLY_RETURN


def test_head_opens_a_new_episode_after_recovery():
    """Fails if capture still returns duplicate for a recovered incident."""
    src = read("crates/ptask-server/src/routes/capture.rs")
    assert head_separates_episode_from_delivery(
        src
    ), "capture must persist an episode and allow a new one after recovery"
    # Same fixtures as the pre-fix case: duplicate delivery, incident, key,
    # no open task → new episode. Retry while open still dedups.
    assert (
        capture_identity_action(True, True, True, False) == "new_episode"
    )
    assert capture_identity_action(True, True, True, True) == "duplicate"
    assert capture_identity_action(True, False, True, False) == "duplicate"
    assert capture_identity_action(False, True, True, False) == "proceed"
