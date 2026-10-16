"""conftest.py's guards: a plain `pytest` never loads the owner's env file and never touches the owner's Composio
session file (2026-10-16: a test run wrote a session over ~/.config/eve-bridge/composio.json)."""
from __future__ import annotations

import os
from pathlib import Path

from eve_bridge import cli, composio_tools, envfile


def test_the_owners_env_file_is_never_loaded(monkeypatch, tmp_path: Path) -> None:
    environ: dict[str, str] = {}
    assert envfile.load_env_file(environ=environ) == [] and environ == {}
    assert cli.load_env_file(environ=environ) == [] and environ == {}
    own = tmp_path / "env"
    own.write_text("EVE_TEST_ISOLATION=1\n")
    assert envfile.load_env_file(own, environ) == ["EVE_TEST_ISOLATION"]       # control: another file still loads


def test_the_composio_session_file_is_this_tests_own() -> None:
    assert "COMPOSIO_API_KEY" not in os.environ
    path = composio_tools.configured(os.environ).state_path
    assert path != Path("~/.config/eve-bridge/composio.json").expanduser()
