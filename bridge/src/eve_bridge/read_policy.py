"""Containment check for Read-tool prompts.

Claude Code auto-allows reads inside the session's working directory, so those
never prompt anywhere. ``is_within`` is the purely lexical check that
``daemon._handle_read_pretooluse`` uses to recognise an in-cwd read and defer it
without a card.
"""

from __future__ import annotations

from pathlib import Path


def is_within(path: str, base: str) -> bool:
    """True if `path` is inside (or equal to) `base`. Purely lexical — no
    filesystem access, so it cannot block the event loop or leak reads."""
    if not path or not base:
        return False
    try:
        p = Path(path).expanduser()
        b = Path(base).expanduser()
    except (ValueError, RuntimeError):
        return False
    if not (p.is_absolute() and b.is_absolute()):
        return False
    return p == b or b in p.parents
