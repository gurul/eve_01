"""claude_launch.py: the "new claude" decision tree, the harness rule, and the Telegram door into it."""

from __future__ import annotations

import asyncio
import json
import shlex
from pathlib import Path
from typing import Any, Optional

import pytest

from eve_bridge import claude_launch as cl
from eve_bridge import telegram
from eve_bridge.telegram import TelegramConfig, TelegramInlet

OWNER = 4242
NOW = 1_800_000_000.0
CFG = TelegramConfig(enabled=True, token="123456:AAsecretTOKENvalue", owner_ids=frozenset({OWNER}))


@pytest.fixture
def root(tmp_path: Path) -> Path:
    for name in ("eve", "car", "portfolio", "spotify-knob"):
        (tmp_path / "personal" / name).mkdir(parents=True)
    for name in ("era-maker", "era-maker-213", "era-maker-214", "era-hub-api", "era-ingress", "portfolio",
                 "spotify-knob", ".hidden"):
        (tmp_path / "work" / name).mkdir(parents=True)
    (tmp_path / "work" / "notes.txt").write_text("not a folder")
    (tmp_path / "Second Brain").mkdir()
    return tmp_path


def flow(root: Path, **kw: Any) -> cl.LaunchFlow:
    return cl.LaunchFlow(root, cl.DEFAULT_AREAS, **kw)


# ---- matching and the list ----------------------------------------------------------------------

def test_match_is_forgiving_and_exact_wins() -> None:
    names = ["era-maker", "era-maker-213", "era-hub-api", "Era-Hub"]
    assert cl.match("era maker", names) == ["era-maker"]            # spaces for dashes
    assert cl.match("ERA_HUB_API", names) == ["era-hub-api"]
    assert cl.match("era-mak", names) == ["era-maker", "era-maker-213"]   # prefix, shortest first
    assert cl.match("hub", names) == ["Era-Hub", "era-hub-api"]            # substring
    assert cl.match("era-hub-apu", names)[0] == "era-hub-api"            # a close spelling, best first
    assert cl.match("zzz", names) == []


def test_menu_folds_numbered_worktrees_into_their_base() -> None:
    names = ["era-maker", "era-maker-213", "era-maker-214", "fw-ERA-4640", "era-hub-api"]
    assert cl.menu(names) == [("era-maker", 2), ("fw-ERA-4640", 0), ("era-hub-api", 0)]


def test_folders_skip_hidden_and_files(root: Path) -> None:
    assert ".hidden" not in cl.folders(root / "work")
    assert "notes.txt" not in cl.folders(root / "work")


# ---- the tree -----------------------------------------------------------------------------------

def test_first_question_is_personal_or_work(root: Path) -> None:
    step = flow(root).start()
    assert step.text.startswith("Personal or Work?")
    assert step.folder is None and not step.done


def test_area_then_general_opens_the_area_itself(root: Path) -> None:
    f = flow(root)
    f.start()
    assert "general, or which folder" in f.answer("work").text
    step = f.answer("general")
    assert step.done and step.folder == root / "work" and step.harness == "era-code"


def test_area_then_folder_name(root: Path) -> None:
    f = flow(root)
    f.start()
    f.answer("p")                                                     # one letter picks the area
    step = f.answer("Eve")
    assert step.folder == root / "personal" / "eve" and step.harness == "claude"


def test_list_shows_folded_folders_and_takes_a_number(root: Path) -> None:
    f = flow(root)
    f.start("work")
    step = f.answer("list")
    assert "era-maker (+2 numbered)" in step.text
    assert "era-maker-213" not in step.text and ".hidden" not in step.text
    n = next(line.split(".")[0] for line in step.text.splitlines() if line.endswith("era-ingress"))
    assert f.answer(n).folder == root / "work" / "era-ingress"


def test_a_folder_name_first_skips_the_area_when_unique(root: Path) -> None:
    step = flow(root).start("car")
    assert step.folder == root / "personal" / "car" and step.harness == "claude"


def test_a_name_in_both_areas_becomes_a_numbered_choice(root: Path) -> None:
    f = flow(root)
    step = f.start("portfolio")
    assert "1. portfolio (personal)" in step.text and "2. portfolio (work)" in step.text
    picked = f.answer("2")
    assert picked.folder == root / "work" / "portfolio" and picked.harness == "era-code"


def test_one_line_request_with_area_and_folder(root: Path) -> None:
    step = flow(root).start("work era hub api")
    assert step.folder == root / "work" / "era-hub-api"


def test_unknown_folder_says_so_and_stays_open(root: Path) -> None:
    f = flow(root)
    step = f.start("work nope")
    assert "No folder like" in step.text and not step.done
    assert f.answer("era-ingress").folder == root / "work" / "era-ingress"


def test_out_of_range_number(root: Path) -> None:
    f = flow(root)
    f.start("portfolio")
    assert "from 1 to 2" in f.answer("7").text


def test_recent_numbers_are_offered_first(root: Path) -> None:
    recent = [cl.Recent(root / "work" / "era-maker", "claude"), cl.Recent(root / "gone", "claude")]
    f = flow(root, recent=recent)
    step = f.start()
    assert "1. era-maker (work)" in step.text and "gone" not in step.text
    picked = f.answer("1")
    assert picked.folder == root / "work" / "era-maker"
    assert picked.harness == "era-code"          # the area decides, not what that session used last time


# ---- the harness: implicit by area, switched only when said -------------------------------------

@pytest.mark.parametrize("text,words,harness", [
    ("work era-maker", "work era-maker", None),
    ("era maker, but I want it in claude instead of era code", "era maker", "claude"),
    ("eve instead of claude", "eve", "era-code"),
    ("eve in era code", "eve", "era-code"),
    ("work atlas claude instead", "work atlas", "claude"),
    ("car with claude code", "car", "claude"),
    ("work claude-debrief", "work claude-debrief", None),      # a folder named claude-something stays a name
])
def test_split_harness(text: str, words: str, harness: Optional[str]) -> None:
    assert cl.split_harness(text) == (words, harness)


def test_explicit_override_switches_the_area_default(root: Path) -> None:
    step = flow(root).start("work era maker, but I want it in claude instead of era code")
    assert step.folder == root / "work" / "era-maker" and step.harness == "claude"
    step = flow(root).start("eve in era-code")
    assert step.folder == root / "personal" / "eve" and step.harness == "era-code"


def test_override_given_early_survives_later_questions(root: Path) -> None:
    f = flow(root)
    f.start("work with claude")
    assert f.answer("general").harness == "claude"


def test_request_is_the_tool_path(root: Path) -> None:
    assert flow(root).request("work", "era maker", "").harness == "era-code"
    assert flow(root).request("work", "era maker", "claude").harness == "claude"
    assert "general, or which folder" in flow(root).request("personal", "", "").text
    assert flow(root).request("", "", "").text.startswith("Personal or Work?")
    assert flow(root).request("", "spotify knob", "").text.startswith("Which one?")


def test_trigger_words() -> None:
    for text in ("new claude", "/newclaude", "New Claude session", "claude new work era-maker", "start claude eve"):
        assert cl.TRIGGER.match(text), text
    for text in ("claude on", "claude: fix the tests", "open claude app", "renew claude"):
        assert not cl.TRIGGER.match(text), text


# ---- opening the terminal -----------------------------------------------------------------------

def test_warp_config_is_quoted_yaml(tmp_path: Path) -> None:
    folder = tmp_path / 'we"ird: name'
    text = cl.warp_config(folder, cl.HARNESSES["era-code"])
    assert 'name: "eve-claude"' in text
    assert f"cwd: {json.dumps(str(folder))}" in text
    assert 'exec: "era-code claude --dangerously-skip-permissions"' in text


def test_open_session_warp_writes_config_opens_by_name_and_remembers(tmp_path: Path, root: Path) -> None:
    calls: list[tuple[str, ...]] = []

    async def run(*argv: str) -> tuple[int, str]:
        calls.append(argv)
        return 0, ""

    recent = tmp_path / "recent.json"
    said = asyncio.run(cl.open_session(root / "work" / "era-maker", "era-code", app="warp",
                                       warp_dir=tmp_path / "warp", recent_path=recent, run=run))
    assert calls == [("open", "warp://launch/eve-claude")]
    assert "era-code claude --dangerously-skip-permissions" in (tmp_path / "warp" / "eve-claude.yaml").read_text()
    assert "starting in era-maker" in said
    assert cl.load_recent(recent) == [cl.Recent(root / "work" / "era-maker", "era-code")]


def test_open_session_terminal_quotes_the_path(tmp_path: Path) -> None:
    folder = tmp_path / "it's here"
    folder.mkdir()
    calls: list[tuple[str, ...]] = []

    async def run(*argv: str) -> tuple[int, str]:
        calls.append(argv)
        return 0, ""

    asyncio.run(cl.open_session(folder, "claude", app="terminal", recent_path=tmp_path / "r.json", run=run))
    script = calls[0][2]
    assert "claude --dangerously-skip-permissions" in script
    quoted = shlex.quote(str(folder)).replace("\\", "\\\\").replace('"', '\\"')
    assert f"cd {quoted} && " in script                             # shell-quoted, then AppleScript-escaped


def test_open_session_failure_is_reported_and_not_remembered(tmp_path: Path, root: Path) -> None:
    async def run(*argv: str) -> tuple[int, str]:
        return 1, "no such app"

    recent = tmp_path / "recent.json"
    said = asyncio.run(cl.open_session(root / "personal" / "car", "claude", app="warp",
                                       warp_dir=tmp_path / "warp", recent_path=recent, run=run))
    assert "couldn't open Warp" in said and not recent.exists()


def test_recent_keeps_five_newest_without_duplicates(tmp_path: Path) -> None:
    path = tmp_path / "r.json"
    for i in range(7):
        cl.remember(tmp_path / str(i), "claude", path)
    cl.remember(tmp_path / "3", "era-code", path)
    got = cl.load_recent(path)
    assert [r.folder.name for r in got] == ["3", "6", "5", "4", "2"]
    assert got[0].harness == "era-code"


# ---- the Telegram door --------------------------------------------------------------------------

class Api:
    def __init__(self) -> None:
        self.sent: list[str] = []
        self.buttons: list[list[str]] = []

    async def send_message(self, chat_id: int, text: str, title: Optional[str] = None,
                           subtitle: Optional[str] = None, buttons: Any = ()) -> None:
        self.sent.append(text)
        self.buttons.append(list(buttons))


def message(text: str, update_id: int = 1) -> dict[str, Any]:
    return {"update_id": update_id, "message": {
        "message_id": update_id, "date": int(NOW), "text": text,
        "from": {"id": OWNER, "is_bot": False, "first_name": "Owner"},
        "chat": {"id": OWNER, "type": "private"}}}


class Rig:
    def __init__(self, root: Path) -> None:
        self.api = Api()
        self.opened: list[tuple[Path, str]] = []

        async def launcher(folder: Path, harness: str) -> str:
            self.opened.append((folder, harness))
            return "opened"

        async def no_model(request: dict[str, Any]) -> dict[str, Any]:
            raise AssertionError("the tree is code; no model call")

        self.inlet = TelegramInlet(CFG, self.api, no_model, launcher=launcher, launch_root=root,
                                   launch_recent=lambda: [], wall=lambda: NOW)

    async def text(self, text: str) -> None:
        self.inlet._dispatch(message(text))
        await asyncio.gather(*list(self.inlet._jobs))


def test_telegram_walks_the_tree_without_a_model(root: Path) -> None:
    async def go() -> Rig:
        rig = Rig(root)
        await rig.text("new claude")
        await rig.text("work")
        await rig.text("era maker")
        return rig

    rig = asyncio.run(go())
    assert rig.api.sent[0].startswith("Personal or Work?")
    assert "general, or which folder" in rig.api.sent[1]
    assert rig.opened == [(root / "work" / "era-maker", "era-code")]
    assert rig.api.sent[-1] == "opened"
    assert rig.inlet._launch is None


def test_telegram_one_line_with_override(root: Path) -> None:
    async def go() -> Rig:
        rig = Rig(root)
        await rig.text("new claude work era-maker in claude instead of era code")
        return rig

    assert asyncio.run(go()).opened == [(root / "work" / "era-maker", "claude")]


def test_telegram_cancel_closes_the_tree(root: Path) -> None:
    async def go() -> Rig:
        rig = Rig(root)
        await rig.text("new claude")
        await rig.text("cancel")
        return rig

    rig = asyncio.run(go())
    assert rig.api.sent[-1] == "Okay, no new session." and rig.inlet._launch is None and not rig.opened


def test_telegram_tool_path_asks_then_the_reply_answers(root: Path) -> None:
    async def go() -> tuple[Rig, dict[str, Any]]:
        rig = Rig(root)
        result = await rig.inlet._tool("start_coding_session", {"area": "work", "folder": "", "harness": ""}, OWNER)
        await asyncio.gather(*list(rig.inlet._jobs))
        await rig.text("era-hub-api")
        return rig, result

    rig, result = asyncio.run(go())
    assert result["ok"] is True
    assert "general, or which folder" in rig.api.sent[0]
    assert rig.opened == [(root / "work" / "era-hub-api", "era-code")]


def test_every_offered_tool_is_accepted_when_the_model_calls_it() -> None:
    """TOOL_NAMES is what parse_response accepts; a tool offered but not accepted fails the turn (the
    start_coding_session bug of 2026-09-23: it was offered, then refused as 'unexpected')."""
    from eve_bridge.memory import TOOLS as MEMORY_TOOLS

    def call(name: str) -> dict:
        return {"output": [{"type": "function_call", "name": name, "call_id": "c1", "arguments": "{}"}]}

    for tool in telegram.TOOLS:
        calls, _, _ = telegram.parse_response(call(tool["name"]))
        assert [c["name"] for c in calls] == [tool["name"]]
    # The memory tools are offered per turn (only with memory on), so they are accepted as that turn's lent tools.
    offered = {tool["name"] for tool in MEMORY_TOOLS}
    for name in offered:
        calls, _, _ = telegram.parse_response(call(name), offered)
        assert [c["name"] for c in calls] == [name]
        with pytest.raises(RuntimeError):
            telegram.parse_response(call(name))          # not offered this turn: refused


# ---- tap buttons and "claude on" -----------------------------------------------------------------

def test_every_question_offers_tap_buttons_and_a_tapped_number_picks(root: Path) -> None:
    flow = cl.LaunchFlow(root, clock=lambda: 0.0)
    step = flow.start()
    assert step.buttons[:2] == ["Personal", "Work"] and step.buttons[-1] == "Cancel"
    step = flow.answer("Work")
    assert step.buttons == ["List", "General", "Cancel"]
    step = flow.answer("List")
    assert step.buttons[-2:] == ["General", "Cancel"]
    tap = next(b for b in step.buttons if b.endswith(". era-hub-api"))
    step = flow.answer(tap)                              # the button's own text, as Telegram sends it
    assert step.done and step.folder == root / "work" / "era-hub-api"


class JoinRig(Rig):
    def __init__(self, root: Path, sessions: list[str]) -> None:
        super().__init__(root)
        self.inlet._claude_sessions = lambda: sessions


def test_claude_on_with_nothing_running_walks_the_tree_then_joins_what_opens(root: Path) -> None:
    async def go() -> JoinRig:
        rig = JoinRig(root, [])
        await rig.text("claude on")
        assert rig.inlet.claude is False and rig.api.sent[-1].startswith(telegram.CLAUDE_NONE_LINE)
        assert "Work" in rig.api.buttons[-1]
        await rig.text("Work")
        await rig.text("era maker")
        return rig

    rig = asyncio.run(go())
    assert rig.opened == [(root / "work" / "era-maker", "era-code")]
    assert rig.inlet.claude is True
    assert rig.inlet._relay_pin == str((root / "work" / "era-maker").resolve())
    assert rig.api.sent[-1] == telegram.CLAUDE_ON_LINE


def test_claude_on_with_several_sessions_asks_which_and_follows_only_that_one(root: Path) -> None:
    a, b = str(root / "personal" / "eve"), str(root / "work" / "era-maker")

    async def go() -> JoinRig:
        rig = JoinRig(root, [a, b])
        await rig.text("claude on")
        assert rig.inlet.claude is False and rig.api.sent[-1] == telegram.CLAUDE_PICK_LINE
        assert rig.api.buttons[-1] == ["claude on eve", "claude on era-maker", telegram.NEW_CLAUDE_BUTTON]
        await rig.text("claude on era-maker")
        assert rig.inlet.claude and rig.inlet._relay_pin == b
        before = len(rig.api.sent)
        await rig.inlet.relay_text("from the other session", a)
        await rig.inlet.relay_text("from the picked one", b)
        await asyncio.gather(*list(rig.inlet._jobs))
        assert rig.api.sent[before:] == ["from the picked one"]
        await rig.text("claude off")
        assert rig.inlet._relay_pin == "" and not rig.inlet.claude
        return rig

    asyncio.run(go())


def test_claude_on_with_one_session_joins_it_at_once(root: Path) -> None:
    async def go() -> JoinRig:
        rig = JoinRig(root, [str(root / "personal" / "eve")])
        await rig.text("claude on")
        return rig

    rig = asyncio.run(go())
    assert rig.inlet.claude and rig.api.sent == [telegram.CLAUDE_ON_LINE] and not rig.opened
