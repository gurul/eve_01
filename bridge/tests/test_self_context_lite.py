"""self_context.py in eve lite (EVE_PROFILE=lite; docs/plans/eve-lite.md M2, WP-3): the live-setup block names only
what eve lite is made of. Its chat brain is eve's web app's, its one door is that web app, its apps line is there only
while the owner is signed in to Composio Connect, and its commit line starts no git. Parts eve lite does not have get
no line at all, not even as off. Each check has its control: the same calls with EVE_PROFILE unset, or in another
environment than the process's, are full eve's block. No network: environments are dicts, HOME a temporary folder."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

from eve_bridge import self_context, telegram, think

PLACEHOLDER = "placeholder-not-a-key"
# eve lite's environment as profile.apply leaves it, for the switches these lines read (profile.LITE_FORCED's)
LITE_ENV = {"EVE_PROFILE": "lite", "OPENROUTER_API_KEY": PLACEHOLDER, "EVE_TELEGRAM": "app", "EVE_MEMORY": "1",
            "EVE_MEM0_EMBEDDER": "openrouter", "EVE_SECOND_BRAIN": "0", "EVE_MINIAPP": "0", "EVE_VOICE": "0",
            "EVE_COMPUTER_CONTROL": "0", "EVE_LIGHTS": "0", "EVE_CANVAS": "0", "EVE_CHIEF": "off", "EVE_SHARE": "0",
            "EVE_AGENTCARD": "0", "EVE_SEARCH_ROUTER": "off", "EVE_WEB_READER": "0", "EVE_WATCH_TLS": "0",
            "EVE_WATCH_BROWSER": "0", "EVE_WATCH_HOSTED": "0", "EVE_APPS": "0", "EVE_COMPOSIO": "0"}
FULL_ENV = {"OPENROUTER_API_KEY": PLACEHOLDER, "EVE_TELEGRAM": "1", "EVE_TELEGRAM_TOKEN": PLACEHOLDER,
            "EVE_TELEGRAM_OWNER": "4242", "EVE_MEMORY": "1", "EVE_VOICE": "0", "EVE_WATCH_TLS": "0",
            "EVE_WATCH_BROWSER": "0", "EVE_APPS": "0"}
# What a lite block never says: the doors, devices and parts eve lite does not have (no "off" lines either)
NOT_LITE = ("Telegram", "iPhone", "Mini App", "Eve Link", "Tasks on the Mac", "Lights", "Canvas", "File sharing",
            "Chief of staff", "AgentCard", "Voice:", "Own apps", "Composio apps", "Running code")


@pytest.fixture
def home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A HOME of the test's own: the Connect sign-in file is looked for under it."""
    monkeypatch.setenv("HOME", str(tmp_path))
    return tmp_path


def connect_file(home: Path) -> Path:
    path = home / ".config" / "eve-bridge" / "composio-connect.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({"version": 1, "access_token": "CANARY-NOT-A-KEY-7f3c"}), encoding="utf-8")
    return path


def test_the_lite_block_is_eve_lites_parts_in_order(home: Path) -> None:
    lines = self_context.lines(LITE_ENV)
    tg, tc = telegram.configured(LITE_ENV), think.configured(LITE_ENV)
    assert lines == [
        f"Chat brain (eve's web app: texts and push-to-talk): {tg.model}, {tg.effort} effort.",
        f"think_hard: {tc.model}, {tc.effort} effort.",
        self_context._search(LITE_ENV), self_context._watch(LITE_ENV), self_context._memory(LITE_ENV),
        "Doors on: eve's web app.",
    ]
    assert lines[2].startswith("Web search: ") and lines[3].startswith("Watcher ladder: ")
    text = "\n".join(lines)
    assert not [w for w in NOT_LITE if w in text], text


def test_the_apps_line_is_there_only_while_the_owner_is_signed_in_to_composio_connect(home: Path) -> None:
    assert not any(ln.startswith("Apps:") for ln in self_context.lines(LITE_ENV))
    path = connect_file(home)
    lines = self_context.lines(LITE_ENV)
    assert lines[-1] == self_context.LITE_APPS == ("Apps: the owner's apps connected in Composio, through the "
                                                   "COMPOSIO tools.")
    assert "CANARY" not in "\n".join(lines)                       # the file's presence is read, never its tokens
    path.unlink()
    assert self_context.LITE_APPS not in self_context.lines(LITE_ENV)
    # control: full eve has no such line, signed in or not
    connect_file(home)
    assert self_context.LITE_APPS not in self_context.lines(FULL_ENV, commit="")


def test_the_block_rides_the_lite_brains_config_inside_its_budget(home: Path) -> None:
    connect_file(home)
    block = self_context.block(LITE_ENV)
    assert block.startswith(self_context.HEADER) and block.endswith(self_context.RULE)
    assert len(block) <= self_context.BUDGET
    assert telegram.configured(LITE_ENV).about == block
    assert "- Chat brain (eve's web app" in block and "- Doors on: eve's web app." in block


def test_the_commit_line_starts_no_git_in_lite_and_does_in_full_eve(monkeypatch: pytest.MonkeyPatch,
                                                                   home: Path) -> None:
    ran: list[list[str]] = []

    def spy(argv: list[str], *_: object, **__: object) -> subprocess.CompletedProcess[str]:
        ran.append(list(argv))
        return subprocess.CompletedProcess(argv, 0, stdout="abc1234\n", stderr="")

    monkeypatch.setattr(subprocess, "run", spy)
    self_context.running_commit.cache_clear()
    try:
        monkeypatch.setenv("EVE_PROFILE", "lite")
        assert self_context.running_commit() == ""
        lines = self_context.lines(LITE_ENV)
        assert ran == [] and not any("git commit" in ln for ln in lines)
        assert self_context.lines(LITE_ENV, commit="abc1234")[-1] == "Running code: git commit abc1234."
        # control: full eve still reads its commit with git (a process is read once, so the cache is cleared)
        monkeypatch.delenv("EVE_PROFILE")
        self_context.running_commit.cache_clear()
        assert self_context.running_commit() == "abc1234" and ran and ran[0][0] == "git"
        assert self_context.lines(FULL_ENV)[-1] == "Running code: git commit abc1234."
        assert len(ran) == 1                                               # once per process, as before
    finally:
        self_context.running_commit.cache_clear()


def test_each_block_follows_the_environment_it_is_given(monkeypatch: pytest.MonkeyPatch, home: Path) -> None:
    """profile.is_lite is scoped to the mapping passed in: the process's own profile never leaks into a block built
    for another environment (the owner golden builds full eve's under any caller)."""
    monkeypatch.setenv("EVE_PROFILE", "lite")
    full = self_context.lines(FULL_ENV, commit="")
    assert full[0].startswith("Telegram brain (texts, calls, desk push-to-talk)")
    assert any(ln.startswith("Doors on: Telegram") for ln in full)
    monkeypatch.delenv("EVE_PROFILE")
    lite = self_context.lines(LITE_ENV)
    assert lite[0].startswith("Chat brain (eve's web app") and "Telegram" not in "\n".join(lite)


def test_the_lite_block_never_imports_the_voice_the_ears_or_the_mini_app(tmp_path: Path) -> None:
    """eve lite's process never loads full eve's voice halves or Mini App to describe them: a fresh interpreter."""
    code = textwrap.dedent(f"""
        import json, sys
        from eve_bridge import self_context
        self_context.lines({LITE_ENV!r})
        print(json.dumps(sorted(m for m in ("eve_bridge.voice_agent", "eve_bridge.ears", "eve_bridge.miniapp",
                                             "eve_bridge.computer_agent", "eve_bridge.lights") if m in sys.modules)))
    """)
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(tmp_path)}
    out = subprocess.run([sys.executable, "-c", code], env=env, capture_output=True, text=True, timeout=120)
    assert out.returncode == 0, out.stderr[-2000:]
    assert json.loads(out.stdout.strip().splitlines()[-1]) == []
