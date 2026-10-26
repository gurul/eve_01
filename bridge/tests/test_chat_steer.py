"""Chat steering (EVE_CHAT_STEER, telegram._steer_entry and _steer_into): a plain message typed while the chat's text
turn runs is folded into that turn as an owner message before its next model call, instead of queueing behind it.
Every test name has "steer" in it: `pytest tests/test_chat_steer.py -k steer`. No network: the model is FakeCreate,
Telegram is FakeApi, the web is a stub."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any, Callable

import pytest
from test_telegram import OWNER, FakeApi, FakeCreate, FakeMemory, Rig, call, say, settle

from eve_bridge import telegram, websearch

FIRST = "find me a ramen place near the office"
SECOND = "actually make it sushi"


class _MaxTurns(dict):
    """The inlet's ``_open_turns``, remembering the most entries it ever held."""

    most = 0

    def __setitem__(self, key: Any, value: Any) -> None:
        super().__setitem__(key, value)
        self.most = max(self.most, len(self))


def _on(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(telegram.CHAT_STEER_ENV, "1")


def _off(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(telegram.CHAT_STEER_ENV, raising=False)


def _typed(text: str, chat: int = OWNER, message_id: int = 8, tapped: bool = False) -> telegram.Inbound:
    return telegram.Inbound(chat, OWNER, text, message_id=message_id, tapped=tapped)


def _held(release: asyncio.Event, response: dict[str, Any]) -> Callable[[], Any]:
    """A model call that is still in flight until ``release`` is set."""

    async def go() -> dict[str, Any]:
        await release.wait()
        return response

    return go


def _users(request: dict[str, Any]) -> list[str]:
    """The user-role words in a request's input, in order."""
    out = []
    for item in request["input"]:
        if item.get("role") == "user":
            out.extend(c.get("text", "") for c in item.get("content", []) if isinstance(c, dict))
    return out


def _kinds(request: dict[str, Any]) -> list[str]:
    """The request's history items by kind, the turn's own developer note left out."""
    kinds = [str(i.get("type") if i.get("type") != "message" else i.get("role")) for i in request["input"]]
    return [k for k in kinds if k != "developer"]


async def _until(check: Callable[[], bool]) -> None:
    for _ in range(500):
        if check():
            return
        await asyncio.sleep(0.01)
    raise AssertionError("never happened")


async def _jobs(rig: Rig) -> None:
    await settle()
    await asyncio.gather(*list(rig.inlet._jobs))


class _Tools:
    """Stands in for the inlet's ``_tool``: records every executed call, and holds web_search until released."""

    def __init__(self, hold: bool = False) -> None:
        self.ran: list[tuple[str, dict[str, Any]]] = []
        self.release = asyncio.Event()
        self.hold = hold
        self.running = False

    async def __call__(self, name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:
        self.ran.append((name, args))
        if self.hold:
            self.running = True
            await self.release.wait()
            self.running = False
        return {"ok": True, "answer": "Three places nearby."}


def test_steer_the_switch_reads_one_and_nothing_else() -> None:
    assert telegram._chat_steer({}) is False
    assert telegram._chat_steer({"EVE_CHAT_STEER": "0"}) is False
    assert telegram._chat_steer({"EVE_CHAT_STEER": ""}) is False
    assert telegram._chat_steer({"EVE_CHAT_STEER": "1"}) is True


def test_steer_off_the_second_message_queues_as_its_own_turn(monkeypatch: pytest.MonkeyPatch) -> None:
    _off(monkeypatch)
    release = asyncio.Event()
    create = FakeCreate(_held(release, call("web_search", {"query": "ramen"})), say("Ramen: Ippudo."),
                        say("Sushi: Nobu."))
    rig = Rig(FakeApi(), create)
    tools = _Tools()
    rig.inlet._tool = tools
    rig.inlet._open_turns = _MaxTurns()

    async def go() -> None:
        turn = asyncio.ensure_future(rig.inlet._turn(_typed(FIRST, message_id=7)))
        await _until(lambda: len(create.requests) == 1)
        rig.inlet._handle(_typed(SECOND))
        await settle()
        assert rig.inlet._steering is None
        release.set()
        await asyncio.wait_for(turn, 1.0)
        await _jobs(rig)

    asyncio.run(go())
    assert len(create.requests) == 3                                    # two rounds, then a turn of its own
    assert SECOND not in _users(create.requests[1])
    assert _users(create.requests[2])[-1] == SECOND
    assert rig.inlet._open_turns.most == 2
    assert [t for _c, t in rig.api.sent][-2:] == ["Ramen: Ippudo.", "Sushi: Nobu."]


def test_steer_on_folds_the_message_into_the_same_turn_before_the_next_model_call(
        monkeypatch: pytest.MonkeyPatch) -> None:
    _on(monkeypatch)
    release = asyncio.Event()
    create = FakeCreate(_held(release, call("web_search", {"query": "ramen"})),
                        call("web_search", {"query": "sushi"}, call_id="call_2"), say("Sushi: Nobu."))
    rig = Rig(FakeApi(), create)
    tools = _Tools()
    rig.inlet._tool = tools
    rig.inlet._open_turns = _MaxTurns()

    async def go() -> None:
        turn = asyncio.ensure_future(rig.inlet._turn(_typed(FIRST, message_id=7)))
        await _until(lambda: len(create.requests) == 1)
        rig.inlet._handle(_typed(SECOND))
        await settle()
        assert len(rig.inlet._steering.inbox) == 1                    # waiting; the model call is not cut
        release.set()
        await asyncio.wait_for(turn, 1.0)
        await _jobs(rig)

    asyncio.run(go())
    assert len(create.requests) == 3                                    # one turn: no second turn's model call
    second = create.requests[1]
    assert _users(second)[-2:] == [FIRST, SECOND]
    # the owner's words come after the round's tool result, as a genuine user message, never inside the result
    assert _kinds(second)[-2:] == ["function_call_output", "user"]
    assert all(SECOND not in i.get("output", "") for i in second["input"] if i.get("type") == "function_call_output")
    assert [n for n, _a in tools.ran] == ["web_search", "web_search"]
    assert [a["query"] for _n, a in tools.ran] == ["ramen", "sushi"]
    assert [t for _c, t in rig.api.sent].count("Sushi: Nobu.") == 1
    # noted the way its own turn notes it: in the history, after the first message and before the reply
    users = [(w, t) for w, t in rig.inlet.turns]
    assert users[-3:] == [("user", FIRST), ("user", SECOND), ("eve", "Sushi: Nobu.")]
    assert rig.inlet._steering is None and rig.inlet._open_turns == {}


def test_steer_a_message_during_a_running_tool_waits_until_the_tool_returns(monkeypatch: pytest.MonkeyPatch) -> None:
    _on(monkeypatch)
    create = FakeCreate(call("web_search", {"query": "ramen"}), say("Sushi: Nobu."))
    rig = Rig(FakeApi(), create)
    tools = _Tools(hold=True)
    rig.inlet._tool = tools
    rig.inlet._open_turns = _MaxTurns()

    async def go() -> None:
        turn = asyncio.ensure_future(rig.inlet._turn(_typed(FIRST, message_id=7)))
        await _until(lambda: tools.running)
        rig.inlet._handle(_typed(SECOND))
        await settle()
        assert tools.running and not turn.done() and len(create.requests) == 1   # the tool is not interrupted
        tools.release.set()
        await asyncio.wait_for(turn, 1.0)
        await _jobs(rig)

    asyncio.run(go())
    assert len(create.requests) == 2
    assert _kinds(create.requests[1])[-2:] == ["function_call_output", "user"]
    outputs = [i["output"] for i in create.requests[1]["input"] if i.get("type") == "function_call_output"]
    assert [json.loads(o) for o in outputs] == [{"ok": True, "answer": "Three places nearby."}]   # whole, unchanged
    assert _users(create.requests[1])[-1] == SECOND
    assert len(tools.ran) == 1


def test_steer_a_message_after_the_last_model_call_runs_as_the_next_turn(monkeypatch: pytest.MonkeyPatch) -> None:
    _on(monkeypatch)
    release = asyncio.Event()
    create = FakeCreate(_held(release, say("Ramen: Ippudo.")), say("Sushi: Nobu."))
    rig = Rig(FakeApi(), create)
    rig.inlet._open_turns = _MaxTurns()

    async def go() -> None:
        turn = asyncio.ensure_future(rig.inlet._turn(_typed(FIRST, message_id=7)))
        await _until(lambda: len(create.requests) == 1)
        rig.inlet._handle(_typed(SECOND))
        release.set()
        await asyncio.wait_for(turn, 1.0)
        await _jobs(rig)

    asyncio.run(go())
    assert len(create.requests) == 2
    assert _users(create.requests[1])[-1] == SECOND                     # a fresh turn's request
    assert not any(i.get("type") == "function_call_output" for i in create.requests[1]["input"])
    assert rig.inlet._open_turns.most == 2
    assert [t for _c, t in rig.api.sent][-2:] == ["Ramen: Ippudo.", "Sushi: Nobu."]


def test_steer_the_last_round_with_tools_leaves_the_message_for_its_own_turn(monkeypatch: pytest.MonkeyPatch) -> None:
    _on(monkeypatch)
    monkeypatch.setattr(telegram, "MAX_TOOL_ROUNDS", 2)
    create = FakeCreate(call("web_search", {"query": "ramen"}), say("Ramen: Ippudo."), say("Sushi: Nobu."))
    rig = Rig(FakeApi(), create)
    tools = _Tools(hold=True)
    rig.inlet._tool = tools
    rig.inlet._open_turns = _MaxTurns()

    async def go() -> None:
        turn = asyncio.ensure_future(rig.inlet._turn(_typed(FIRST, message_id=7)))
        await _until(lambda: tools.running)
        rig.inlet._handle(_typed(SECOND))
        tools.release.set()
        await asyncio.wait_for(turn, 1.0)
        await _jobs(rig)

    asyncio.run(go())
    assert len(create.requests) == 3
    assert SECOND not in _users(create.requests[1])                     # round 2 is the last with tools
    assert _users(create.requests[2])[-1] == SECOND
    assert rig.inlet._open_turns.most == 2


def test_steer_the_out_of_rounds_summary_never_folds(monkeypatch: pytest.MonkeyPatch) -> None:
    _on(monkeypatch)
    monkeypatch.setattr(telegram, "MAX_TOOL_ROUNDS", 1)
    release = asyncio.Event()
    create = FakeCreate(call("web_search", {"query": "ramen"}), _held(release, say("Found Ippudo; keep going?")),
                        say("Sushi: Nobu."))
    rig = Rig(FakeApi(), create)
    rig.inlet._tool = _Tools()

    async def go() -> None:
        turn = asyncio.ensure_future(rig.inlet._turn(_typed(FIRST, message_id=7)))
        await _until(lambda: len(create.requests) == 2)               # the tools-off summary is in flight
        rig.inlet._handle(_typed(SECOND))
        release.set()
        await asyncio.wait_for(turn, 1.0)
        await _jobs(rig)

    asyncio.run(go())
    assert len(create.requests) == 3
    assert SECOND not in _users(create.requests[1])
    assert _users(create.requests[2])[-1] == SECOND


def _running(rig: Rig, create: FakeCreate) -> tuple[asyncio.Event, Any]:
    release = asyncio.Event()
    create.responses = [_held(release, say("Ramen: Ippudo."))] + create.responses
    return release, rig.inlet._turn(_typed(FIRST, message_id=7))


def test_steer_an_answer_to_an_open_question_still_answers_it(monkeypatch: pytest.MonkeyPatch) -> None:
    _on(monkeypatch)
    create = FakeCreate()
    rig = Rig(FakeApi(), create)

    async def go() -> asyncio.Future:
        release, coro = _running(rig, create)
        turn = asyncio.ensure_future(coro)
        await _until(lambda: len(create.requests) == 1)
        waiting: asyncio.Future = asyncio.get_running_loop().create_future()
        rig.inlet._pending_answer = waiting
        rig.inlet._handle(_typed("the second one"))
        await settle()
        assert rig.inlet._steering.inbox == []
        release.set()
        await asyncio.wait_for(turn, 1.0)
        await _jobs(rig)
        return waiting

    waiting = asyncio.run(go())
    assert waiting.result() == "the second one"
    assert len(create.requests) == 1


@pytest.mark.parametrize("text,tapped", [("/start", False), ("eve: find sushi instead", False), ("Sushi", True)])
def test_steer_commands_eve_prefix_and_taps_are_never_folded(monkeypatch: pytest.MonkeyPatch, text: str,
                                                             tapped: bool) -> None:
    _on(monkeypatch)
    create = FakeCreate(say("Later."))
    rig = Rig(FakeApi(), create)

    async def go() -> None:
        release, coro = _running(rig, create)
        turn = asyncio.ensure_future(coro)
        await _until(lambda: len(create.requests) == 1)
        rig.inlet._handle(_typed(text, tapped=tapped))
        await settle()
        assert rig.inlet._steering.inbox == []
        release.set()
        await asyncio.wait_for(turn, 1.0)
        await _jobs(rig)

    asyncio.run(go())
    if text == "/start":
        assert any(t == telegram.HELLO_LINE for _c, t in rig.api.sent)
        assert len(create.requests) == 1
    else:
        assert len(create.requests) == 2                                # its own turn, as today


def test_steer_relay_text_goes_to_the_relay(monkeypatch: pytest.MonkeyPatch) -> None:
    _on(monkeypatch)
    create = FakeCreate()
    rig = Rig(FakeApi(), create)
    relayed: list[str] = []

    async def muse_send(chat_id: int, text: str) -> None:
        relayed.append(text)

    rig.inlet._muse_send = muse_send
    rig.inlet.muse = True

    async def go() -> None:
        release, coro = _running(rig, create)
        turn = asyncio.ensure_future(coro)
        await _until(lambda: len(create.requests) == 1)
        rig.inlet._handle(_typed(SECOND))
        await settle()
        assert rig.inlet._steering.inbox == []
        release.set()
        await asyncio.wait_for(turn, 1.0)
        await _jobs(rig)

    asyncio.run(go())
    assert relayed == [SECOND] and len(create.requests) == 1


def test_steer_a_chief_code_word_stays_the_chief_s(monkeypatch: pytest.MonkeyPatch) -> None:
    _on(monkeypatch)
    create = FakeCreate()
    rig = Rig(FakeApi(), create)
    heard: list[tuple[str, str]] = []

    async def chief_command(inbound: telegram.Inbound, word: str, card: str) -> None:
        heard.append((word, card))

    rig.inlet._chief_word = lambda text: ("jobs", "") if text == "jobs" else None    # the chief is on
    rig.inlet._chief_command = chief_command

    async def go() -> None:
        release, coro = _running(rig, create)
        turn = asyncio.ensure_future(coro)
        await _until(lambda: len(create.requests) == 1)
        rig.inlet._handle(_typed("jobs"))
        await settle()
        assert rig.inlet._steering.inbox == []
        release.set()
        await asyncio.wait_for(turn, 1.0)
        await _jobs(rig)

    asyncio.run(go())
    assert heard == [("jobs", "")] and len(create.requests) == 1


def test_steer_another_chat_is_not_folded(monkeypatch: pytest.MonkeyPatch) -> None:
    _on(monkeypatch)
    create = FakeCreate(say("Sushi: Nobu."))
    rig = Rig(FakeApi(), create)

    async def go() -> None:
        release, coro = _running(rig, create)
        turn = asyncio.ensure_future(coro)
        await _until(lambda: len(create.requests) == 1)
        rig.inlet._handle(_typed(SECOND, chat=OWNER + 1))
        await settle()
        assert rig.inlet._steering.inbox == []
        release.set()
        await asyncio.wait_for(turn, 1.0)
        await _jobs(rig)

    asyncio.run(go())
    assert len(create.requests) == 2 and _users(create.requests[1])[-1] == SECOND


def test_steer_an_app_or_call_turn_keeps_the_queue(monkeypatch: pytest.MonkeyPatch) -> None:
    _on(monkeypatch)
    create = FakeCreate()
    rig = Rig(FakeApi(), create)

    async def go() -> None:
        mark = telegram._FROM_APP.set(object())
        try:
            release, coro = _running(rig, create)
            turn = asyncio.ensure_future(coro)
        finally:
            telegram._FROM_APP.reset(mark)
        await _until(lambda: len(create.requests) == 1)
        assert rig.inlet._steering is None
        release.set()
        await asyncio.wait_for(turn, 1.0)

    asyncio.run(go())


def test_steer_a_gated_acting_call_after_a_folded_message_still_asks(
        monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    _on(monkeypatch)
    monkeypatch.setattr(websearch, "search", lambda query, config=None: {"ok": True, "answer": "Outside words."})
    memory = FakeMemory(tmp_path)
    release = asyncio.Event()
    create = FakeCreate(_held(release, call("web_search", {"query": "sam"})),
                        call("forget_apply", {"token": "t-sam"}, call_id="call_2"), say("Okay, I left it."))
    rig = Rig(FakeApi(), create, memory=memory)
    rig.inlet._open_turns = _MaxTurns()

    async def go() -> None:
        turn = asyncio.ensure_future(rig.inlet._turn(_typed("look up what I said about Sam", message_id=7)))
        await _until(lambda: len(create.requests) == 1)
        rig.inlet._handle(_typed("and then forget it"))
        release.set()
        # the web result tainted the turn: round 2's acting call, written after the folded message, still asks
        await _until(lambda: rig.inlet._live_question() is not None and bool(rig.api.keyboards))
        assert not any(c[0] == "forget_apply" for c in memory.calls)
        from test_telegram import tap_update

        message_id, key = rig.api.key("No")
        rig.inlet._dispatch(tap_update(key, message_id=message_id))
        await asyncio.wait_for(turn, 1.0)
        await _jobs(rig)

    asyncio.run(go())
    assert _users(create.requests[1])[-1] == "and then forget it"
    assert not any(c[0] == "forget_apply" for c in memory.calls)
    assert len(create.requests) == 3 and rig.inlet._open_turns.most == 2   # its queued turn ended unasked


def test_steer_a_turn_already_queued_behind_keeps_its_place(monkeypatch: pytest.MonkeyPatch) -> None:
    """A turn that waits behind the running one (a picture, a quick answer's fallback) is answered before a message
    typed after it: that message is not folded ahead of it, and neither is any later one."""
    _on(monkeypatch)
    release = asyncio.Event()
    create = FakeCreate(_held(release, call("web_search", {"query": "ramen"})), say("Ramen: Ippudo."),
                        say("That is a cat."), say("Sushi: Nobu."))
    rig = Rig(FakeApi(), create)
    rig.inlet._tool = _Tools()

    async def go() -> None:
        turn = asyncio.ensure_future(rig.inlet._turn(_typed(FIRST, message_id=7)))
        await _until(lambda: len(create.requests) == 1)
        between = asyncio.ensure_future(rig.inlet._turn(_typed("what is in this picture", message_id=8)))
        await settle()
        rig.inlet._handle(_typed(SECOND, message_id=9))
        await settle()
        assert rig.inlet._steering is not None and rig.inlet._steering.inbox == []
        assert rig.inlet._steering.open is False
        release.set()
        await asyncio.wait_for(asyncio.gather(turn, between), 1.0)
        await _jobs(rig)

    asyncio.run(go())
    assert len(create.requests) == 4
    assert SECOND not in _users(create.requests[1])                     # never folded into the first turn
    assert _users(create.requests[2])[-1] == "what is in this picture"
    assert _users(create.requests[3])[-1] == SECOND
    assert [t for _c, t in rig.api.sent][-3:] == ["Ramen: Ippudo.", "That is a cat.", "Sushi: Nobu."]


def test_steer_recent_messages_leaves_out_every_message_of_the_turn(
        monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """With a message folded in, recent_messages still leaves out the messages being answered (both of them), so
    "the message before my last one" counts from before the turn."""
    from test_chat_memory import rig_with_log

    _on(monkeypatch)
    release = asyncio.Event()
    earlier = "remind me to water the plants on friday"
    rig, wall, log = rig_with_log(tmp_path, _held(release, call("web_search", {"query": "ramen"})),
                                  call("recent_messages", {"count": 6, "before": 0}, call_id="call_2"),
                                  say("Sushi: Nobu."))
    if log is None:
        pytest.skip("no chat log module")
    tools = _Tools()
    real_tool = rig.inlet._tool

    async def tool(name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:
        if name == "recent_messages":
            return await real_tool(name, args, chat_id)
        return await tools(name, args, chat_id)

    rig.inlet._tool = tool
    rig.inlet._note("user", earlier)
    wall.t += 20
    rig.inlet._note("eve", "I will remind you on Friday.")
    wall.t += 20

    async def go() -> None:
        turn = asyncio.ensure_future(rig.inlet._turn(_typed(FIRST, message_id=7)))
        await _until(lambda: len(rig.create.requests) == 1)
        rig.inlet._handle(_typed(SECOND))
        await settle()
        release.set()
        await asyncio.wait_for(turn, 1.0)
        await _jobs(rig)

    asyncio.run(go())
    outs = [json.loads(i["output"]) for i in rig.create.requests[-1]["input"]
            if i.get("type") == "function_call_output" and i.get("call_id") == "call_2"]
    assert outs, "recent_messages never ran"
    texts = [m.get("text") for m in outs[0]["messages"]]
    assert FIRST not in texts and SECOND not in texts
    assert earlier in texts
    owner = {m["owner_back"]: m["text"] for m in outs[0]["messages"] if "owner_back" in m}
    assert owner[1] == earlier
