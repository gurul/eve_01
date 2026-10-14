"""The owner golden (tools/owner_golden.py, WP-1 of docs/plans/eve-lite.md): with EVE_PROFILE unset, full eve's
Telegram config, request bodies, app block, self-context, Composio config and verdicts, tool_gate readings, tool
lists, launchd plists and `eve-bridge daemon` are what main's code made of them, byte for byte.

Every check has its positive control: the owner removed from the environment, a prompt or a plist changed in code,
a process started while the snapshot is taken, a command that builds no Daemon, and a golden that does not match
are each caught; the caller's own environment, EVE_PROFILE=lite included, never reaches the snapshot."""

from __future__ import annotations

import copy
import json
import os
import re
import subprocess
from pathlib import Path
from typing import Any

import owner_golden
import pytest

from eve_bridge import _service_launchd, telegram

TURNS = {"telegram_turn", "app_turn", "telegram_turn_composio_tools", "app_turn_composio_tools"}


@pytest.fixture(scope="module")
def golden() -> dict[str, Any]:
    return owner_golden.load()


@pytest.fixture(scope="module")
def now() -> dict[str, Any]:
    return owner_golden.snapshot()


def test_full_eve_is_byte_for_byte_what_main_recorded(golden: dict[str, Any], now: dict[str, Any]) -> None:
    diffs = owner_golden.differences(golden["snapshot"], now)
    assert not diffs, "full eve changed (tools/owner_golden.py --dump DIR shows the texts):\n" + "\n".join(diffs[:30])


def test_the_golden_holds_every_part_the_spec_names(golden: dict[str, Any]) -> None:
    """A golden of empty sections would match anything: each part holds what it was recorded for."""
    snap, envs = golden["snapshot"], set(owner_golden.ENVS)
    assert re.fullmatch(r"[0-9a-f]{7}", golden["base"])
    assert set(owner_golden.SECTIONS) <= set(snap)
    for env in envs:
        tg = snap["telegram"][env]
        assert tg["enabled"] and tg["owner_ids"] == [int(owner_golden.OWNER)] and tg["token"] == "<set>"
        assert tg["app_only"] == env.startswith("app_only") and tg["about"]["chars"] > 0
        requests = snap["requests"][env]
        assert set(requests) == TURNS
        # an app turn reads the app block where a Telegram turn reads the texting style; tools are the same
        assert requests["app_turn"]["instructions"] != requests["telegram_turn"]["instructions"]
        assert requests["app_turn"]["tools"] == requests["telegram_turn"]["tools"]
        for turn, request in requests.items():
            composio = [n for n in request["tools"] if n.startswith("COMPOSIO_")]
            assert composio == (list(owner_golden.COMPOSIO_NAMES) if turn.endswith("_composio_tools") else [])
            assert {"web_search", "memory_search", "recent_messages", "watch_add"} <= set(request["tools"])
            # the speed dial grades app turns only, and each grade is its own body
            graded = request.get("graded_sha256", {})
            assert set(graded) == ({"instant", "think"} if turn.startswith("app_turn") else set())
            assert request["body_sha256"] not in graded.values() and len(set(graded.values())) == len(graded)
        has_composio = env.endswith("_composio")
        assert snap["composio"]["configured"][env]["enabled"] == has_composio
        assert any(line.startswith("Composio apps:") for line in snap["self_context"][env]["lines"]) == has_composio
    assert snap["telegram"]["app_only"]["about"] != snap["telegram"]["app_only_composio"]["about"]
    assert snap["app_block"]["chars"] > 1000
    verdicts = {v[p][0] for v in snap["composio"]["decide"]["cases"].values() for p in owner_golden.POLICIES}
    assert verdicts == {"run", "ask", "refuse"}
    assert snap["tool_gate"]["COMPOSIO_SEARCH_TOOLS"]["source"] == "Slack, Notion or Linear"
    tools = snap["tools"]
    assert set(tools["own_tools"]) < set(tools["own_tools_robot"])
    assert tools["menu_tools"] and not tools["menu_tools_off"]
    plist = snap["plist"]
    assert owner_golden.SERIAL_GLOB in plist["daemon_serial"]["xml"]
    assert "EVE_SERIAL_PORT" not in plist["daemon_plain"]["xml"]
    assert "EVE_CLAUDE_CONFIG_DIRS" in plist["daemon_wrapper_home"]["xml"]
    assert all(owner_golden.PLIST_HOME in unit["xml"] for unit in plist.values())
    assert snap["cli"]["daemon"]["builds"] == "eve_bridge.daemon.Daemon"
    assert snap["hermetic"]["processes"] == []


def test_an_environment_without_the_owner_does_not_match(golden: dict[str, Any]) -> None:
    """The spec's positive control: the same environments with EVE_TELEGRAM_OWNER removed differ from the golden,
    in each environment's door and in its request bodies."""
    mutated = owner_golden.snapshot({k: owner_golden.drop_owner(env) for k, env in owner_golden.ENVS.items()})
    diffs = owner_golden.differences(golden["snapshot"], mutated)
    for env in owner_golden.ENVS:
        assert f"telegram.{env}.enabled: golden true now false" in diffs
        assert any(line.startswith(f"requests.{env}.app_turn.instructions.sha256:") for line in diffs)
    assert owner_golden.control_misses(golden["snapshot"]) == []


def test_a_prompt_or_a_plist_changed_in_code_does_not_match(golden: dict[str, Any],
                                                          monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(telegram, "INSTRUCTIONS", telegram.INSTRUCTIONS + "\nOne more rule.")
    monkeypatch.setattr(_service_launchd, "LABEL", "com.example.another-daemon")
    diffs = owner_golden.differences(golden["snapshot"], owner_golden.snapshot())
    for env in owner_golden.ENVS:
        for turn in TURNS:
            assert any(line.startswith(f"requests.{env}.{turn}.instructions.sha256:") for line in diffs)
    assert any(line.startswith("plist.daemon_serial.sha256:") for line in diffs)
    assert not any(line.startswith("plist.widget.") for line in diffs)       # the widget has a label of its own


def test_a_process_started_while_the_snapshot_is_taken_is_refused_and_recorded(
        golden: dict[str, Any], monkeypatch: pytest.MonkeyPatch) -> None:
    real = telegram.recall_line
    refused: list[str] = []

    def recall_and_spawn(chat_log: bool, memory: bool) -> str:
        for start in (lambda: subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, check=False),
                      lambda: os.system("uname -a")):
            try:
                start()
            except PermissionError as e:
                refused.append(str(e))
        return real(chat_log, memory)

    monkeypatch.setattr(telegram, "recall_line", recall_and_spawn)
    snap = owner_golden.snapshot()
    assert refused and all("no process starts" in why for why in refused)      # refused, never run
    assert snap["hermetic"]["processes"] == ["git", "uname"]
    assert ('hermetic.processes: added ["git", "uname"], removed []'
            in owner_golden.differences(golden["snapshot"], snap))


def test_the_callers_environment_never_reaches_the_snapshot(golden: dict[str, Any], tmp_path: Path,
                                                            monkeypatch: pytest.MonkeyPatch) -> None:
    for name, value in {"EVE_PROFILE": "lite", "EVE_TELEGRAM_OWNER": "7", "EVE_TELEGRAM": "0", "EVE_ROBOT": "1",
                        "EVE_MENU_TOOLS": "off", "EVE_COMPOSIO": "1", "EVE_COMPOSIO_POLICY": "gmail=auto",
                        "EVE_SERIAL_PORT": "/dev/cu.another", "CLAUDE_CONFIG_DIR": str(tmp_path / "claude"),
                        "EVE_WEB_SEARCH": "off", "HOME": str(tmp_path)}.items():
        monkeypatch.setenv(name, value)
    assert owner_golden.differences(golden["snapshot"], owner_golden.snapshot()) == []


def test_eve_bridge_daemon_builds_the_daemon_and_a_command_that_does_not_is_seen() -> None:
    seen = owner_golden.daemon_command()
    assert seen["builds"] == "eve_bridge.daemon.Daemon" and seen["exit"] is None
    assert seen["kwargs"]["serial_port"] == owner_golden.SERIAL_GLOB and seen["kwargs"]["socket_path"] == "<socket>"
    control = owner_golden.daemon_command(["--version"])
    assert control == {"builds": None, "args": [], "kwargs": {}, "exit": 0}


def test_check_prints_its_mark_only_for_a_golden_that_matches(golden: dict[str, Any], tmp_path: Path,
                                                             capsys: pytest.CaptureFixture[str]) -> None:
    assert owner_golden.check() == 0
    assert owner_golden.MARK in capsys.readouterr().out
    tampered = copy.deepcopy(golden)
    case = tampered["snapshot"]["composio"]["decide"]["cases"]["multi, gmail send"]
    assert case["default"][0] == "refuse"
    case["default"][0] = "run"
    path = tmp_path / "tampered.json"
    path.write_text(json.dumps(tampered), encoding="utf-8")
    assert owner_golden.check(path) == 1
    out = capsys.readouterr().out
    assert owner_golden.MARK not in out and "composio.decide.cases.multi, gmail send.default" in out
    del tampered["snapshot"]["plist"]
    path.write_text(json.dumps(tampered), encoding="utf-8")
    assert owner_golden.check(path) == 1
    assert "the golden has no plist section" in capsys.readouterr().out


def test_write_records_only_from_mains_code_and_loses_no_value(golden: dict[str, Any], tmp_path: Path,
                                                              monkeypatch: pytest.MonkeyPatch) -> None:
    """--write is refused on a tree that is not main's (a branch must never re-record the golden to pass), and
    what it writes reads back as the snapshot it took. Both on a temporary path: the real golden is not touched."""
    path = tmp_path / "golden.json"
    monkeypatch.setattr(owner_golden, "_main_base", lambda: ("abc1234", "bridge/src differs from abc1234"))
    assert owner_golden.write(path) == 1 and not path.exists()
    monkeypatch.setattr(owner_golden, "_main_base", lambda: ("abc1234", ""))
    assert owner_golden.write(path) == 0
    written = owner_golden.load(path)
    assert written["base"] == "abc1234" and written["snapshot"] == golden["snapshot"]


# What a public repository never holds (AGENTS rule 2), and what would mean the sandbox leaked into the golden.
LEAKS = re.compile(r"/Users/|/var/folders/|/private/|owner-golden-|" + re.escape(owner_golden.SET)
                   + r"|\bsk-|ghp_|@gmail")


def test_the_golden_names_no_home_path_no_temporary_folder_and_no_key() -> None:
    assert LEAKS.search("/Users/someone/.config") and LEAKS.search(owner_golden.SET)     # the pattern finds them
    found = LEAKS.findall(owner_golden.GOLDEN.read_text(encoding="utf-8"))
    assert found == []
