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

Writing (``set_key``, ``remove_key``) is for eve lite's setup page, which saves the key OpenRouter mints instead
of asking anyone to paste one. A write replaces the file atomically, leaves it owner-only (0600) and keeps every
line it does not set, comments included.
"""

from __future__ import annotations

import contextlib
import logging
import os
import re
import tempfile
import threading
from pathlib import Path
from typing import Mapping, MutableMapping, Optional

log = logging.getLogger(__name__)

DEFAULT_PATH = Path("~/.config/eve-bridge/env")

_KEY_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
# What str.splitlines() ends a line on, so what parse_env_file would read as the start of another line.
_LINE_BREAKS = frozenset("\n\r\v\f\x1c\x1d\x1e\x85  ")
_write_lock = threading.Lock()


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


def env_path(environ: Mapping[str, str] = os.environ) -> Path:
    """The env file this process reads: ``EVE_ENV_FILE`` when set, else the default, as ``load_env_file``."""
    return Path(environ.get("EVE_ENV_FILE") or DEFAULT_PATH).expanduser()


def set_key(path: Path, name: str, value: str) -> None:
    """Set ``name`` to ``value`` in the env file at ``path``.

    The first line that sets ``name`` (``export`` form included) is replaced where it is, later ones are dropped,
    and a new name goes at the end. Every other line stays as it was: comments, blanks, other keys and lines the
    parser skips. The value is written so ``parse_env_file`` reads it back unchanged: it is quoted only where the
    parser would otherwise trim or unquote it. A missing file is created, in a new folder made 0700 if need be;
    a symlinked file is written through to its target.

    Raises ValueError for a name the parser would not read or a value holding a line break or NUL (it would
    start a second line), and the file is left as it was; an unreadable file is never overwritten. Only the name
    is logged."""
    _check_name(name)
    if not isinstance(value, str) or "\0" in value or any(c in _LINE_BREAKS for c in value):
        raise ValueError(f"env file: the value for {name} must be text on one line, with no NUL")
    target = _real(path)
    line = f"{name}={_quoted(value)}"
    with _write_lock:
        lines: list[str] = []
        done = False
        for raw in _lines(target):
            if _name_of(raw) == name:
                if not done:
                    lines.append(line)
                    done = True
                continue
            lines.append(raw)
        if not done:
            lines.append(line)
        _replace(target, lines)
    log.info("env file: set %s in %s", name, target)


def remove_key(path: Path, name: str) -> bool:
    """Delete every line that sets ``name`` from the env file at ``path``. True when there was one. When there
    was none, or no file, nothing is written and the answer is False. A rewritten file is 0600, as ``set_key``
    leaves it."""
    _check_name(name)
    target = _real(path)
    with _write_lock:
        lines = _lines(target)
        kept = [raw for raw in lines if _name_of(raw) != name]
        if len(kept) == len(lines):
            return False
        _replace(target, kept)
    log.info("env file: removed %s from %s", name, target)
    return True


def _check_name(name: str) -> None:
    # fullmatch, not match: "$" also matches before a final newline, and "NAME\n" would split the line it writes.
    if not isinstance(name, str) or not _KEY_RE.fullmatch(name):
        raise ValueError("env file: a name is letters, digits and _, and does not start with a digit")


def _name_of(raw: str) -> Optional[str]:
    """The name a line sets, read by the parser ``load_env_file`` uses; None for a comment, a blank or a line the
    parser skips."""
    names = list(parse_env_file(raw))
    return names[0] if names else None


def _quoted(value: str) -> str:
    """``value`` as the parser reads it back unchanged. The parser trims the value and strips one pair of matching
    quotes, so a value it would change is wrapped in double quotes; anything inside them is kept as it is."""
    if value != value.strip() or (len(value) >= 2 and value[0] == value[-1] and value[0] in ("'", '"')):
        return f'"{value}"'
    return value


def _real(path: Path) -> Path:
    return Path(os.path.realpath(Path(path).expanduser()))


def _lines(path: Path) -> list[str]:
    """The file's lines, or none when there is no file. Any other failure to read it, undecodable bytes included,
    is raised: rewriting a file eve could not read would lose what it holds."""
    try:
        return path.read_text(encoding="utf-8-sig").splitlines()
    except FileNotFoundError:
        return []


def _replace(path: Path, lines: list[str]) -> None:
    """``lines`` into ``path`` through a 0600 temporary file in the same folder and one rename, so a reader sees
    the old file or the new one, never part of one."""
    if not path.parent.is_dir():
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write("".join(line + "\n" for line in lines))
            f.flush()
            os.fsync(f.fileno())
        os.chmod(tmp, 0o600)
        os.replace(tmp, path)
    except BaseException:
        with contextlib.suppress(OSError):
            os.unlink(tmp)
        raise
    _sync_folder(path.parent)


def _sync_folder(folder: Path) -> None:
    """Flush the folder too, so the rename survives a power cut. Skipped where a folder cannot be opened."""
    try:
        fd = os.open(folder, os.O_RDONLY)
    except OSError:
        return
    try:
        os.fsync(fd)
    except OSError:
        pass
    finally:
        os.close(fd)
