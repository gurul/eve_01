"""Load ``~/.config/eve-bridge/env`` into the process environment.

The launchd service starts the daemon with a fixed, minimal environment, so
secrets such as ``OPENAI_API_KEY`` cannot come from the shell. They live in
a ``KEY=VALUE`` file the user owns (mode 600 recommended) and are read here
once at startup. Values already present in the environment win — the file
fills gaps, it never overrides.

File format: one ``KEY=VALUE`` per line. Blank lines and lines starting
with ``#`` are ignored, a leading ``export `` is tolerated, and a value
wrapped in matching single or double quotes is unquoted. Nothing else is
interpreted — no inline comments, no ``$VAR`` expansion.

Values are never logged. Only the key names are.
"""

from __future__ import annotations

import logging
import os
import re
from pathlib import Path
from typing import MutableMapping, Optional

log = logging.getLogger(__name__)

DEFAULT_PATH = Path("~/.config/eve-bridge/env")

_KEY_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


def parse_env_file(text: str) -> dict[str, str]:
    """``KEY=VALUE`` lines -> dict. Malformed lines are skipped, not fatal."""
    out: dict[str, str] = {}
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[len("export "):].lstrip()
        key, sep, value = line.partition("=")
        key = key.strip()
        if not sep or not _KEY_RE.match(key):
            continue
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in ("'", '"'):
            value = value[1:-1]
        out[key] = value
    return out


def load_env_file(
    path: Optional[Path] = None,
    environ: MutableMapping[str, str] = os.environ,
) -> list[str]:
    """Read the env file into ``environ`` without overriding existing keys.

    Returns the names that were set. A missing or unreadable file sets
    nothing and returns an empty list — the file is optional.
    """
    p = (path if path is not None else Path(environ.get("EVE_ENV_FILE") or DEFAULT_PATH)).expanduser()
    try:
        text = p.read_text(encoding="utf-8-sig")
    except FileNotFoundError:
        return []
    except OSError as e:
        log.warning("env file: cannot read %s: %s", p, e)
        return []
    loaded: list[str] = []
    for key, value in parse_env_file(text).items():
        if key in environ:
            continue
        environ[key] = value
        loaded.append(key)
    if loaded:
        log.info("env file: loaded %s from %s", ", ".join(loaded), p)
    return loaded
