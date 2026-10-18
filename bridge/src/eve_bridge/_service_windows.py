"""Windows Task Scheduler backend for daemon auto-start."""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

NAME = "task-scheduler"
TASK_NAME = "eve-bridge-daemon"
LOG_PATH = Path.home() / "AppData" / "Local" / "eve-bridge" / "daemon.log"


def launcher_path() -> Path:
    """The script the task runs: the daemon with its env and log redirect, in cmd syntax."""
    return LOG_PATH.parent / "daemon.cmd"


def _batch(value: str) -> str:
    """A value for a .cmd line: a lone % would start a variable there."""
    return value.replace("%", "%%")


def install(serial_port: str | None = None) -> int:
    if shutil.which("schtasks") is None:
        print("eve-bridge: `schtasks` not found on PATH", file=sys.stderr)
        return 2

    LOG_PATH.parent.mkdir(parents=True, exist_ok=True)

    # schtasks runs /tr as a program, not through cmd.exe, so the `set ... &&` and the `>> log 2>&1` it used
    # to carry were handed to python as arguments and the task never started; /tr is also capped at 261
    # characters. The cmd syntax goes in a launcher script beside the log, and the task runs that with cmd /c.
    # Untested on Windows: built and checked by tests/test_service_windows.py on any OS.
    lines = ["@echo off"]
    if serial_port:
        lines.append(f'set "EVE_SERIAL_PORT={_batch(serial_port)}"')
    lines.append(f'"{_batch(sys.executable)}" -m eve_bridge.cli daemon >> "{_batch(str(LOG_PATH))}" 2>&1')
    launcher = launcher_path()
    launcher.write_text("\r\n".join(lines) + "\r\n", encoding="utf-8")
    cmd = f'cmd.exe /c "{launcher}"'

    result = subprocess.run(
        [
            "schtasks", "/create",
            "/tn", TASK_NAME,
            "/tr", cmd,
            "/sc", "onlogon",
            "/f",
        ],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        print(f"schtasks create failed ({result.returncode}): {result.stderr.strip()}",
              file=sys.stderr)
        return 2

    print(f"installed: Task Scheduler task '{TASK_NAME}'")
    print(f"logs at:   {LOG_PATH}")
    print("daemon will start on your next login.")
    print("To start now, run: eve-bridge daemon")
    return 0


def uninstall() -> int:
    if not is_installed():
        print("service not installed; nothing to do")
        return 0

    result = subprocess.run(
        ["schtasks", "/delete", "/tn", TASK_NAME, "/f"],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        print(f"schtasks delete failed ({result.returncode}): {result.stderr.strip()}",
              file=sys.stderr)
        return 2

    try:
        launcher_path().unlink()
    except OSError:
        pass
    print(f"removed: Task Scheduler task '{TASK_NAME}'")
    return 0


def is_installed() -> bool:
    if shutil.which("schtasks") is None:
        return False
    result = subprocess.run(
        ["schtasks", "/query", "/tn", TASK_NAME],
        capture_output=True, text=True,
    )
    return result.returncode == 0


def is_loaded() -> bool:
    return is_installed()


def unit_path() -> str:
    return f"Task Scheduler: {TASK_NAME}"


def log_path() -> Path:
    return LOG_PATH
