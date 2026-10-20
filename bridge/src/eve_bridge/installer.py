"""Install / uninstall eve-bridge hooks into Claude Code's settings.json.

We identify our entries by a marker substring in the command string
(`eve_bridge.hooks.`). Non-eve-bridge hooks are left alone.

Which settings.json is the load-bearing question: Claude Code reads its config
from ``$CLAUDE_CONFIG_DIR`` when that is set and from ``~/.claude`` otherwise.
Wrappers (era-code, and anything else running Claude Code against a private
config home) set it. Installing into the wrong home fails *silently* — the
hooks exist, the daemon runs, the board connects, and no session ever prompts,
because the sessions you're actually running never read that file.
"""

from __future__ import annotations

import json
import os
import shlex
import shutil
import sys
from datetime import datetime
from pathlib import Path
from typing import Any

from .claude_home import settings_path

MARKER = "eve_bridge.hooks."
# The package's name before the rename to eve (2026-10-10). Its entries point at a module that no longer exists, so
# Claude Code's hooks fail and eve never hears of a session; install replaces them and uninstall removes them.
LEGACY_MARKERS = ("cc_bu" + "ddy_bridge.hooks.",)
HOOK_TIMEOUT_SECS = 330  # must be > the pretooluse hook's BLOCK_TIMEOUT_SECS (320 s) + a small buffer

# (Claude Code hook event name, python module, matcher, needs_decision)
HOOK_DEFS: list[tuple[str, str, str | None, bool]] = [
    ("PreToolUse",        "eve_bridge.hooks.pretooluse",         "Bash", True),
    ("PreToolUse",        "eve_bridge.hooks.pretooluse",         "Read", True),
    # A question for the owner: relayed to the phone with numbered options, never answered by a hook.
    ("PreToolUse",        "eve_bridge.hooks.pretooluse",         "AskUserQuestion", True),
    # Every permission dialog, whatever the tool: a yes/no on the phone while "claude on".
    ("PermissionRequest", "eve_bridge.hooks.permission_request", "*",    True),
    ("PostToolUse",       "eve_bridge.hooks.posttooluse",        "*",    False),
    ("SessionStart",      "eve_bridge.hooks.session_start",      None,   False),
    ("SessionEnd",        "eve_bridge.hooks.session_end",        None,   False),
    ("UserPromptSubmit",  "eve_bridge.hooks.user_prompt_submit", None,   False),
    ("Stop",              "eve_bridge.hooks.stop",               None,   False),
    ("Notification",      "eve_bridge.hooks.notification",       None,   False),
]


def _python_executable() -> str:
    """Prefer the Python that was used to install this package (keeps things working
    when invoked via `eve-bridge install` from a venv).

    On Windows, convert backslashes to forward slashes for bash compatibility.
    """
    exe = sys.executable
    if sys.platform == "win32":
        # Convert Windows path to POSIX-style for bash
        exe = exe.replace("\\", "/")
    return exe


def _hook_command(module: str, hooks_dir: str | None = None) -> str:
    """``-I`` (isolated): Claude Code runs a hook in the session's folder, and plain ``-m`` puts that folder first
    on ``sys.path``, so a ``json.py`` planted there ran as the hook (shown 2026-10-12). Isolated mode leaves the
    folder and the PYTHON* variables out; the venv's own packages still load.

    With ``hooks_dir``, the hook is the Rust binary for the module in that folder (``rust/target/release``): it starts
    in about 3 ms where the interpreter takes about 29, on every tool call. The trailing comment carries the module, so
    either form replaces the other and status/uninstall find both (Claude Code's shell drops the comment)."""
    if hooks_dir is None:
        return f"{_python_executable()} -I -m {module}"
    path = str(Path(hooks_dir) / native_binary(module))
    if sys.platform == "win32":
        path = path.replace("\\", "/")
    return f"{shlex.quote(path)} # {module}"


def native_binary(module: str) -> str:
    """The Rust hook binary (rust/crates/eve-hooks) for a hook module: ``eve_bridge.hooks.session_start`` is
    ``eve-hook-session-start``."""
    name = module[len(MARKER):] if module.startswith(MARKER) else module
    return "eve-hook-" + name.replace("_", "-")


def _is_our_entry(hook_obj: dict[str, Any]) -> bool:
    cmd = hook_obj.get("command", "")
    return isinstance(cmd, str) and (MARKER in cmd or any(m in cmd for m in LEGACY_MARKERS))


def _is_legacy(hook_obj: dict[str, Any]) -> bool:
    cmd = hook_obj.get("command", "")
    return isinstance(cmd, str) and any(m in cmd for m in LEGACY_MARKERS)


def _load_settings(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    with path.open("r", encoding="utf-8") as f:
        return json.load(f)


def _save_settings(path: Path, data: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as f:
        json.dump(data, f, indent=2, ensure_ascii=False)
        f.write("\n")


def _backup(path: Path) -> Path:
    ts = datetime.now().strftime("%Y%m%d-%H%M%S")
    dest = path.with_name(f"settings.json.ccbb-backup-{ts}")
    shutil.copy2(path, dest)
    return dest


def install_hooks(config_dir: str | None = None, hooks_dir: str | None = None) -> int:
    path = settings_path(config_dir)
    if hooks_dir is not None:
        hooks_dir = str(Path(hooks_dir).expanduser().resolve())
        missing = sorted({native_binary(m) for _, m, _, _ in HOOK_DEFS
                          if not os.access(Path(hooks_dir) / native_binary(m), os.X_OK)})
        if missing:
            # A hook that names a missing binary fails on every tool call, so nothing is written.
            print(f"no hook binaries in {hooks_dir}: {', '.join(missing)}\n"
                  "Build them first: (cd rust && cargo build --release -p eve-hooks)", file=sys.stderr)
            return 2
    # The config HOME must exist — that proves Claude Code actually uses it, and
    # guards against a typo'd --config-dir silently creating a tree nothing
    # reads. The local settings file itself is ours to create: it is the file
    # wrappers do not own (see claude_home.settings_path) and often won't exist.
    if not path.parent.is_dir():
        print(f"Claude config home not found at {path.parent}", file=sys.stderr)
        return 2
    if not path.exists():
        path.write_text("{}\n", encoding="utf-8")
        print(f"created {path}")
    else:
        backup = _backup(path)
        print(f"backed up settings to {backup}")

    data = _load_settings(path)
    hooks = data.setdefault("hooks", {})

    removed = 0
    for entries in hooks.values():
        for group in entries if isinstance(entries, list) else []:
            inner = group.get("hooks") if isinstance(group, dict) else None
            if isinstance(inner, list):
                kept = [h for h in inner if not _is_legacy(h)]
                removed += len(inner) - len(kept)
                inner[:] = kept
    for event in list(hooks):
        if isinstance(hooks[event], list):
            hooks[event] = [g for g in hooks[event] if not (isinstance(g, dict) and g.get("hooks") == [])]
    if removed:
        print(f"removed {removed} hook(s) left from before the rename")

    added = 0
    for event, module, matcher, needs_decision in HOOK_DEFS:
        entries = hooks.setdefault(event, [])
        cmd = _hook_command(module, hooks_dir)

        # Find or create the matcher group.
        group = _find_matcher_group(entries, matcher)
        if group is None:
            group = {"hooks": []}
            if matcher is not None:
                group["matcher"] = matcher
            entries.append(group)

        inner = group.setdefault("hooks", [])
        # Skip if an identical eve-bridge entry already exists.
        if any(_is_our_entry(h) and h.get("command") == cmd for h in inner):
            continue

        # Remove any stale eve-bridge entry for the same module (path changed, etc.).
        inner[:] = [
            h for h in inner
            if not (_is_our_entry(h) and module in h.get("command", ""))
        ]
        inner.append({
            "type": "command",
            "command": cmd,
            "timeout": HOOK_TIMEOUT_SECS if needs_decision else 10,
        })
        added += 1

    _save_settings(path, data)
    print(f"installed {added} hook(s) into {path}")
    if added == 0:
        print("(already up to date)")
    return 0


def uninstall_hooks(config_dir: str | None = None) -> int:
    path = settings_path(config_dir)
    if not path.exists():
        print(f"settings.json not found at {path}", file=sys.stderr)
        return 2

    data = _load_settings(path)
    hooks = data.get("hooks")
    if not isinstance(hooks, dict):
        print("no hooks block — nothing to remove")
        return 0

    removed = 0
    for event in list(hooks.keys()):
        entries = hooks.get(event) or []
        if not isinstance(entries, list):
            continue
        cleaned_groups = []
        for group in entries:
            if not isinstance(group, dict):
                cleaned_groups.append(group)
                continue
            inner = group.get("hooks") or []
            before = len(inner)
            remaining = [h for h in inner if not _is_our_entry(h)]
            removed += before - len(remaining)
            if remaining:
                group["hooks"] = remaining
                cleaned_groups.append(group)
            # else drop the group entirely
        if cleaned_groups:
            hooks[event] = cleaned_groups
        else:
            hooks.pop(event)

    if not hooks:
        data.pop("hooks", None)

    if removed == 0:
        print("no eve-bridge hooks found — nothing to remove")
        return 0

    backup = _backup(path)
    print(f"backed up settings to {backup}")
    _save_settings(path, data)
    print(f"removed {removed} hook(s) from {path}")
    return 0


def show_status(config_dir: str | None = None) -> int:
    path = settings_path(config_dir)
    env_dir = os.environ.get("CLAUDE_CONFIG_DIR")
    print(f"Hooks: ({path})")
    if env_dir:
        print(f"  CLAUDE_CONFIG_DIR={env_dir}")
    if not path.exists():
        print(f"  settings.json not found at {path}")
    else:
        data = _load_settings(path)
        hooks = data.get("hooks") or {}
        any_installed = False
        for event, entries in hooks.items():
            if not isinstance(entries, list):
                continue
            for group in entries:
                if not isinstance(group, dict):
                    continue
                for h in group.get("hooks") or []:
                    if _is_our_entry(h):
                        any_installed = True
                        matcher = group.get("matcher", "*")
                        print(f"  {event} [{matcher}] → {h.get('command')}")
        if not any_installed:
            print("  no eve-bridge hooks installed")

    from . import service
    backend = service.backend_name()
    header = f"Service ({backend}):" if backend else "Service:"
    print(f"\n{header}")
    if backend is None:
        print(f"  unsupported platform ({sys.platform})")
    elif not service.is_installed():
        print("  not installed (run `eve-bridge install --service` to add)")
    else:
        loaded = "loaded" if service.is_loaded() else "installed but not loaded"
        print(f"  {loaded}: {service.unit_path()}")
        print(f"  logs: {service.log_path()}")
    return 0


def _find_matcher_group(entries: list, matcher: str | None) -> dict | None:
    for e in entries:
        if not isinstance(e, dict):
            continue
        # Treat absent matcher and "*" as the same target for events with no matcher.
        if matcher is None and "matcher" not in e:
            return e
        if matcher is not None and e.get("matcher") == matcher:
            return e
    return None
