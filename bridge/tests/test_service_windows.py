"""The Windows Task Scheduler backend's command line (review D9). Built and checked here on any OS:
schtasks is never run, and the task itself is untested on Windows."""

from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import _service_windows as win


@pytest.fixture
def schtasks(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> list[list[str]]:
    calls: list[list[str]] = []
    monkeypatch.setattr(win.shutil, "which", lambda name: "C:\\Windows\\System32\\schtasks.exe")
    monkeypatch.setattr(win, "LOG_PATH", tmp_path / "eve bridge" / "daemon.log")
    monkeypatch.setattr(win.sys, "executable", "C:\\Program Files\\Python 3.12\\python.exe")

    def run(args: list[str], **kw: Any) -> subprocess.CompletedProcess:
        calls.append(list(args))
        return subprocess.CompletedProcess(args, 0, "", "")
    monkeypatch.setattr(win.subprocess, "run", run)
    return calls


def _tr(calls: list[list[str]]) -> str:
    args = calls[-1]
    return args[args.index("/tr") + 1]


def test_the_task_runs_its_redirect_and_env_through_cmd(schtasks: list[list[str]]) -> None:
    """/tr is run as a program, not through cmd.exe, so `set ... &&` and `>> log 2>&1` never worked: the
    task runs a launcher script with cmd /c instead, and the script holds the cmd syntax."""
    assert win.install("COM3") == 0
    tr = _tr(schtasks)
    launcher = win.LOG_PATH.parent / "daemon.cmd"
    assert tr == f'cmd.exe /c "{launcher}"'
    assert len(tr) <= 261                                  # schtasks' /tr limit
    script = launcher.read_text()
    assert 'set "EVE_SERIAL_PORT=COM3"' in script
    assert f'"C:\\Program Files\\Python 3.12\\python.exe" -m eve_bridge.cli daemon >> "{win.LOG_PATH}" 2>&1' in script
    assert "&&" not in tr and ">>" not in tr


def test_without_a_port_the_launcher_sets_nothing(schtasks: list[list[str]]) -> None:
    assert win.install(None) == 0
    assert "EVE_SERIAL_PORT" not in (win.LOG_PATH.parent / "daemon.cmd").read_text()


def test_a_percent_sign_is_escaped_for_the_batch_file(schtasks: list[list[str]], monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(win.sys, "executable", "C:\\100%\\python.exe")
    assert win.install(None) == 0
    assert '"C:\\100%%\\python.exe"' in (win.LOG_PATH.parent / "daemon.cmd").read_text()


def test_uninstall_removes_the_launcher(schtasks: list[list[str]]) -> None:
    assert win.install(None) == 0
    assert win.uninstall() == 0
    assert not (win.LOG_PATH.parent / "daemon.cmd").exists()
