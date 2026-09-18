"""Finding 5 — the local model must be told to emit idx/keep.

openai_request_body sent only model/messages/temperature/reasoning_effort.
The classification prompt never named those fields, so a well-formed
[{"index":0,"keep":true}] failed to deserialize and quarantined captures.
"""

from __future__ import annotations

import json

from source import fn_body, read

PRE_FIX_BODY = {
    "model": "fixture",
    "messages": [{"role": "user", "content": "Return a JSON array with EXACTLY one object per numbered item."}],
    "temperature": 0.2,
    "reasoning_effort": "none",
}

PRE_FIX_CLASSIFY_PROMPT_TAIL = (
    "Return a JSON array with EXACTLY one object per numbered item."
)


def request_communicates_idx(body: dict) -> bool:
    blob = json.dumps(body)
    return "idx" in blob


def local_provider_transmits_idx(src: str) -> bool:
    classify = fn_body(src, "classify_batch")
    # The OpenAiCompat impl is the second classify_batch; search the file
    # around the discarded-schema evidence.
    openai_impl = src[src.find("impl LlmProvider for OpenAiCompatProvider") :]
    classify = fn_body(openai_impl, "classify_batch")
    consolidate = fn_body(openai_impl, "consolidate")
    body_fn = fn_body(src, "openai_request_body")
    prompt_has_idx = "idx" in classify
    schema_sent = "schema" in body_fn and (
        "response_format" in body_fn or "responseSchema" in body_fn
    )
    # Schema must not be discarded.
    schema_discarded = "let _schema" in classify
    consolidate_has_schema = "title" in consolidate and (
        "schema" in consolidate.lower() or "title" in consolidate
    )
    return prompt_has_idx and schema_sent and not schema_discarded and consolidate_has_schema


def test_pre_fix_request_never_mentions_idx():
    """Negative control: the reviewed request body has no output contract."""
    assert not request_communicates_idx(PRE_FIX_BODY)
    assert "idx" not in PRE_FIX_CLASSIFY_PROMPT_TAIL
    parsed = [{"index": 0, "keep": True}]
    assert "idx" not in parsed[0]


def test_head_tells_the_local_model_to_emit_idx():
    """Fails if classify_batch still discards the schema and omits idx."""
    src = read("crates/ptask-distill/src/providers.rs")
    assert local_provider_transmits_idx(
        src
    ), "OpenAiCompatProvider must send idx/keep in the prompt or request schema"
    openai_impl = src[src.find("impl LlmProvider for OpenAiCompatProvider") :]
    classify = fn_body(openai_impl, "classify_batch")
    assert "idx" in classify and "keep" in classify
    assert "let _schema" not in classify
    body_fn = fn_body(src, "openai_request_body")
    assert "schema" in body_fn
