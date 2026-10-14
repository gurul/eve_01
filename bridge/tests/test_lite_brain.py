"""eve lite's brain as the model reads it (EVE_PROFILE=lite; docs/plans/eve-lite.md M2, WP-3).

A lite turn offers exactly eve lite's tools, whatever else is lent; every instruction it is sent names only tools that
turn offers and no app (the owner's rule for prompts, AGENTS rules 6 and 7); the app block names eve's web app; the
apps block is built from the Composio tools the bridge lists; and a push-to-talk turn reads the web app's call note.
The name checker has its positive controls (an injected Mac tool name and an injected app name are caught), and
full eve's request is the owner golden's (tools/owner_golden.py). No network."""

from __future__ import annotations

import asyncio
import re
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Iterable

import pytest
from test_telegram import call, say
from test_telegram_lite import COMPOSIO_NAMES, LITE_ENV, FakeConnect, Spy, app_turns, drain, lite_rig

from eve_bridge import (
    agentcard,
    america,
    app_registry,
    apps_maker,
    apps_mcp,
    canvas,
    chief,
    events,
    link_chat,
    meet,
    memory,
    menu_tools,
    opentable,
    phone_location,
    profile,
    science_search,
    second_brain,
    share,
    telegram,
    voice_agent,
    watch,
    websearch,
)
from eve_bridge.telegram import (
    APP_STYLE_LITE,
    CALL_NOTE,
    CALL_NOTE_LITE,
    INSTRUCTIONS_LITE,
    LITE_APP_SWAPS,
    apps_block_lite,
    lite_app_block,
    offered_names,
)

# Every tool name eve offers somewhere, in any door: the names a lite prompt may write only when the turn offers them.
UNIVERSE: frozenset[str] = frozenset(
    set(telegram.TOOL_NAMES) | set(menu_tools.TOOL_NAMES) | {str(t["name"]) for t in menu_tools.FILE_TOOLS}
    | {str(t["name"]) for t in memory.VOICE_TOOLS} | set(watch.TOOL_NAMES) | set(science_search.TOOL_NAMES)
    | set(second_brain.SECOND_BRAIN_TOOL_NAMES) | {telegram.RECENT_MESSAGES_TOOL["name"], websearch.TOOL_NAME}
    | set(COMPOSIO_NAMES) | {apps_mcp.OPEN_TOOL} | set(meet.TOOL_NAMES) | set(chief.TOOL_NAMES)
    | set(canvas.TOOL_NAMES) | set(america.TOOL_NAMES) | set(opentable.TOOL_NAMES) | set(share.TOOL_NAMES)
    | set(agentcard.TOOL_NAMES) | set(phone_location.TOOL_NAMES) | set(events.TOOL_NAMES)
    | set(apps_maker.MAKER_TOOL_NAMES) | {n for app in app_registry.with_tools() for n in app.mod().TOOL_NAMES}
    | {str(t["name"]) for t in voice_agent.TOOLS if t.get("name")})
# Names that are also plain English words ("find my keys", "look", "remember that", "a lesson plan"): the checker
# cannot tell the tool from the word, so it leaves these out and reads every other name.
ENGLISH = frozenset({"find", "look", "remember", "lesson"})
CHECKED = frozenset(n for n in UNIVERSE if n not in ENGLISH)
# App, door and device names a lite prompt never writes: Composio Connect holds whichever apps the owner connected.
APP_NAMES = ("Gmail", "Google Calendar", "Google Drive", "Google Docs", "Google Sheets", "Google Maps", "Slack",
             "Notion", "Linear", "GitHub", "Outlook", "Spotify", "Fire TV", "Telegram", "iPhone", "Mini App",
             "Eve Link", "Canvas", "OpenTable", "Ticketmaster", "Obsidian", "Codex", "Claude Code", "OBSBOT",
             "StackChan", "Pika", "TinyFish", "Firecrawl", "AgentCard")
# eve lite's own tools in a chat turn (profile.LITE_TOOL_NAMES less the voice's recent_conversation), with Connect's
LITE_CHAT_TOOLS = (profile.LITE_TOOL_NAMES - {"recent_conversation"}) | set(COMPOSIO_NAMES)


def names_written(text: str) -> set[str]:
    """Every tool name of eve's the text writes, as a whole word."""
    return {n for n in CHECKED if re.search(r"(?<![A-Za-z0-9_])" + re.escape(n) + r"(?![A-Za-z0-9_])", text)}


def problems(text: str, offered: Iterable[str]) -> list[str]:
    """What breaks the owner's rule in ``text`` for a turn offering ``offered``: a tool name it does not offer, or
    any app name."""
    off = sorted(names_written(text) - set(offered))
    apps = [a for a in APP_NAMES if re.search(r"\b" + re.escape(a) + r"\b", text)]
    return [f"tool {n}" for n in off] + [f"app {a}" for a in apps]


@pytest.fixture
def lite(monkeypatch: pytest.MonkeyPatch) -> None:
    for key, value in LITE_ENV.items():
        if key != "OPENROUTER_API_KEY":
            monkeypatch.setenv(key, value)


@pytest.fixture
def full_profile(monkeypatch: pytest.MonkeyPatch) -> None:
    """Full eve (EVE_PROFILE unset) with lite's routers off, so a control turn is shaped as the lite one is."""
    for key, value in LITE_ENV.items():
        if key not in ("OPENROUTER_API_KEY", "EVE_PROFILE"):
            monkeypatch.setenv(key, value)
    monkeypatch.delenv("EVE_PROFILE", raising=False)


def first_request(rig: Any, text: str = "hello there, what can you do") -> dict[str, Any]:
    app_turns(rig, text)
    return rig.create.requests[0]


def _docx(*paragraphs: str) -> bytes:
    """A smallest real .docx (a zip with word/document.xml): what the app uploads for a Word file."""
    import io
    import zipfile

    body = "".join('<w:p><w:r><w:t xml:space="preserve">' + p + "</w:t></w:r></w:p>" for p in paragraphs)
    doc = ('<?xml version="1.0"?><w:document '
           'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">'
           f"<w:body>{body}</w:body></w:document>")
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w") as zf:
        zf.writestr("word/document.xml", doc)
    return out.getvalue()


def user_parts(request: dict[str, Any]) -> str:
    """Every input_text and the shape of the last user turn's content, flattened for a substring check."""
    users = [i for i in request["input"] if isinstance(i, dict) and i.get("role") == "user"]
    return " ".join(str(p.get("text", "")) for p in users[-1]["content"] if isinstance(p, dict))


# ---- the offered set ------------------------------------------------------------------------------------------

def test_a_lite_turn_offers_exactly_eve_lites_tools(lite: None, tmp_path: Path,
                                                    monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVE_SCIENCE_SEARCH", "1")
    rig = lite_rig(tmp_path, say("Hi!"), apps=FakeConnect())
    names = offered_names(first_request(rig)["tools"])
    assert len(names) == len(set(names))
    assert set(names) == LITE_CHAT_TOOLS
    # in the order full eve lends them: its own, memory, the chat log, the web, the watcher's slot, the apps
    assert names[:2] == ["remember", "think_hard"] and names[-len(COMPOSIO_NAMES):] == list(COMPOSIO_NAMES)


def test_whatever_else_is_lent_a_lite_turn_offers_only_eve_lites_tools(lite: None, tmp_path: Path,
                                                                       monkeypatch: pytest.MonkeyPatch) -> None:
    """The robot's switch on, the menu's tools on, a vault lent, the science switch on, an app maker's tools: the
    filter (profile.filter_tools) leaves eve lite's, and the instructions still name none of the others."""
    monkeypatch.setenv("EVE_ROBOT", "1")
    monkeypatch.setenv("EVE_SCIENCE_SEARCH", "1")
    vault = second_brain.VaultConfig(enabled=True, root=tmp_path / "vault")
    rig = lite_rig(tmp_path, say("Hi!"), apps=FakeConnect(), vault=vault)
    rig.inlet._maker = SimpleNamespace(tools=lambda: [{"type": "function", "name": "make_app", "parameters": {}}])
    request = first_request(rig)
    names = offered_names(request["tools"])
    assert set(names) == LITE_CHAT_TOOLS
    assert problems(request["instructions"], names) == []
    # control: full eve lends them all
    monkeypatch.delenv("EVE_PROFILE")
    full = offered_names(telegram.own_tools())
    assert {"start_task", "screenshot", "take_photo", "look", "move_head"} <= set(full)


def test_own_tools_and_tools_for_filter_in_lite_and_not_in_full_eve(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVE_ROBOT", "1")
    monkeypatch.delenv("EVE_PROFILE", raising=False)
    full_own = telegram.own_tools()
    assert full_own is telegram.TOOLS                                    # full eve: the very list, as before
    monkeypatch.setenv("EVE_PROFILE", "lite")
    assert offered_names(telegram.own_tools()) == ["remember", "think_hard"]
    cfg = telegram.configured(LITE_ENV)
    lent = telegram.tools_for(cfg, memory_tools=[{"type": "function", "name": n} for n in
                                                 ("memory_search", "capture_note")],
                              extra=[{"type": "function", "name": "COMPOSIO_SEARCH_TOOLS"},
                                     {"type": "function", "name": "gmail__send_message"}],
                              vault=True)
    assert offered_names(lent) == ["remember", "think_hard", "memory_search", websearch.TOOL_NAME,
                                   "COMPOSIO_SEARCH_TOOLS"]


# ---- every instruction names only offered tools and no app ---------------------------------------------------

def test_the_checker_catches_a_mac_tool_and_an_app_name() -> None:
    """Positive controls: an injected start_task, screenshot or Gmail is caught; lite's own text is clean."""
    offered = ["remember", "think_hard"]
    assert problems(INSTRUCTIONS_LITE, offered) == []
    assert names_written(INSTRUCTIONS_LITE) == {"think_hard"}            # remember is a word too: not counted
    assert problems(INSTRUCTIONS_LITE + "\nWhen asked, call start_task.", offered) == ["tool start_task"]
    assert problems(INSTRUCTIONS_LITE + " Use screenshot.", offered) == ["tool screenshot"]
    assert problems("Your Gmail is connected.", offered) == ["app Gmail"]
    assert problems("start_tasks and my_start_task are other words", offered) == []


@pytest.mark.parametrize("shape", ["everything", "no_apps", "apps_not_started", "search_and_execute_only",
                                   "no_memory_no_chat_log", "no_watcher"])
def test_every_lite_app_turns_instructions_name_only_what_it_offers(shape: str, lite: None, tmp_path: Path,
                                                                    monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVE_SCIENCE_SEARCH", "1")
    apps: Any = {"everything": FakeConnect(), "no_apps": None, "apps_not_started": FakeConnect(started=False),
                 "search_and_execute_only": FakeConnect(("COMPOSIO_SEARCH_TOOLS", "COMPOSIO_MULTI_EXECUTE_TOOL")),
                 "no_memory_no_chat_log": FakeConnect(), "no_watcher": FakeConnect()}[shape]
    rig = lite_rig(tmp_path, say("Hi!"), apps=apps, memory=shape != "no_memory_no_chat_log",
                   chat=shape != "no_memory_no_chat_log", watcher=shape != "no_watcher")
    request = first_request(rig)
    offered = offered_names(request["tools"])
    text = request["instructions"]
    assert problems(text, offered) == []
    assert "eve's web app" in text and APP_STYLE_LITE in text
    assert "computer agent" not in text and "the Mac's screen" not in text      # no task, no agent, in lite
    if shape == "everything":
        assert telegram._RECALL_NOTES_LITE in text and "recent_messages" in text
    if shape == "search_and_execute_only":
        assert "COMPOSIO_MANAGE_CONNECTIONS" not in text and "COMPOSIO_SEARCH_TOOLS" in text
    if shape in ("no_apps", "apps_not_started"):
        assert "COMPOSIO" not in text.replace("through the COMPOSIO tools.", "")
    # the turn's own note (the clock, the brief) names nothing either
    note = [i for i in request["input"] if i.get("role") == "developer"]
    assert all(problems(str(i), offered) == [] for i in note)


def test_a_push_to_talk_turn_reads_the_web_apps_call_note(lite: None, tmp_path: Path,
                                                          monkeypatch: pytest.MonkeyPatch) -> None:
    rig = lite_rig(tmp_path, say("It's sunny."), apps=FakeConnect())
    heard: list[str] = []

    async def go() -> None:
        rig.inlet.hear("what's the weather like outside", call=heard.append)
        await drain(rig)

    asyncio.run(go())
    request = rig.create.requests[0]
    note = " ".join(str(i) for i in request["input"] if i.get("role") == "developer")
    assert CALL_NOTE_LITE in note and CALL_NOTE not in note
    assert telegram._TEXT_STYLE_LITE in request["instructions"]           # a call turn keeps the texting style
    assert problems(request["instructions"], offered_names(request["tools"])) == []
    assert problems(CALL_NOTE_LITE, []) == [] and "shown in the chat" in CALL_NOTE_LITE
    assert heard == ["It's sunny."]
    # the call's warm-up reads it too
    rig.create.responses.append(say("ok"))
    asyncio.run(rig.inlet._warm())
    warm = " ".join(str(i) for i in rig.create.requests[-1]["input"] if i.get("role") == "developer")
    assert CALL_NOTE_LITE in warm and CALL_NOTE not in warm


PROGRAM = ("Here's how they compare.\n```openui-lang\nroot = Card([table])\n"
           "table = Table([Col(\"Drink\", [\"Tea\", \"Coffee\"]), Col(\"Caffeine (mg)\", [47, 95], \"number\")])\n```")


def card_then_call(rig: Any) -> tuple[list[str], dict[str, Any]]:
    """An app turn whose reply is a card written after a web search (so it is kept as outside words), then a call
    turn: what the call heard, and the call turn's request."""
    heard: list[str] = []
    app_turns(rig, "tea or coffee, which has more caffeine")

    async def go() -> None:
        rig.inlet.hear("so which one should I drink tonight", call=heard.append)
        await drain(rig)

    asyncio.run(go())
    return heard, rig.create.requests[-1]


def test_a_call_turn_reads_an_earlier_card_in_lite_words_and_its_taint_still_carries(
        lite: None, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """A call cannot draw a card, so its history reads the card as a placeholder (link_chat.history_text). eve lite's
    placeholder points at its chat, not the phone app; and the card, written after a web search, still taints the
    call turn that shows it (the taint marks are made from the same words)."""
    monkeypatch.setitem(telegram.TOOL_HANDLERS, "web_search", Spy({"ok": True, "answer": "Coffee: 95 mg; tea: 47."}))
    rig = lite_rig(tmp_path, call("web_search", {"query": "caffeine tea vs coffee"}), say(PROGRAM),
                   say("Tea, it has half the caffeine."), memory=False)     # no transcript: the marks alone carry
    heard, request = card_then_call(rig)
    history = " ".join(str(i) for i in request["input"] if i.get("role") == "assistant")
    assert telegram.OPENUI_PLACEHOLDER_LITE in history and "Eve Link" not in history and "openui-lang" not in history
    assert rig.inlet._taint == telegram.tool_gate.CARRIED                # the card's outside words carried over
    assert heard == ["Tea, it has half the caffeine."]


def test_full_eves_call_turn_reads_the_card_as_it_always_did(full_profile: None, tmp_path: Path,
                                                             monkeypatch: pytest.MonkeyPatch) -> None:
    """Control: with EVE_PROFILE unset the placeholder is link_chat's own, and the taint carries as before."""
    monkeypatch.setitem(telegram.TOOL_HANDLERS, "web_search", Spy({"ok": True, "answer": "Coffee: 95 mg; tea: 47."}))
    cfg = telegram.TelegramConfig(enabled=True, app_only=True, owner_ids=frozenset({4242}))
    rig = lite_rig(tmp_path, call("web_search", {"query": "caffeine tea vs coffee"}), say(PROGRAM),
                   say("Tea, it has half the caffeine."), config=cfg, memory=False)
    _, request = card_then_call(rig)
    history = " ".join(str(i) for i in request["input"] if i.get("role") == "assistant")
    assert link_chat.OPENUI_PLACEHOLDER in history and telegram.OPENUI_PLACEHOLDER_LITE not in history
    assert rig.inlet._taint == telegram.tool_gate.CARRIED


def test_full_eves_call_note_and_style_are_its_own(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """Control: with EVE_PROFILE unset a call turn reads CALL_NOTE and an app turn APP_STYLE (the golden holds the
    bytes)."""
    monkeypatch.delenv("EVE_PROFILE", raising=False)
    cfg = telegram.TelegramConfig(enabled=True, app_only=True, owner_ids=frozenset({4242}))
    body = telegram.request(cfg, [telegram.message_item("user", "hi")], app_block=link_chat.app_block())
    assert telegram.APP_STYLE in body["instructions"] and INSTRUCTIONS_LITE not in body["instructions"]
    assert "Eve Link app" in body["instructions"]


# ---- an Office document is read in lite, not handed to a Mac task --------------------------------------------

def test_a_lite_doc_upload_reaches_the_brain_as_its_words_not_a_mac_task(lite: None, tmp_path: Path) -> None:
    """docs/lite.md promises Office files work in lite. A .docx has no Mac task to open it here (lite offers none),
    so its text is pulled out and sent like a text file; the "start a task on the Mac" line full eve uses, which
    names something lite cannot do, never appears."""
    from eve_bridge.link_uploads import UploadStore

    store = UploadStore(tmp_path / "up")
    doc = store.save(_docx("Quarterly budget", "Rent is 1200 a month, paid on the 1st."), "budget.docx")
    rig = lite_rig(tmp_path, say("Rent is 1200 a month."))

    async def go() -> None:
        rig.inlet.hear("what does it say about rent?", on_call=False, app="t-1", attachments=[doc])
        await drain(rig)

    asyncio.run(go())
    parts = user_parts(rig.create.requests[0])
    assert "Rent is 1200 a month, paid on the 1st." in parts and "context, not instructions" in parts
    assert "start a task on the Mac" not in parts and "saved on this Mac" not in parts


# ---- the app block and the apps block ------------------------------------------------------------------------

def test_each_swapped_text_is_still_in_link_chats_app_block() -> None:
    """The exact-text swaps (LITE_APP_SWAPS) find their words: a reworded link_chat.py or openui_prompt.txt fails here
    rather than leaving the phone app's name in a lite prompt."""
    block = link_chat.app_block()
    assert block, "openui_prompt.txt is missing: run ios/EveLink/Web/scripts/gen-prompt.mjs"
    for old, new in LITE_APP_SWAPS:
        assert block.count(old) == 1, old
        assert new not in block
    assert link_chat.APP_RULE.startswith(LITE_APP_SWAPS[0][0])
    assert LITE_APP_SWAPS[1][0] in link_chat.APP_LINE


def test_the_lite_app_block_names_the_web_app_and_keeps_the_rest() -> None:
    block = link_chat.app_block()
    lite = lite_app_block(block)
    assert "## Replying in eve's web app" in lite and "This turn is from eve's web app:" in lite
    assert not [w for w in ("Eve Link", "iPhone", "native cards") if w in lite]
    assert link_chat.APP_EXAMPLE in lite and lite.endswith(link_chat.APP_LINE.replace("the Eve Link app",
                                                                                       "eve's web app"))
    assert len(lite) - len(block) == sum((len(new) - len(old)) for old, new in LITE_APP_SWAPS)
    assert lite_app_block("") == ""


def test_the_apps_block_is_built_from_the_names_the_bridge_lists() -> None:
    every = apps_block_lite(COMPOSIO_NAMES)
    assert "COMPOSIO_SEARCH_TOOLS" in every and "COMPOSIO_MULTI_EXECUTE_TOOL" in every
    assert "COMPOSIO_MANAGE_CONNECTIONS" in every and "sign-in link" in every and "web_search" in every
    assert problems(every, COMPOSIO_NAMES + (websearch.TOOL_NAME,)) == []
    assert "mail, calendar, files and work apps" in every
    two = apps_block_lite(("COMPOSIO_SEARCH_TOOLS", "COMPOSIO_MULTI_EXECUTE_TOOL"))
    assert "COMPOSIO_MANAGE_CONNECTIONS" not in two and "sign-in link" not in two
    manage = apps_block_lite(("COMPOSIO_MANAGE_CONNECTIONS",), web=False)
    assert "COMPOSIO_SEARCH_TOOLS" not in manage and "web_search" not in manage and "public web" in manage
    assert problems(manage, ("COMPOSIO_MANAGE_CONNECTIONS",)) == []
    # control: full eve's block names its apps and is not lite's
    assert "Linear" in telegram.APPS_BLOCK_REST and "Linear" not in every


def test_lite_instructions_say_what_eve_lite_is_in_a_positive_set() -> None:
    assert telegram._TEXT_STYLE_LITE in INSTRUCTIONS_LITE and "no lists" in telegram._TEXT_STYLE_LITE
    swapped = INSTRUCTIONS_LITE.replace(telegram._TEXT_STYLE_LITE, APP_STYLE_LITE)
    assert "no lists" not in swapped and "web app section below" in swapped
    flat = " ".join(INSTRUCTIONS_LITE.split())                              # as the model reads it, lines aside
    for part in ("chat", "remember", "search the web", "keep watch", "think hard", "apps they connect"):
        assert part in flat
    assert "eve lite does not do that" in flat                              # requests outside it get a plain no
    assert "none of your tools reaches them" in flat and "eve's Settings, under Apps" in flat
    assert "Keep API keys out of this chat" in flat                         # the Ori eval's safety rule stays
