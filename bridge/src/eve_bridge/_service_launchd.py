"""macOS launchd backend for the daemon auto-start service.

Writes a user-level LaunchAgent at ``~/Library/LaunchAgents/<LABEL>.plist``
that runs ``eve-bridge daemon`` at login and keeps it alive across
crashes. Logs stdout/stderr to ``~/Library/Logs/eve-bridge.log``.

eve lite (EVE_PROFILE=lite, docs/plans/eve-lite.md M8) has a unit of its own,
``com.github.eve-bridge.lite``, written by ``install_lite`` and logging to
``~/Library/Logs/eve-lite.log``. Full eve's plists are built exactly as before.
"""

from __future__ import annotations

import contextlib
import os
import plistlib
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, Optional

from .claude_home import MULTI_ENV, daemon_config_dirs
from .profile import LITE, PROFILE_ENV

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

# eve lite's unit (docs/plans/eve-lite.md M8). A label of its own, so installing it never replaces full eve's. Its
# environment pins the profile: the env file only fills gaps (envfile.py), so this login item runs eve lite whatever
# the env file says. launchd starts it again only after a failure exit, LITE_THROTTLE_SECS later: a sign-in that saved
# a credential exits 75 and comes back into it, while a stop, or full eve installed for this user, exits 0 and stays
# down (lite.py). Its paths are read from HOME when they are used, not fixed at import as full eve's above.
LITE_LABEL = "com.github.eve-bridge.lite"
LITE_LOG_NAME = "eve-lite.log"
LITE_PATH = "/usr/bin:/bin:/usr/sbin:/sbin"     # launchd's own default: eve lite needs nothing from Homebrew
LITE_THROTTLE_SECS = 5
LITE_UMASK = 0o077                               # 63 in the plist: what the daemon creates is the user's alone


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


# ---- eve lite's unit ------------------------------------------------------------------------------------------------

def agent_path(label: str) -> Path:
    """Where this user's LaunchAgent ``label`` lives, read from HOME now."""
    return Path.home() / "Library" / "LaunchAgents" / f"{label}.plist"


def lite_plist_path() -> Path:
    return agent_path(LITE_LABEL)


def lite_log_path() -> Path:
    return Path.home() / "Library" / "Logs" / LITE_LOG_NAME


def _build_lite_plist() -> bytes:
    """Render eve lite's plist (spec M8). Not :func:`_base_plist`: launchd restarts eve lite only after a failure exit
    and only after LITE_THROTTLE_SECS, and starts it with the profile, owner-only files (Umask), launchd's own PATH and
    no user site-packages (PYTHONNOUSERSITE: nothing outside eve lite's venv is imported). ``ProgramArguments`` is the
    interpreter running this install, eve lite's venv's python when the installer runs it."""
    log = str(lite_log_path())
    return plistlib.dumps({
        "Label": LITE_LABEL,
        "ProgramArguments": [sys.executable, "-m", "eve_bridge.cli", "daemon"],
        "RunAtLoad": True,
        "KeepAlive": {"SuccessfulExit": False},
        "ThrottleInterval": LITE_THROTTLE_SECS,
        "ProcessType": "Interactive",
        "Umask": LITE_UMASK,
        "StandardOutPath": log,
        "StandardErrorPath": log,
        "EnvironmentVariables": {
            "HOME": str(Path.home()),
            "PATH": LITE_PATH,
            PROFILE_ENV: LITE,
            "PYTHONNOUSERSITE": "1",
        },
    })


def _write_plist(path: Path, data: bytes) -> None:
    """``data`` into ``path`` through a temporary file in the same folder and one rename, so launchd never reads half a
    plist; 0644, as LaunchAgents are (it holds no secret)."""
    fd, tmp = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
        os.chmod(tmp, 0o644)
        os.replace(tmp, path)
    except BaseException:
        with contextlib.suppress(OSError):
            os.unlink(tmp)
        raise


def install_lite() -> int:
    """`eve-bridge install --service` under EVE_PROFILE=lite: write eve lite's plist and (re)load it, so eve lite starts
    now and at every login; run again, it picks up a new interpreter path (an update). Refused while full eve's agent
    is installed for this user: full eve and eve lite keep their data in the same ~/.config/eve-bridge, so a Mac
    account has one of them (install-lite.sh refuses it too, and eve lite's daemon stays down beside full eve)."""
    if shutil.which("launchctl") is None:
        print("eve-bridge: `launchctl` not found on PATH", file=sys.stderr)
        return 2
    full = agent_path(LABEL)
    if full.exists():
        print(f"eve-bridge: full eve's login service is installed for this user ({full}). Full eve and eve lite "
              "share ~/.config/eve-bridge, so a Mac account has one of them. Nothing was changed.", file=sys.stderr)
        return 2
    path, log = lite_plist_path(), lite_log_path()
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        log.parent.mkdir(parents=True, exist_ok=True)
        _write_plist(path, _build_lite_plist())
    except OSError as e:
        print(f"eve-bridge: eve lite's login service could not be written to {path.parent} ({type(e).__name__}).",
              file=sys.stderr)
        return 2

    rc = _load(path)
    if rc:
        return rc

    print(f"installed: {path}")
    print(f"logs at:   {log}")
    print("eve lite will start on your next login (and is starting now).")
    return 0


def uninstall_lite() -> int:
    """Stop eve lite and remove its plist. It is stopped by its label (``launchctl bootout``, as install-lite.sh
    --uninstall does), which works whether or not the file is still there; ``unload`` by the file when that fails."""
    path = lite_plist_path()
    loaded = lite_loaded()
    if not loaded and not path.exists():
        print("eve lite's login service is not installed; nothing to do")
        return 0
    if loaded:
        done = subprocess.run(["launchctl", "bootout", f"gui/{os.getuid()}/{LITE_LABEL}"], capture_output=True)
        if done.returncode != 0 and path.exists():
            subprocess.run(["launchctl", "unload", str(path)], capture_output=True)
        print("stopped eve lite")
    if path.exists():
        path.unlink()
        print(f"removed: {path}")
    return 0


def lite_installed() -> bool:
    return lite_plist_path().exists()


def lite_loaded() -> bool:
    """True iff launchctl lists eve lite's agent: running, or loaded and waiting to start again."""
    if shutil.which("launchctl") is None:
        return False
    result = subprocess.run(["launchctl", "list"], capture_output=True, text=True)
    if result.returncode != 0:
        return False
    # "PID\tStatus\tLabel" per line: the label is the last column, matched whole
    return any(line.rsplit("\t", 1)[-1].strip() == LITE_LABEL for line in result.stdout.splitlines())


def installed_lite_plist() -> Optional[dict[str, Any]]:
    """eve lite's plist as installed, parsed; None when there is none, or it does not parse."""
    try:
        with open(lite_plist_path(), "rb") as f:
            data = plistlib.load(f)
    except Exception:  # noqa: BLE001 — missing, unreadable or not a plist: there is no usable one
        return None
    return data if isinstance(data, dict) else None
