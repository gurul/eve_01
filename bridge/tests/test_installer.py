"""Installer tests — run against a temp settings.json so we never touch the real one."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from eve_bridge import installer


@pytest.fixture
def temp_settings(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Point $CLAUDE_CONFIG_DIR at a temp home.

    Driving the real env var rather than patching a module constant is
    deliberate: resolving the config home *is* the thing that regressed in
    production (hooks silently installed into a home no session reads), so the
    tests exercise that resolution instead of bypassing it.
    """
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path))
    return tmp_path / "settings.json"


def _baseline(extra: dict | None = None) -> dict:
    d = {
        "statusLine": {"type": "command", "command": "true"},
        "permissions": {"defaultMode": "auto"},
    }
    if extra:
        d.update(extra)
    return d


def _write(p: Path, data: dict) -> None:
    p.write_text(json.dumps(data, indent=2), encoding="utf-8")


def test_install_from_scratch(temp_settings: Path) -> None:
    _write(temp_settings, _baseline())
    assert installer.install_hooks() == 0
    data = json.loads(temp_settings.read_text())
    assert "hooks" in data
    # All 7 hook events covered.
    assert set(data["hooks"].keys()) == {
        "PreToolUse", "PostToolUse", "SessionStart", "SessionEnd",
        "UserPromptSubmit", "Stop", "Notification", "PermissionRequest",
    }
    # Non-hook settings preserved.
    assert data["statusLine"]["command"] == "true"
    assert data["permissions"]["defaultMode"] == "auto"


def test_install_is_idempotent(temp_settings: Path) -> None:
    _write(temp_settings, _baseline())
    installer.install_hooks()
    first = json.loads(temp_settings.read_text())
    installer.install_hooks()
    second = json.loads(temp_settings.read_text())
    # Same number of entries — no duplicates.
    assert len(first["hooks"]["PreToolUse"][0]["hooks"]) == len(second["hooks"]["PreToolUse"][0]["hooks"]) == 1


def test_uninstall_removes_only_our_entries(temp_settings: Path) -> None:
    baseline = _baseline({
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Bash",
                    "hooks": [
                        {"type": "command", "command": "/path/to/unrelated-hook", "timeout": 5},
                    ],
                }
            ]
        }
    })
    _write(temp_settings, baseline)
    installer.install_hooks()

    # Now install added our Bash hook alongside the user's.
    after_install = json.loads(temp_settings.read_text())
    bash_group = after_install["hooks"]["PreToolUse"][0]
    assert bash_group["matcher"] == "Bash"
    assert len(bash_group["hooks"]) == 2

    assert installer.uninstall_hooks() == 0
    after_uninstall = json.loads(temp_settings.read_text())
    # User's unrelated hook survived.
    bash_group = after_uninstall["hooks"]["PreToolUse"][0]
    assert len(bash_group["hooks"]) == 1
    assert bash_group["hooks"][0]["command"] == "/path/to/unrelated-hook"


def test_uninstall_drops_empty_hooks_block(temp_settings: Path) -> None:
    _write(temp_settings, _baseline())
    installer.install_hooks()
    installer.uninstall_hooks()
    data = json.loads(temp_settings.read_text())
    # Nothing else was in hooks; block should be gone.
    assert "hooks" not in data


def test_uninstall_when_nothing_to_remove(temp_settings: Path) -> None:
    _write(temp_settings, _baseline())
    assert installer.uninstall_hooks() == 0  # no-op, exit clean


def test_install_replaces_the_hooks_from_before_the_rename(temp_settings: Path) -> None:
    # 2026-10-10: after the rename the old entries stayed, every hook failed, and "claude on" found no session.
    old = "/old/venv/bin/python -m cc_bu" + "ddy_bridge.hooks."
    foreign = {"type": "command", "command": "other-tool hook"}
    _write(temp_settings, _baseline({"hooks": {
        "SessionStart": [{"hooks": [{"type": "command", "command": old + "session_start"}, foreign]}],
        "Stop": [{"hooks": [{"type": "command", "command": old + "stop"}]}],
    }}))
    assert installer.install_hooks() == 0
    text = temp_settings.read_text()
    assert "ddy_bridge" not in text and "other-tool hook" in text
    data = json.loads(text)
    starts = [h["command"] for g in data["hooks"]["SessionStart"] for h in g["hooks"]]
    assert any(c.endswith("-m eve_bridge.hooks.session_start") for c in starts)
    assert all(g["hooks"] for e in data["hooks"].values() for g in e)          # no empty groups left behind
