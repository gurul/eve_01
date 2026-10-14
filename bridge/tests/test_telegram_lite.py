"""telegram.py in eve lite (EVE_PROFILE=lite; docs/plans/eve-lite.md M2, WP-3): the brain of eve's web app.

It is configured without Telegram (app-only, no token, one local chat). A code word of a feature eve lite does not
have gets LITE_NOT_HERE and nothing else happens; a call to a tool eve lite does not have is refused before any
handler runs; an OpenRouter 402 is the no-credit line; a rundown's words are an ordinary turn; and Composio's gate
is eve lite's (decide_lite). Every check has its control in full eve (EVE_PROFILE unset), where the same words, calls
and failures do what they always did. The rig is test_telegram.py's, lent what lite.py lends: no agent, no launcher,
no Mac. No network.
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any, Callable, Optional

import httpx
import openai
import pytest
from test_telegram import OWNER, FakeApi, FakeCreate, FakeMemory, Rig, call, say, settle

from eve_bridge import chat_log, composio_tools, profile, telegram, tool_gate, watch
from eve_bridge.telegram import (
    FAILED_LINE,
    LITE_KEY_REJECTED_LINE,
    LITE_NO_CREDIT_LINE,
    LITE_NOT_HERE,
    LITE_TOOL_OFF,
    NOTHING_TO_STOP_LINE,
    STEALTH_ON_LINE,
    MutedBotApi,
    TelegramConfig,
    configured,
    key_rejected,
    no_credit,
)

PLACEHOLDER = "placeholder-not-a-key"
TOKEN = "123456:test-token-not-real"
FIXTURES = Path(__file__).parent / "fixtures" / "composio_connect"
COMPOSIO_NAMES = ("COMPOSIO_SEARCH_TOOLS", "COMPOSIO_GET_TOOL_SCHEMAS", "COMPOSIO_MULTI_EXECUTE_TOOL",
                  "COMPOSIO_MANAGE_CONNECTIONS", "COMPOSIO_WAIT_FOR_CONNECTIONS", "COMPOSIO_REMOTE_WORKBENCH",
                  "COMPOSIO_REMOTE_BASH_TOOL")
# eve lite's environment, for the parts these tests build (profile.LITE_FORCED's switches; the key a placeholder)
LITE_ENV = {"EVE_PROFILE": "lite", "OPENROUTER_API_KEY": PLACEHOLDER, "EVE_TELEGRAM": "app", "EVE_MEMORY": "1",
            "EVE_TOOLSET_ROUTER": "off", "EVE_GRADE_ROUTER": "off", "EVE_SEARCH_ROUTER": "off",
            "EVE_MENU_TOOLS": "off", "EVE_WATCH_TLS": "0", "EVE_WATCH_BROWSER": "0", "EVE_WATCH_HOSTED": "0",
            "EVE_COMPOSIO_POLICY": "gmail=auto", "EVE_APPS": "0", "EVE_COMPOSIO": "0", "EVE_CHIEF": "off",
            "EVE_ROBOT": "0", "EVE_COMPUTER_CONTROL": "0"}


# ---- the lite rig ---------------------------------------------------------------------------------------------

def connect_tools(names: tuple[str, ...] = COMPOSIO_NAMES) -> list[dict[str, Any]]:
    """Composio Connect's meta tools as Responses function tools, from spike S1's tool objects (as served)."""
    out = []
    for name in names:
        obj = json.loads((FIXTURES / f"{name}.json").read_text(encoding="utf-8"))
        schema = {k: v for k, v in obj["inputSchema"].items() if k != "$schema"}
        out.append({"type": "function", "name": name, "description": obj["description"].strip()[:400],
                    "parameters": schema, "strict": False})
    return out


class FakeConnect:
    """composio_connect.ConnectBridge's duck type (the one the brain reads: started, names, tools, execute,
    accounts_note): Composio Connect's seven meta tools, every call recorded, a canned result."""

    def __init__(self, names: tuple[str, ...] = COMPOSIO_NAMES, started: bool = True) -> None:
        self.started = started
        self._tools = connect_tools(names)
        self.names = frozenset(names)
        self.executed: list[tuple[str, dict[str, Any]]] = []

    def tools(self) -> list[dict[str, Any]]:
        return [dict(t) for t in self._tools]

    def execute(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        self.executed.append((name, args))
        if name == composio_tools.MANAGE_CONNECTIONS:
            return {"successful": True, "data": {"results": {"gmail": {
                "status": "initiated", "redirect_url": "https://connect.composio.dev/link/ln_test"}}}}
        return {"successful": True, "data": {"results": [{"successful": True}]}}

    def accounts_note(self) -> str:
        return ""


@pytest.fixture
def lite(monkeypatch: pytest.MonkeyPatch) -> dict[str, str]:
    """The process is eve lite (profile.is_lite reads EVE_PROFILE), with lite's switches for what a turn reads."""
    for key, value in LITE_ENV.items():
        if key != "OPENROUTER_API_KEY":                      # never in os.environ: the conftest keeps it out
            monkeypatch.setenv(key, value)
    return LITE_ENV


@pytest.fixture
def full(monkeypatch: pytest.MonkeyPatch) -> None:
    """Full eve: EVE_PROFILE unset, as on the owner's Mac."""
    monkeypatch.delenv("EVE_PROFILE", raising=False)


class Spy:
    """Records each call; returns ``result``."""

    def __init__(self, result: Any = None) -> None:
        self.calls: list[tuple[Any, ...]] = []
        self.result = {"ok": True} if result is None else result

    async def __call__(self, *args: Any, **kwargs: Any) -> Any:
        self.calls.append(args)
        return self.result


def lite_rig(tmp_path: Path, *responses: Any, config: Optional[TelegramConfig] = None, memory: bool = True,
             chat: bool = True, watcher: bool = True, apps: Any = None, **kw: Any) -> Rig:
    """A TelegramInlet lent what lite.py lends (LiteDaemon: memory, chat log, watcher, Connect, no agent, no launcher,
    no screen, no Claude sessions, no Codex folders), on test_telegram's fakes."""
    lent: dict[str, Any] = dict(agent_factory=None, agent_enabled=False, launcher=Spy("refused"),
                                launch_recent=lambda: [], claude_sessions=lambda: [], codex_folders=lambda: [],
                                screen=lambda: None, thinker=Spy({"ok": True, "answer": "42"}),
                                small_talk_ask=None)          # every turn reaches the brain the test scripts
    if memory:
        lent["memory"] = FakeMemory(tmp_path, profile="Name: Sam. Likes green tea.")
    if chat:
        lent["chat_log"] = chat_log.ChatLog(tmp_path / "chat" / "log.json")
    if watcher:
        lent["watcher"] = watch.Watcher(watch.WatchConfig(enabled=True, path=tmp_path / "watches.json"),
                                        fetch=lambda url, **_: (200, ""), offload=False)
    if apps is not None:
        lent["apps"] = apps
    lent.update(kw)
    return Rig(FakeApi(), FakeCreate(*responses), config=config or configured(LITE_ENV), **lent)


async def drain(rig: Rig) -> None:
    """Every job the inlet started, and the ones those started, run to their end."""
    for _ in range(100):
        await settle()
        jobs = [j for j in rig.inlet._jobs if not j.done()]
        if not jobs:
            return
        await asyncio.gather(*jobs, return_exceptions=True)


def app_turns(rig: Rig, *texts: str, during: Optional[Callable[[list[tuple[str, str, str]]], Any]] = None
              ) -> list[tuple[str, str, str]]:
    """Each text as an app turn of its own (link_chat's hear(app=<turn id>)), one after another; what the app was
    handed, as (turn id, line, kind). ``during``: the last turn asks the owner something, and ``during`` is called
    once the question reached the app (it answers it), before the turn is run to its end."""
    seen: list[tuple[str, str, str]] = []

    def app_out(turn: str, line: str, kind: str) -> bool:
        seen.append((turn, line, kind))
        return True

    rig.inlet.app_out = app_out

    async def go() -> None:
        for i, text in enumerate(texts):
            rig.inlet.hear(text, on_call=False, app=f"t-{i + 1}")
            if during is None or i < len(texts) - 1:
                await drain(rig)
        if during is not None:
            for _ in range(2000):
                if any(kind == "ask" for _, _, kind in seen):
                    break
                await asyncio.sleep(0)
            await during(seen)
            await drain(rig)

    asyncio.run(go())
    return seen


def said(seen: list[tuple[str, str, str]]) -> list[str]:
    return [line for _, line, _ in seen]


# ---- configuration: the web app's brain, never Telegram ---------------------------------------------------------

def test_lite_configures_the_web_apps_brain_with_one_local_chat_and_no_token() -> None:
    cfg = configured(LITE_ENV)
    assert cfg.enabled and cfg.app_only and cfg.token == ""
    assert cfg.owner_ids == frozenset({profile.LITE_CHAT_ID})
    assert cfg.model == telegram.DEFAULT_MODEL and cfg.effort == telegram.DEFAULT_EFFORT
    assert "Chat brain (eve's web app" in cfg.about and "Telegram" not in cfg.about
    # a token or an owner left in the environment changes nothing: lite never talks to Telegram
    leftover = configured({**LITE_ENV, "EVE_TELEGRAM": "1", "EVE_TELEGRAM_TOKEN": TOKEN, "EVE_TELEGRAM_OWNER": "42"})
    assert leftover.app_only and leftover.token == "" and leftover.owner_ids == frozenset({profile.LITE_CHAT_ID})
    assert TOKEN not in repr(leftover)
    # its models and efforts are read as full eve reads them, a bad effort falling back
    tuned = configured({**LITE_ENV, "EVE_TELEGRAM_MODEL": "gpt-6-sol", "EVE_TELEGRAM_EFFORT": "medium",
                        "EVE_APP_THINK_EFFORT": "sideways"})
    assert (tuned.model, tuned.effort, tuned.app_think_effort) == ("gpt-6-sol", "medium",
                                                                   telegram.DEFAULT_APP_THINK_EFFORT)


def test_full_eve_keeps_its_own_door_rule() -> None:
    """Control: with EVE_PROFILE unset the enable rule is the owner's: app-only still needs a token and an owner."""
    assert not configured({"EVE_TELEGRAM": "app"}).enabled
    assert not configured({"EVE_TELEGRAM": "app", "EVE_TELEGRAM_TOKEN": TOKEN}).enabled
    on = configured({"EVE_TELEGRAM": "app", "EVE_TELEGRAM_TOKEN": TOKEN, "EVE_TELEGRAM_OWNER": str(OWNER)})
    assert on.enabled and on.token == TOKEN and on.owner_ids == frozenset({OWNER})


def test_make_inlet_mutes_telegram_and_the_inlet_has_its_one_chat(monkeypatch: pytest.MonkeyPatch, lite: Any,
                                                                  tmp_path: Path) -> None:
    monkeypatch.setenv("OPENROUTER_API_KEY", PLACEHOLDER)
    monkeypatch.setenv("EVE_NOTIFY", "0")
    seen: dict[str, Any] = {}

    class Inlet:
        def __init__(self, config: TelegramConfig, api: Any, *a: Any, **k: Any) -> None:
            seen["api"] = api

    monkeypatch.setattr(telegram, "TelegramInlet", Inlet)
    hidden = set(telegram._hidden_tokens)
    telegram.make_inlet(configured(LITE_ENV))
    assert isinstance(seen["api"], MutedBotApi) and not hasattr(seen["api"], "_client")
    assert seen["api"].notifier is None and telegram._hidden_tokens == hidden    # "" is nothing to hide
    monkeypatch.undo()
    rig = lite_rig(tmp_path)
    assert rig.inlet._chat_id == profile.LITE_CHAT_ID


# ---- code words of features eve lite does not have ------------------------------------------------------------

BLOCKED_WORDS = (
    "claude on", "/claude_on", "claude off", "/claude_off", "claude on eve", "claude: fix the failing tests",
    "> fix the failing tests", "new claude", "/new_claude", "start claude in eve", "codex", "/codex on",
    "codex use eve", "codex: fix the failing tests", "stealth mode", "/stealth", "wake up", "/apps", "my apps",
    "open eve", "/stick", "eve link", "/stick new", "screenshot", "send me a screenshot", "what's on my screen",
    "/meet", "/meet 6pm", "/meet https://meet.google.com/abc-defg-hij", "/america",
    "/america when are taxes due", "/opentable", "/opentable dinner for 2 friday", "/jobs", "/jobs c12", "go c12",
    "done c3",
)


@pytest.mark.parametrize("word", BLOCKED_WORDS)
def test_a_code_word_of_a_feature_lite_lacks_gets_one_line_and_nothing_else(word: str, lite: Any,
                                                                            tmp_path: Path) -> None:
    rig = lite_rig(tmp_path)
    inlet = rig.inlet
    typed: list[str] = []

    async def type_to_claude(*args: Any, **kwargs: Any) -> bool:
        typed.append(str(args))
        return True

    inlet._type_to_claude = type_to_claude                 # the relay's way into a terminal: never reached
    seen = app_turns(rig, word)
    assert seen == [("t-1", LITE_NOT_HERE, "say")]
    assert rig.create.requests == [] and rig.api.sent == [] and typed == []
    assert not inlet.stealth and not inlet.claude and inlet._codex_chat is None and inlet._launch is None
    assert inlet._launcher.calls == [] and inlet._agent is None and not inlet.task_running
    # the exchange is in the chat's record, as a quick answer's is, so the web app shows it after a reload
    lines = [(ln["who"], ln["text"]) for ln in inlet._memory.said() if ln.get("kind") == "say"]
    assert lines[-2:] == [("owner", word), ("eve", LITE_NOT_HERE)]


def test_the_harness_sees_a_side_effect_when_there_is_one(full: None, tmp_path: Path) -> None:
    """Control for the test above: in full eve the same harness sees stealth go on, and the relay's line typed."""
    rig = lite_rig(tmp_path, config=TelegramConfig(enabled=True, app_only=True, owner_ids=frozenset({OWNER})))
    typed: list[str] = []

    async def type_to_claude(*args: Any, **kwargs: Any) -> bool:
        typed.append(str(args))
        return True

    rig.inlet._type_to_claude = type_to_claude
    seen = app_turns(rig, "stealth mode", "claude: fix the failing tests")
    assert rig.inlet.stealth and STEALTH_ON_LINE in said(seen) and typed
    assert LITE_NOT_HERE not in said(seen)


@pytest.mark.parametrize("word", ["stop", "/spend", "/watches", "what time is it"])
def test_stop_spend_watch_and_quick_answers_keep_working_in_lite(word: str, lite: Any, tmp_path: Path,
                                                                 monkeypatch: pytest.MonkeyPatch) -> None:
    rig = lite_rig(tmp_path)

    async def spend_text() -> str:
        return "Today: $0.12."

    monkeypatch.setattr(rig.inlet, "_spend_text", spend_text)
    seen = app_turns(rig, word)
    assert seen and LITE_NOT_HERE not in said(seen) and rig.create.requests == []
    if word == "stop":
        assert said(seen) == [NOTHING_TO_STOP_LINE]
    elif word == "/spend":
        assert said(seen) == ["Today: $0.12."]
    elif word == "/watches":
        assert "Watching" in said(seen)[0]


# ---- tools eve lite does not have: refused before any handler -------------------------------------------------

NOT_LITE_TOOLS = ("screenshot", "start_task", "send_file", "list_files", "read_file", "take_photo",
                  "start_coding_session", "claude_relay", "codex_relay", "steer_task", "stop_task", "look",
                  "move_head", "take_notes", "set_stealth", "pair_eve_link", "open_app", "capture_note",
                  "opentable_search", "lights_control", "")


def spy_handlers(monkeypatch: pytest.MonkeyPatch) -> dict[str, Spy]:
    """Every fixed tool's handler (TOOL_HANDLERS) swapped for a spy, so any handler that runs is seen."""
    spies = {name: Spy() for name in telegram.TOOL_HANDLERS}
    for name, spy in spies.items():
        monkeypatch.setitem(telegram.TOOL_HANDLERS, name, spy)
    return spies


@pytest.mark.parametrize("name", NOT_LITE_TOOLS)
def test_a_tool_lite_lacks_is_refused_and_no_handler_runs(name: str, lite: Any, tmp_path: Path,
                                                          monkeypatch: pytest.MonkeyPatch) -> None:
    rig = lite_rig(tmp_path, apps=FakeConnect())
    spies = spy_handlers(monkeypatch)
    result = asyncio.run(rig.inlet._tool(name, {"goal": "open Safari", "path": "~/Desktop/a.txt"}, 1))
    assert result == {"ok": False, "reason": LITE_TOOL_OFF}
    assert all(spy.calls == [] for spy in spies.values())
    assert rig.inlet._apps.executed == [] and rig.create.requests == []


def test_the_handlers_do_run_in_full_eve_and_for_lites_own_tools(lite: Any, tmp_path: Path,
                                                                 monkeypatch: pytest.MonkeyPatch) -> None:
    """Controls: lite's own fixed tools reach their handlers, and in full eve a Mac tool reaches its handler."""
    rig = lite_rig(tmp_path)
    spies = spy_handlers(monkeypatch)
    asyncio.run(rig.inlet._tool("remember", {"claim": "Sam likes green tea"}, 1))
    asyncio.run(rig.inlet._tool("web_search", {"query": "weather in Lisbon"}, 1))
    assert spies["remember"].calls and spies["web_search"].calls
    monkeypatch.delenv("EVE_PROFILE")
    asyncio.run(rig.inlet._tool("screenshot", {"caption": ""}, 1))
    assert spies["screenshot"].calls


@pytest.mark.parametrize("name, args", [("screenshot", {"caption": "here"}), ("take_notes", {"action": "start"}),
                                        ("start_task", {"goal": "open Safari"})])
def test_a_turn_whose_model_calls_a_tool_lite_lacks_is_told_which_it_has_and_goes_on(
        name: str, args: dict[str, Any], lite: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """parse_response takes any of eve's own names; in lite the turn refuses one it does not have as it refuses any
    name not offered, and the result lists the tools it has, so the next round repairs it. No handler runs and the
    owner is asked nothing."""
    rig = lite_rig(tmp_path, call(name, args), say("eve lite can't do that, but I can look it up for you."))
    spies = spy_handlers(monkeypatch)
    seen = app_turns(rig, "do that thing on my mac now")
    assert said(seen) == ["eve lite can't do that, but I can look it up for you."]
    assert all(spy.calls == [] for spy in spies.values()) and not any(kind == "ask" for _, _, kind in seen)
    refused = json.loads(rig.create.requests[1]["input"][-1]["output"])
    offered = telegram.offered_names(rig.create.requests[0]["tools"])
    assert refused == {"ok": False, "reason": "that name is not a tool here; the tools this turn has: "
                                              + ", ".join(offered)}
    assert name not in offered and name not in refused["reason"]


def test_a_tainted_lite_turn_never_asks_about_a_tool_lite_lacks(lite: Any, tmp_path: Path,
                                                                monkeypatch: pytest.MonkeyPatch) -> None:
    """Recording the room after outside words is a question first (tool_gate.needs_confirmation); in lite the call
    is refused before that question exists."""
    assert tool_gate.needs_confirmation("take_notes", {"action": "start"}, "a web search")   # the gate would ask
    rig = lite_rig(tmp_path, call("take_notes", {"action": "start"}), say("Not in eve lite."))
    spies = spy_handlers(monkeypatch)

    async def think() -> tuple[str, int]:
        return await rig.inlet._think([telegram.message_item("user", "take notes")], 1, taint="a web search")

    reply, rounds = asyncio.run(think())
    assert reply == "Not in eve lite." and rounds == 2 and spies["take_notes"].calls == []
    assert rig.inlet._live_question() is None and rig.api.sent == []


# ---- OpenRouter's 402: the no-credit line --------------------------------------------------------------------

def status_error(code: int) -> openai.APIStatusError:
    request = httpx.Request("POST", "https://openrouter.ai/api/v1/responses")
    return openai.APIStatusError(f"HTTP {code}", response=httpx.Response(code, request=request), body=None)


def test_a_402_is_no_credit_and_nothing_else_is() -> None:
    assert no_credit(status_error(402))
    assert not no_credit(status_error(500)) and not no_credit(status_error(401)) and not no_credit(ValueError("x"))
    try:
        try:
            raise status_error(402)
        except openai.APIStatusError as inner:
            raise RuntimeError("the round failed") from inner
    except RuntimeError as outer:
        assert no_credit(outer)
    request = httpx.Request("POST", "https://openrouter.ai/api/v1/responses")
    assert no_credit(httpx.HTTPStatusError("402", request=request, response=httpx.Response(402, request=request)))


def test_a_turn_that_gets_402_says_to_add_credits_in_lite_and_full_eve_says_what_it_said(
        lite: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    rig = lite_rig(tmp_path, status_error(402))
    assert said(app_turns(rig, "hello there, how are you doing")) == [LITE_NO_CREDIT_LINE]
    assert LITE_NO_CREDIT_LINE == ("OpenRouter has no credit on this key. Add credits at openrouter.ai, then send "
                                   "that again.")
    # control: full eve's turn failure line is unchanged
    monkeypatch.delenv("EVE_PROFILE")
    rig = lite_rig(tmp_path / "full", status_error(402),
                   config=TelegramConfig(enabled=True, app_only=True, owner_ids=frozenset({OWNER})))
    assert said(app_turns(rig, "hello there, how are you doing")) == [FAILED_LINE]


def test_a_turn_that_gets_401_says_to_sign_in_again_in_lite_and_full_eve_does_not(
        lite: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """OpenRouter turns the OAuth key down (a 401, e.g. the owner deleted it): eve lite's turn says to sign in again
    on the Mac, where the only fix is. It names no 402 remedy (credit would not help), and full eve, which has no
    such sign-in, keeps its generic failure line."""
    assert key_rejected(status_error(401)) and not key_rejected(status_error(402))
    assert not no_credit(status_error(401))                                 # the two 400s do not cross wires
    rig = lite_rig(tmp_path, status_error(401))
    assert said(app_turns(rig, "what's the weather like")) == [LITE_KEY_REJECTED_LINE]
    assert "sign in with OpenRouter again" in LITE_KEY_REJECTED_LINE and "the Mac" in LITE_KEY_REJECTED_LINE
    assert "credit" not in LITE_KEY_REJECTED_LINE
    # control: full eve's 401 is its generic line, not the lite re-sign-in line
    monkeypatch.delenv("EVE_PROFILE")
    full = lite_rig(tmp_path / "full", status_error(401),
                    config=TelegramConfig(enabled=True, app_only=True, owner_ids=frozenset({OWNER})))
    assert said(app_turns(full, "what's the weather like")) == [FAILED_LINE]


# ---- a rundown's words are an ordinary turn in lite ----------------------------------------------------------

def test_the_calendar_today_is_an_ordinary_turn_that_can_send_a_sign_in_link(lite: Any, tmp_path: Path) -> None:
    """User flow A.6: "what's on my calendar today?" with Gmail and the calendar not connected yet reaches the
    connection manager, whose link goes back in the reply. Full eve takes the same words as its rundown."""
    apps = FakeConnect()
    rig = lite_rig(tmp_path, call("COMPOSIO_MANAGE_CONNECTIONS", {"toolkits": ["googlecalendar"]}),
                   say("Connect your calendar here: https://connect.composio.dev/link/ln_test"), apps=apps)
    seen = app_turns(rig, "what's on my calendar today")
    first = rig.create.requests[0]
    assert "COMPOSIO_MANAGE_CONNECTIONS" in telegram.offered_names(first["tools"])
    assert not any(item.get("role") == "developer" and "Current source context" in json.dumps(item)
                   for item in first["input"])
    assert apps.executed == [("COMPOSIO_MANAGE_CONNECTIONS", {"toolkits": ["googlecalendar"]})]   # no question
    assert said(seen) == ["Connect your calendar here: https://connect.composio.dev/link/ln_test"]


def test_full_eve_still_takes_those_words_as_its_rundown(full: None, tmp_path: Path) -> None:
    rig = lite_rig(tmp_path, say("Your day."), config=TelegramConfig(enabled=True, app_only=True,
                                                                     owner_ids=frozenset({OWNER})))
    app_turns(rig, "what's on my calendar today")
    first = rig.create.requests[0]
    assert any(item.get("role") == "developer" and "Current source context" in json.dumps(item)
               for item in first["input"])


# ---- Composio's gate in a lite turn ---------------------------------------------------------------------------

def test_the_owners_own_gmail_is_read_without_a_question(lite: Any, tmp_path: Path) -> None:
    apps = FakeConnect()
    read = {"tools": [{"tool_slug": "GMAIL_FETCH_EMAILS", "arguments": {"query": "from:sam"}}],
            "sync_response_to_workbench": False}
    rig = lite_rig(tmp_path, call("COMPOSIO_MULTI_EXECUTE_TOOL", read), say("Two from Sam today."), apps=apps)
    seen = app_turns(rig, "any mail from sam today")
    assert apps.executed == [("COMPOSIO_MULTI_EXECUTE_TOOL", read)]
    assert said(seen) == ["Two from Sam today."]


@pytest.mark.parametrize("answer, runs", [("no", False), ("yes", True)])
def test_a_send_and_a_reconnect_ask_first_and_run_only_on_yes(answer: str, runs: bool, lite: Any,
                                                              tmp_path: Path) -> None:
    send = {"tools": [{"tool_slug": "GMAIL_SEND_EMAIL", "arguments": {
        "recipient_email": "reader@example.org", "subject": "Lunch", "body": "Noon works."}}],
        "sync_response_to_workbench": False}
    reconnect = {"toolkits": ["gmail"], "reinitiate_all": True}
    for name, args, word in (("COMPOSIO_MULTI_EXECUTE_TOOL", send, "GMAIL_SEND_EMAIL"),
                             ("COMPOSIO_MANAGE_CONNECTIONS", reconnect, "reinitiate_all")):
        apps = FakeConnect()
        rig = lite_rig(tmp_path / f"{answer}-{word}", call(name, args), say("Done." if runs else "Okay, not done."),
                       apps=apps)

        async def during(seen: list[tuple[str, str, str]], rig: Rig = rig, word: str = word) -> None:
            asks = [line for _, line, kind in seen if kind == "ask"]
            assert len(asks) == 1 and word in asks[0] and "yes / no?" in asks[0]
            if word == "GMAIL_SEND_EMAIL":
                assert "reader@example.org" in asks[0] and "Noon works." in asks[0]   # the whole email
            assert rig.inlet._apps.executed == []
            rig.inlet.hear(answer, on_call=False, app="t-answer")

        app_turns(rig, "send it" if word == "GMAIL_SEND_EMAIL" else "reconnect my gmail", during=during)
        assert apps.executed == ([(name, args)] if runs else [])


def test_a_long_email_is_asked_whole_with_gmails_room_and_a_long_post_elsewhere_is_not(lite: Any,
                                                                                       tmp_path: Path) -> None:
    """eve lite's mail goes only through Composio: a send is shown whole with the room full eve gives the owner's own
    Gmail (apps_mcp.WHOLE_ASK_CHARS), where the shorter limit would refuse it unasked. Any other app keeps that
    limit (control), and full eve's question for the same call is what it was (control)."""
    body = "A paragraph of the letter, as the owner dictated it. " * 40           # about 2,100 characters
    assert telegram.tool_gate.SHOW_CHARS < len(body) < telegram.apps_mcp.WHOLE_ASK_CHARS - 200
    mail = {"tools": [{"tool_slug": "GMAIL_SEND_EMAIL", "arguments": {
        "recipient_email": "reader@example.org", "subject": "The letter", "body": body}}],
        "sync_response_to_workbench": False}
    post = {"tools": [{"tool_slug": "SLACK_SEND_MESSAGE", "arguments": {"channel": "design", "text": body}}],
            "sync_response_to_workbench": False}
    rig = lite_rig(tmp_path, apps=FakeConnect())
    asked: list[str] = []

    async def ask_user(question: str, chat_id: Any, **kwargs: Any) -> str:
        asked.append(question)
        return "no"

    rig.inlet._ask_user = ask_user
    refused = asyncio.run(rig.inlet._app_tool("COMPOSIO_MULTI_EXECUTE_TOOL", mail, 1))
    assert len(asked) == 1 and body.strip() in asked[0] and "reader@example.org" in asked[0]
    assert refused["ok"] is False and "said no" in refused["reason"] and rig.inlet._apps.executed == []
    too_long = asyncio.run(rig.inlet._app_tool("COMPOSIO_MULTI_EXECUTE_TOOL", post, 1))
    assert too_long == telegram.tool_gate.TOO_LONG and len(asked) == 1
    assert rig.inlet._write_question(composio_tools, "COMPOSIO_MULTI_EXECUTE_TOOL", mail) is None
