"""The / menu's commands as the chat brain's tools (menu_tools.py), behind EVE_MENU_TOOLS. No network: the
model is FakeCreate, Telegram is FakeApi. The eval harness's own tests are in test_intent_eval.py."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any

import pytest
from test_telegram import CODEX_FOLDER, OWNER, FakeApi, FakeCodex, FakeCreate, Rig, call, say, settle, update

from eve_bridge import events, menu_tools, opentable, rundown, spend, telegram, toolset_router


@pytest.fixture(autouse=True)
def _menu_tools_on(monkeypatch: pytest.MonkeyPatch) -> None:
    """These tests are of the tools themselves: the switch is on (it ships off until the blind eval passes)."""
    monkeypatch.setenv("EVE_MENU_TOOLS", "on")


def _in(text: str, message_id: int = 7) -> telegram.Inbound:
    return telegram.Inbound(OWNER, OWNER, text, message_id=message_id)


async def _drain(rig: Rig) -> None:
    for _ in range(5):
        await asyncio.gather(*list(rig.inlet._jobs))
        await settle(20)


# ---- what every turn carries ------------------------------------------------------------------------------------

def test_every_menu_tool_is_lent_allowed_and_dispatched_on_every_turn() -> None:
    rig = Rig(FakeApi(), FakeCreate())
    parts, allowed, _ = asyncio.run(rig.inlet._turn_parts(0))
    lent = [t["name"] for t in parts["watch_tools"]]
    assert lent[:len(menu_tools.TOOL_NAMES)] == list(menu_tools.TOOL_NAMES)      # first, fixed: the cached prefix
    assert set(menu_tools.TOOL_NAMES) <= set(allowed)
    assert all(telegram.TOOL_HANDLERS[n] == telegram.TelegramInlet._tool_menu for n in menu_tools.TOOL_NAMES)
    assert events.TOOL_NAME not in lent and "events_search" not in parts["watch_block"]   # no key in tests


def test_events_search_is_lent_with_the_key_and_its_line_names_only_what_is_offered(
        monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("TICKETMASTER_API_KEY", "tm-test")
    rig = Rig(FakeApi(), FakeCreate())
    parts, allowed, _ = asyncio.run(rig.inlet._turn_parts(0))
    assert events.TOOL_NAME in [t["name"] for t in parts["watch_tools"]] and events.TOOL_NAME in allowed
    assert events.instructions(False) in parts["watch_block"] and "watch_add" not in parts["watch_block"]
    assert telegram.TOOL_HANDLERS[events.TOOL_NAME] == telegram.TelegramInlet._tool_events


def test_the_switch_off_lends_nothing_and_writes_no_line(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("TICKETMASTER_API_KEY", "tm-test")
    monkeypatch.setenv("EVE_MENU_TOOLS", "off")
    rig = Rig(FakeApi(), FakeCreate(), opentable_enabled=True)
    parts, allowed, _ = asyncio.run(rig.inlet._turn_parts(0))
    names = [t["name"] for t in parts["watch_tools"]]
    assert not set(menu_tools.TOOL_NAMES) & set(names) and events.TOOL_NAME not in names
    assert not set(menu_tools.TOOL_NAMES) & set(allowed)
    assert telegram.INTENT_WORDS not in parts["watch_block"] and "events_search" not in parts["watch_block"]
    # OpenTable keeps its narrow wording, byte for byte
    assert opentable.tools()[0] in parts["watch_tools"] and opentable.instructions() in parts["watch_block"]
    for value in ("", "maybe"):
        monkeypatch.setenv("EVE_MENU_TOOLS", value)
        assert menu_tools.enabled() is menu_tools.SHIPPED


def test_the_switch_on_writes_the_tool_choice_lines_naming_only_offered_tools() -> None:
    rig = Rig(FakeApi(), FakeCreate(), opentable_enabled=True)
    parts, allowed, _ = asyncio.run(rig.inlet._turn_parts(0))
    block = parts["watch_block"]
    assert block.startswith(telegram.INTENT_WORDS)
    assert "opentable_search" in block and opentable.WIDE_DESCRIPTION in json.dumps(parts["watch_tools"])
    assert "codex_relay" in block and "start_coding_session" in block
    # no Fire TV, no chief here: their lines are not written, and the plain web line says where showtimes go
    assert "firetv_control" not in block and "jobs_list" not in block
    assert "theater showtimes are web_search" in block


def test_intent_block_writes_a_line_only_when_every_tool_it_names_is_offered() -> None:
    every = {n for needs, _ in telegram.INTENT_LINES for n in needs}
    full = telegram.intent_block(every)
    assert all(line in full for needs, line in telegram.INTENT_LINES
               if needs not in (("web_search",), telegram._FILES_NO_READ))
    # the files line comes in one of two forms: with read_file when it is offered, else without (never both)
    no_read = next(line for needs, line in telegram.INTENT_LINES if needs == telegram._FILES_NO_READ)
    assert no_read not in full and no_read in telegram.intent_block(every - {"read_file"})
    assert "theater showtimes" not in full                      # the movie line covers them
    assert telegram.intent_block(set()) == telegram.INTENT_WORDS
    only_jobs = telegram.intent_block({"jobs_list"})
    assert "jobs_list" in only_jobs and "web_search" not in only_jobs and "opentable_search" not in only_jobs


def test_every_tool_name_a_written_block_holds_is_offered() -> None:
    """The owner's rule: no name in a prompt that the turn does not offer. Every snake_case token of the block that
    names some tool eve has must be in the offered set, for the full set and for each line's own set."""
    import re

    known = ({n for needs, _ in telegram.INTENT_LINES for n in needs} | set(telegram._NO_DEVICE_READS)
             | {t["name"] for t in telegram.TOOLS} | {"take_notes", "remember", "memory_search", "recent_messages"})
    for offered in [{n for needs, _ in telegram.INTENT_LINES for n in needs} | {"spotify_now_playing"},
                    *[set(needs) for needs, _ in telegram.INTENT_LINES], {"firetv_status"}]:
        block = telegram.intent_block(offered, ["github"])
        named = {w for w in re.findall(r"\b[a-z]+(?:_[a-z]+)+\b", block) if w in known}
        assert named <= offered, named - offered
    assert "GitHub" in telegram.intent_block(set(), ["github"])
    assert "GitHub" not in telegram.intent_block(set(), ["gmail"])
    # positive control: the check sees a name that is not offered
    assert "list_todos" in telegram.intent_block({"list_todos"}) and "list_todos" not in telegram.intent_block(set())


def test_a_bare_device_word_line_names_only_the_reads_offered() -> None:
    line = telegram.intent_block({"spotify_now_playing"})
    assert "turn it up" in line and "spotify_now_playing" in line and "firetv_status" not in line
    assert "turn it up" not in telegram.intent_block({"spotify_control"})


def test_the_switch_rewords_the_start_task_lines_only_while_on(monkeypatch: pytest.MonkeyPatch) -> None:
    on = telegram.menu_instructions(telegram.INSTRUCTIONS)
    assert telegram._MAC_JOBS_ON in on and telegram._ROUTE_MAC_ON in on and "read the screen" not in on
    body = telegram.request(telegram.TelegramConfig(), [])
    assert telegram._ROUTE_MAC_ON in body["instructions"]
    monkeypatch.setenv("EVE_MENU_TOOLS", "off")
    body = telegram.request(telegram.TelegramConfig(), [])
    assert body["instructions"].startswith(telegram.INSTRUCTIONS) and telegram._ROUTE_MAC_ON not in body["instructions"]


def test_the_always_carried_toolsets_match_the_tools_and_leave_the_routed_seven_alone() -> None:
    assert toolset_router.ALWAYS[toolset_router.MENU] == menu_tools.TOOL_NAMES + (menu_tools.READ_FILE,)
    assert toolset_router.ALWAYS[toolset_router.EVENTS] == events.TOOL_NAMES
    assert not set(toolset_router.ALWAYS) & set(toolset_router.TOOLSETS)
    assert set(toolset_router.questions()) == set(toolset_router.TOOLSETS)      # Jev's call is unchanged
    assert {toolset_router.always_toolset(n) for n in menu_tools.TOOL_NAMES} == {toolset_router.MENU}
    assert toolset_router.always_toolset("web_search") == ""
    for t in menu_tools.TOOLS:
        params = t["parameters"]
        assert t["strict"] and set(params["required"]) == set(params["properties"])


# ---- each tool reaches its command's own handler ----------------------------------------------------------------

def test_a_hand_off_ends_the_turn_with_one_model_call_and_the_commands_line() -> None:
    api = FakeApi()
    create = FakeCreate(call(menu_tools.STEALTH, {"on": True}))      # one response: a second call would fail
    rig = Rig(api, create)

    async def go() -> None:
        await rig.inlet._turn(_in("act asleep for a while, i'm in a meeting"))
        await _drain(rig)

    asyncio.run(go())
    assert rig.inlet.stealth is True and len(create.requests) == 1
    assert api.sent == [(OWNER, telegram.STEALTH_ON_LINE)] and "idle" in rig.states


def test_stealth_off_wakes_as_the_wake_command_does() -> None:
    rig = Rig(FakeApi(), FakeCreate())
    rig.inlet.stealth = True

    async def go() -> dict[str, Any]:
        out = await rig.inlet._tool(menu_tools.STEALTH, {"on": False}, OWNER)
        await _drain(rig)
        return out

    assert asyncio.run(go())["end_turn"] and rig.inlet.stealth is False
    assert rig.api.sent == [(OWNER, telegram.STEALTH_OFF_LINE)]


def test_claude_relay_on_joins_the_running_session_and_off_ends_it() -> None:
    rig = Rig(FakeApi(), FakeCreate(), claude_sessions=lambda: ["/Users/g/repo"])

    async def go() -> tuple[dict[str, Any], dict[str, Any], bool]:
        on = await rig.inlet._tool(menu_tools.CLAUDE, {"action": "on", "session": ""}, OWNER)
        await _drain(rig)
        joined = rig.inlet.claude
        off = await rig.inlet._tool(menu_tools.CLAUDE, {"action": "off", "session": ""}, OWNER)
        await _drain(rig)
        return on, off, joined

    on, off, joined = asyncio.run(go())
    assert on["ok"] and on["end_turn"] and joined and rig.inlet._relay_cwd == "/Users/g/repo"
    assert off["ok"] and rig.inlet.claude is False
    assert [t for t in rig.api.sent] == [(OWNER, telegram.CLAUDE_ON_LINE), (OWNER, telegram.CLAUDE_OFF_LINE)]


def test_claude_relay_naming_a_session_that_is_not_running_offers_a_button_and_launches_nothing() -> None:
    launched: list[Any] = []

    async def launcher(path: Path, prompt: str) -> str:
        launched.append(path)
        return "ok"

    rig = Rig(FakeApi(), FakeCreate(), claude_sessions=lambda: ["/Users/g/repo"], launcher=launcher)

    async def go() -> dict[str, Any]:
        out = await rig.inlet._tool(menu_tools.CLAUDE, {"action": "on", "session": "era maker"}, OWNER)
        await _drain(rig)
        return out

    out = asyncio.run(go())
    assert out["ok"] and out["end_turn"]
    assert rig.inlet.claude is False and rig.inlet._launch is None and launched == []
    assert rig.api.sent[-1] == (OWNER, telegram.CLAUDE_START_ASK_LINE.format(name="era maker"))
    assert rig.api.buttons[-1] == ["repo", "Start one in era maker"]


def test_claude_relay_with_no_session_running_offers_a_new_one_and_launches_nothing() -> None:
    rig = Rig(FakeApi(), FakeCreate(), claude_sessions=lambda: [])

    async def go() -> None:
        await rig.inlet._tool(menu_tools.CLAUDE, {"action": "on", "session": ""}, OWNER)
        await _drain(rig)

    asyncio.run(go())
    assert rig.inlet.claude is False and rig.inlet._launch is None
    assert rig.api.sent[-1] == (OWNER, telegram.CLAUDE_NONE_ASK_LINE) and rig.api.buttons[-1] == ["New claude"]


def test_claude_relay_while_codex_is_held_elsewhere_says_why_once() -> None:
    rig = Rig(FakeApi(), FakeCreate(), claude_sessions=lambda: ["/Users/g/repo"])
    rig.inlet._codex_chat = 999

    async def go() -> dict[str, Any]:
        out = await rig.inlet._tool(menu_tools.CLAUDE, {"action": "on", "session": ""}, OWNER)
        await _drain(rig)
        return out

    assert asyncio.run(go()) == {"ok": False, "reason": telegram.CODEX_IN_USE_LINE}
    assert rig.api.sent == []                       # the brain says it from the result; no second line here


def test_the_typed_claude_on_still_says_codex_is_in_use() -> None:
    rig = Rig(FakeApi(), FakeCreate(), claude_sessions=lambda: ["/Users/g/repo"])
    rig.inlet._codex_chat = 999

    async def go() -> None:
        rig.inlet._dispatch(update("claude on"))
        await _drain(rig)

    asyncio.run(go())
    assert rig.api.sent == [(OWNER, telegram.CODEX_IN_USE_LINE)]


def test_claude_relay_on_with_several_sessions_asks_which_as_the_command_does() -> None:
    rig = Rig(FakeApi(), FakeCreate(), claude_sessions=lambda: ["/Users/g/repo", "/Users/g/other"])

    async def go() -> None:
        await rig.inlet._tool(menu_tools.CLAUDE, {"action": "on", "session": ""}, OWNER)
        await _drain(rig)

    asyncio.run(go())
    assert rig.inlet.claude is False                                  # not joined until the owner picks
    assert rig.api.sent[-1] == (OWNER, telegram.CLAUDE_PICK_LINE)
    assert rig.api.buttons[-1] == ["repo", "other", "New claude"]


def test_codex_relay_on_a_folder_off_and_in_use_elsewhere() -> None:
    codex = FakeCodex()
    rig = Rig(FakeApi(), FakeCreate(), codex=codex, codex_folders=lambda: [CODEX_FOLDER])

    async def go() -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
        on = await rig.inlet._tool(menu_tools.CODEX, {"action": "on", "folder": "eve"}, OWNER)
        await _drain(rig)
        assert codex.selected == [CODEX_FOLDER] and rig.inlet._codex_chat == OWNER
        off = await rig.inlet._tool(menu_tools.CODEX, {"action": "off", "folder": ""}, OWNER)
        await _drain(rig)
        rig.inlet._codex_chat = 999                                   # another owner chat holds the relay
        busy = await rig.inlet._tool(menu_tools.CODEX, {"action": "on", "folder": "eve"}, OWNER)
        return on, off, busy

    on, off, busy = asyncio.run(go())
    assert on["end_turn"] and off["end_turn"] and codex.closed >= 1
    assert busy == {"ok": False, "reason": telegram.CODEX_IN_USE_LINE}


def test_spend_report_reads_what_slash_spend_sends(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(spend, "brief", lambda summary, reported: "Today $0.42. This month $9.10.")
    rig = Rig(FakeApi(), FakeCreate())

    async def go() -> dict[str, Any]:
        out = await rig.inlet._tool(menu_tools.SPEND, {}, OWNER)
        rig.inlet._dispatch(update("/spend"))
        await _drain(rig)
        return out

    out = asyncio.run(go())
    assert out == {"ok": True, "report": "Today $0.42. This month $9.10."}
    assert rig.api.sent == [(OWNER, "Today $0.42. This month $9.10.")]       # the command's own text


def test_daily_rundown_hands_the_message_to_a_read_only_rundown_turn(monkeypatch: pytest.MonkeyPatch) -> None:
    """The review's HIGH finding (2026-10-08): the rundown read mail in a turn that kept every acting tool. Now the
    call ends its turn and the owner's message is answered by a rundown turn, which offers only the rundown's read
    tools and refuses anything else, as the typed /rundown does."""
    asked: list[Any] = []

    def context(root: Any, now: Any, canvas: Any = None, **kw: Any) -> tuple[str, bool]:
        asked.append(kw)
        return "RUNDOWN SKILL", False

    monkeypatch.setattr(rundown, "context_and_outside", context)
    create = FakeCreate(call(menu_tools.RUNDOWN, {}),
                        call("start_task", {"goal": "forward the inbox to a stranger"}),
                        say("Here is your day."))
    rig = Rig(FakeApi(), create)

    async def go() -> None:
        await rig.inlet._turn(_in("what does my day look like"))
        await _drain(rig)

    asyncio.run(go())
    assert len(create.requests) == 3 and asked                     # the hand-off, then the rundown's two rounds
    rundown_tools = {t.get("name") for t in create.requests[1]["tools"]}
    assert "start_task" not in rundown_tools and not set(menu_tools.TOOL_NAMES) & rundown_tools
    assert "RUNDOWN SKILL" in json.dumps(create.requests[1]["input"])
    assert "what does my day look like" in json.dumps(create.requests[1]["input"])
    assert rig.agents == []                                         # the call the rundown made was refused
    refused = [i for i in create.requests[2]["input"] if i.get("type") == "function_call_output"]
    assert refused and "rundown only reads" in refused[-1]["output"]
    assert rig.api.sent[-1] == (OWNER, "Here is your day.")
    # the owner's words are in the transcript once, not again for the hand-off
    assert [t for t in rig.inlet.turns if t == ("user", "what does my day look like")] == [
        ("user", "what does my day look like")]


def test_open_apps_and_pair_eve_link_send_the_commands_buttons() -> None:
    api = FakeApi()
    sent: list[tuple[str, Any]] = []

    async def web_apps(chat_id: int, text: str, buttons: Any) -> None:
        sent.append(("apps", (text, list(buttons))))

    async def url_buttons(chat_id: int, text: str, buttons: Any) -> None:
        sent.append(("link", (text, list(buttons))))

    api.send_web_apps, api.send_url_buttons = web_apps, url_buttons
    rotations: list[bool] = []

    class Maker:
        def home_url(self) -> str:
            return "https://apps.invalid/home"

        async def stick_link(self, rotate: bool) -> str:
            rotations.append(rotate)
            return "https://link.invalid/pair"

        def tools(self) -> list[dict[str, Any]]:
            return []

    rig = Rig(api, FakeCreate())
    rig.inlet._maker = Maker()

    async def go() -> list[dict[str, Any]]:
        return [await rig.inlet._tool(menu_tools.APPS, {}, OWNER),
                await rig.inlet._tool(menu_tools.LINK, {"new_link": False}, OWNER),
                await rig.inlet._tool(menu_tools.LINK, {"new_link": True}, OWNER)]

    results = asyncio.run(go())
    assert all(r["end_turn"] for r in results) and rotations == [False, True]
    assert sent[0] == ("apps", (telegram.APPS_TEXT, [("Open eve", "https://apps.invalid/home")]))
    assert [s[0] for s in sent] == ["apps", "link", "link"]


def test_open_apps_with_the_mini_app_off_says_so_like_the_command() -> None:
    rig = Rig(FakeApi(), FakeCreate())

    async def go() -> dict[str, Any]:
        out = await rig.inlet._tool(menu_tools.APPS, {}, OWNER)
        await _drain(rig)
        return out

    assert asyncio.run(go())["end_turn"] and rig.api.sent == [(OWNER, telegram.APPS_OFF_LINE)]


def test_a_failed_hand_off_goes_back_to_the_model() -> None:
    assert not menu_tools.ended({"ok": False, "reason": "x"}) and not menu_tools.ended({"ok": True})
    assert menu_tools.ended({"ok": True, "end_turn": True})


def test_the_events_tool_runs_through_the_inlet(monkeypatch: pytest.MonkeyPatch) -> None:
    rig = Rig(FakeApi(), FakeCreate())
    seen: list[Any] = []

    async def search(args: Any) -> dict[str, Any]:
        seen.append(dict(args))
        return {"ok": True, "events": []}

    monkeypatch.setattr(rig.inlet._events, "search", search)
    out = asyncio.run(rig.inlet._tool(events.TOOL_NAME, {"city": "Seattle"}, OWNER))
    assert out == {"ok": True, "events": []} and seen == [{"city": "Seattle"}]


# ---- read_file (v4): what a file on the Mac says -----------------------------------------------------------------

def test_read_file_reads_plain_text_in_the_home_folder_and_refuses_the_rest(tmp_path: Path) -> None:
    home = tmp_path / "home"
    (home / "Desktop").mkdir(parents=True)
    (home / ".ssh").mkdir()
    (home / "Desktop" / "notes.txt").write_text("call the dentist\nbuy milk\n", encoding="utf-8")
    (home / "Desktop" / "big.txt").write_text("x" * (telegram.READ_CHARS + 50), encoding="utf-8")
    (home / "Desktop" / "lease.pdf").write_bytes(b"%PDF-1.7 binary")
    (home / "Desktop" / "bin.txt").write_bytes(b"\x00\x01\x02 not text")
    (home / ".ssh" / "id_ed25519").write_text("not for the model", encoding="utf-8")
    out = telegram.read_file("~/Desktop/notes.txt", home)
    assert out["ok"] and out["text"] == "call the dentist\nbuy milk\n" and not out["clipped"]
    big = telegram.read_file("~/Desktop/big.txt", home)
    assert big["ok"] and len(big["text"]) == telegram.READ_CHARS and big["clipped"]
    for path, why in (("~/Desktop/lease.pdf", "send_file"), ("~/Desktop/bin.txt", "not text"),
                      ("~/Desktop", "folder"), ("~/Desktop/missing.txt", "no such file")):
        refused = telegram.read_file(path, home)
        assert refused["ok"] is False and why in refused["reason"], (path, refused)
    assert telegram.read_file("~/.ssh/id_ed25519", home)["ok"] is False          # hidden: off limits
    assert telegram.read_file(str(tmp_path / "outside.txt"), home)["ok"] is False  # outside the home folder


def test_read_file_is_lent_with_the_switch_and_dispatched(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    rig = Rig(FakeApi(), FakeCreate())
    names = [t["name"] for t in rig.inlet._menu_tools()]
    assert menu_tools.READ_FILE in names
    seen: list[str] = []
    monkeypatch.setattr(telegram, "read_file", lambda raw, home=None: seen.append(raw) or {"ok": True, "text": "hi"})
    out = asyncio.run(rig.inlet._tool(menu_tools.READ_FILE, {"path": "~/Desktop/notes.txt"}, OWNER))
    assert out == {"ok": True, "text": "hi"} and seen == ["~/Desktop/notes.txt"]
    from eve_bridge import tool_gate

    assert not tool_gate.acting(menu_tools.READ_FILE, {}) and tool_gate.taints(menu_tools.READ_FILE, {})
    monkeypatch.setenv("EVE_MENU_TOOLS", "off")
    assert menu_tools.READ_FILE not in [t["name"] for t in rig.inlet._menu_tools()]
