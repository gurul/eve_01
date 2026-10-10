"""macOS launchd backend for the daemon auto-start service.

Writes a user-level LaunchAgent at ``~/Library/LaunchAgents/<LABEL>.plist``
that runs ``eve-bridge daemon`` at login and keeps it alive across
crashes. Logs stdout/stderr to ``~/Library/Logs/eve-bridge.log``.
"""

from __future__ import annotations

import os
import plistlib
import shutil
import subprocess
import sys
from pathlib import Path

from .claude_home import MULTI_ENV, daemon_config_dirs

NAME = "launchd"
LABEL = "com.github.eve-bridge.daemon"
PLIST_PATH = Path.home() / "Library" / "LaunchAgents" / f"{LABEL}.plist"
LOG_PATH = Path.home() / "Library" / "Logs" / "eve-bridge.log"

# Second, optional unit: the desktop notes widget (notes_widget.py). Its own
# label so it can be installed/removed independently of the daemon, and
# KeepAlive off — a widget the user quit from its context menu must stay quit.
WIDGET_LABEL = "com.github.eve-bridge.notes-widget"
WIDGET_PLIST_PATH = Path.home() / "Library" / "LaunchAgents" / f"{WIDGET_LABEL}.plist"
WIDGET_LOG_PATH = Path.home() / "Library" / "Logs" / "eve-bridge-notes-widget.log"


def _base_plist(label: str, subcommand: str, keep_alive: bool, log_path: Path) -> dict:
    """Shared LaunchAgent skeleton for every eve-bridge unit.

    ``ProgramArguments`` uses the Python interpreter that's running *this*
    install command, so a user who installs from inside the project venv
    gets a service pointing at that venv's python — which has bleak and
    watchfiles installed. No need for a separate executable path.

    ``ProcessType = Interactive`` tells launchd this agent runs in the user's
    GUI session; required for CoreBluetooth (BLE) access and for AppKit
    windows on macOS.
    """
    return {
        "Label": label,
        "ProgramArguments": [sys.executable, "-m", "eve_bridge.cli", subcommand],
        "RunAtLoad": True,
        "KeepAlive": keep_alive,
        "ProcessType": "Interactive",
        "StandardOutPath": str(log_path),
        "StandardErrorPath": str(log_path),
        "EnvironmentVariables": {
            "HOME": str(Path.home()),
            # Keep a reasonable default PATH — launchd starts with an empty
            # one, and some bleak paths shell out.
            "PATH": "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        },
    }


def _build_plist(serial_port: str | None = None) -> bytes:
    """Render the daemon plist as XML bytes. See :func:`_base_plist`."""
    plist = _base_plist(LABEL, "daemon", keep_alive=True, log_path=LOG_PATH)
    if serial_port:
        # cli.py's daemon subcommand defaults --serial-port from this env
        # var, so the service uses USB serial instead of BLE.
        plist["EnvironmentVariables"]["EVE_SERIAL_PORT"] = serial_port
    # launchd starts the daemon outside any Claude Code session, so it can
    # never inherit a per-session $CLAUDE_CONFIG_DIR. Bake in the homes to
    # serve at install time — otherwise the daemon tails only ~/.claude and
    # sessions run by a wrapper (era-code) report no tokens and no entries.
    dirs = daemon_config_dirs()
    if dirs != [Path.home() / ".claude"]:
        plist["EnvironmentVariables"][MULTI_ENV] = os.pathsep.join(str(d) for d in dirs)
    return plistlib.dumps(plist)


def _build_widget_plist() -> bytes:
    """Render the notes-widget plist. Same interpreter and env as the daemon
    unit; no daemon-only variables (serial port, hotkey, Claude homes)."""
    return plistlib.dumps(_base_plist(WIDGET_LABEL, "notes-widget", keep_alive=False,
                                      log_path=WIDGET_LOG_PATH))


def _load(plist_path: Path) -> int:
    # Unload first so idempotent re-install picks up any new interpreter path.
    subprocess.run(["launchctl", "unload", str(plist_path)], capture_output=True)
    result = subprocess.run(
        ["launchctl", "load", "-w", str(plist_path)],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        print(f"launchctl load failed ({result.returncode}): {result.stderr.strip()}",
              file=sys.stderr)
        return 2
    return 0


def install(serial_port: str | None = None) -> int:
    if shutil.which("launchctl") is None:
        print("eve-bridge: `launchctl` not found on PATH", file=sys.stderr)
        return 2

    PLIST_PATH.parent.mkdir(parents=True, exist_ok=True)
    LOG_PATH.parent.mkdir(parents=True, exist_ok=True)
    PLIST_PATH.write_bytes(_build_plist(serial_port))

    rc = _load(PLIST_PATH)
    if rc:
        return rc

    print(f"installed: {PLIST_PATH}")
    print(f"logs at:   {LOG_PATH}")
    print("daemon will start on your next login (and is starting now).")
    return 0


def uninstall() -> int:
    if not PLIST_PATH.exists():
        print("service not installed; nothing to do")
        return 0

    subprocess.run(["launchctl", "unload", str(PLIST_PATH)], capture_output=True)
    PLIST_PATH.unlink()
    print(f"removed: {PLIST_PATH}")
    return 0


def install_widget() -> int:
    if shutil.which("launchctl") is None:
        print("eve-bridge: `launchctl` not found on PATH", file=sys.stderr)
        return 2

    WIDGET_PLIST_PATH.parent.mkdir(parents=True, exist_ok=True)
    WIDGET_LOG_PATH.parent.mkdir(parents=True, exist_ok=True)
    WIDGET_PLIST_PATH.write_bytes(_build_widget_plist())

    rc = _load(WIDGET_PLIST_PATH)
    if rc:
        return rc

    print(f"installed: {WIDGET_PLIST_PATH}")
    print(f"logs at:   {WIDGET_LOG_PATH}")
    print("notes widget will show on your next login (and is showing now).")
    return 0


def uninstall_widget() -> int:
    if not WIDGET_PLIST_PATH.exists():
        print("notes widget not installed; nothing to do")
        return 0

    subprocess.run(["launchctl", "unload", str(WIDGET_PLIST_PATH)], capture_output=True)
    WIDGET_PLIST_PATH.unlink()
    print(f"removed: {WIDGET_PLIST_PATH}")
    return 0


def is_installed() -> bool:
    return PLIST_PATH.exists()


def is_loaded() -> bool:
    """True iff launchctl reports the agent currently loaded."""
    if shutil.which("launchctl") is None:
        return False
    result = subprocess.run(
        ["launchctl", "list"], capture_output=True, text=True,
    )
    if result.returncode != 0:
        return False
    return any(LABEL in line for line in result.stdout.splitlines())


def unit_path() -> Path:
    return PLIST_PATH


def log_path() -> Path:
    return LOG_PATH
