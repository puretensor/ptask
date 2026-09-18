"""Read production source so pytest pins behaviour to this checkout."""

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def read(rel: str) -> str:
    return (ROOT / rel).read_text(encoding="utf-8")


def fn_body(src: str, name: str) -> str:
    """Return the source of `fn name` through the next top-level `fn`/`}`."""
    import re

    match = re.search(rf"\bfn {re.escape(name)}\b", src)
    if not match:
        raise AssertionError(f"function {name!r} not found")
    start = match.start()
    # Skip `{` used in parameter destructuring (`Parameters(NextArg { limit })`).
    brace = None
    paren = 0
    seen_paren = False
    for i, ch in enumerate(src[start:], start):
        if ch == "(":
            paren += 1
            seen_paren = True
        elif ch == ")":
            paren -= 1
        elif ch == "{" and paren == 0 and seen_paren:
            brace = i
            break
    if brace is None:
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
