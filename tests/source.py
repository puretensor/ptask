"""Read production source so pytest pins behaviour to this checkout."""

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def read(rel: str) -> str:
    return (ROOT / rel).read_text(encoding="utf-8")


def fn_body(src: str, name: str) -> str:
    """Return the source of `fn name` through the next top-level `fn`/`}`."""
    needle = f"fn {name}"
    start = src.find(needle)
    if start < 0:
        raise AssertionError(f"function {name!r} not found")
    # Walk braces from the first `{` after the signature.
    brace = src.find("{", start)
    if brace < 0:
        raise AssertionError(f"function {name!r} has no body")
    depth = 0
    for i, ch in enumerate(src[brace:], brace):
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return src[start : i + 1]
    raise AssertionError(f"function {name!r} is unclosed")
