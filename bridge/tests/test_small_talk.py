"""The small-talk lane (small_talk.py and its wiring in telegram.py): the phrase list, the fall-through to the brain
(attachment, eve waiting, every error), the call's body, the transcript, and the switch. No network: Haiku is a fake
everywhere."""

from __future__ import annotations

import asyncio
import json
import logging
import time
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Optional

import pytest
from test_link_chat import events, serve
from test_link_chat import turn as app_turn
from test_telegram import OWNER, FakeApi, FakeCreate, FakeMemory, Rig, call, run_rig, say, settle, update

from eve_bridge import link_chat, openrouter, records, small_talk, telegram, toolset_router
from eve_bridge.recall import RecallConfig
from eve_bridge.small_talk import State
from eve_bridge.telegram import Inbound

HAIKU = "anthropic/claude-haiku-5.5"


# ---- fakes ------------------------------------------------------------------------------------------------------

class FakeLane:
    """small_talk.ask, scripted: answers ``reply`` (or fails with ``reason``, or raises); records each call."""

    def __init__(self, reply: str = "Hey you.", reason: str = "", raises: Optional[Exception] = None) -> None:
        self.reply, self.reason, self.raises = reply, reason, raises
        self.calls: list[tuple[str, str]] = []

    async def __call__(self, text: str, voice: str = "") -> small_talk.Answer:
        self.calls.append((text, voice))
        if self.raises is not None:
            raise self.raises
        if self.reason:
            return small_talk.Answer(reason=self.reason, secs=0.4, model=HAIKU)
        return small_talk.Answer(reply=self.reply, secs=0.4, model=HAIKU)


class Recorder:
    """A toolset router's Jev or a grader that records what it was asked."""

    def __init__(self, result: Any) -> None:
        self.result, self.calls = result, []

    def __call__(self, text: str, previous: str = "") -> Any:
        self.calls.append((text, previous))
        return self.result


def router() -> Recorder:
    return Recorder(toolset_router.Route(ms=1.0))


def grader() -> Recorder:
    from eve_bridge.grade_router import GradeAnswer

    return Recorder(GradeAnswer(0.0, 1.0))


def lane_rig(lane: Any, *responses: Any, memory: Optional[FakeMemory] = None, **kw: Any) -> Rig:
    api = kw.pop("api", FakeApi())
    extra = {"memory": memory} if memory is not None else {}
    rig = Rig(api, FakeCreate(*responses), small_talk_ask=lane, **extra, **kw)
    rig.inlet._chat_id = OWNER
    return rig


# ---- the phrase list (code, no model) -----------------------------------------------------------------------------

@pytest.mark.parametrize("text", [
    "hi", "Hi!", "  hey  ", "heyyy", "hey eve", "Eve, hello", "hello eve!!", "yo", "hiya", "good morning",
    "Good Morning Eve ☀️", "goood morning", "morning", "evening", "gm", "gn",
    "thanks", "thanks!", "thank you", "thank youuu", "Thank you so much!!", "thx", "ty", "thanks eve", "thanks, eve",
    "appreciate it", "thanks 🙏",
    "bye", "goodnight", "good night eve", "night", "see ya", "later", "ttyl",
    "how are you", "how are you?", "How’s it going", "hows it going?", "how are you doing eve", "you good?",
    "lol", "LOL", "loool", "haha", "hahaha", "lmao", "lmaooo", "😂", "😂😂😂",
    "you're the best", "youre the best eve", "love you", "good job", "nice work!",
])
def test_small_talk_phrases_match(text: str) -> None:
    assert small_talk.matches(text), text
    assert small_talk.gate(text, State()) == ""

@pytest.mark.parametrize("text", [
    # greetings
    "sup", "what's up", "whats up?", "Whatsup eve", "wassup", "wasssup", "hello hello", "hey hey", "hi hi",
    "hey there", "Hi there!", "hello there eve", "howdy", "heya", "good day", "happy monday", "Happy Friday!",
    "happy sunday eve", "happy weekend", "top of the morning",
    # goodbyes
    "see you", "see you later", "see ya later", "cya", "later eve", "talk later", "catch you later", "ok bye",
    "okay bye", "thanks bye", "ok thanks bye", "bye bye", "byee byee", "nighty night", "night night", "sleep well",
    "sweet dreams", "gn eve", "good night eve", "have a good one", "take care",
    # thanks
    "thanks a lot", "thanks a ton!", "many thanks", "cheers", "cheers eve", "thank u", "tysm", "much appreciated",
    "thanks so much",
    # how are you
    "how are things?", "how's your day", "how was your day?", "how's it going eve", "how have you been",
    "how you doing", "you doing ok?",
    # laughter
    "hehe", "hehehe", "haha nice", "lol nice", "rofl", "🤣", "🤣🤣", "that's funny", "you're funny", "good one",
    "lmfao",
    # compliments
    "you're awesome", "you rock", "you're amazing", "love ya", "you're the goat", "best assistant", "legend",
])
def test_the_grown_list_matches(text: str) -> None:
    assert small_talk.matches(text), text
    assert small_talk.gate(text, State()) == ""


@pytest.mark.parametrize("text", [
    "got it", "sounds good", "perfect", "do it", "go ahead", "nvm", "wait", "yeah", "alright",
    "what's up with my order", "see you at 5", "talk later about the trip", "happy birthday to mom",
    "thanks bye and cancel it", "cheers, book it", "good one, now send it", "take care of it",
    "how was your day planned", "legend, do it again", "ok", "ok bye now send", "what's new",
])
def test_the_grown_list_keeps_answers_and_commands_out(text: str) -> None:
    assert not small_talk.matches(text), text


@pytest.mark.parametrize("text", [
    "", "   ", "eve", "ok", "okay", "yes", "no", "yep", "nope", "sure", "cool", "nice", "great", "👍", "🙏", "😂🤣",
    "hi can you check my mail", "hi, can you…", "thanks, now send it", "thanks send it", "thanks but no",
    "good morning what's on today", "good morning, what's on my calendar", "how are you doing on that task",
    "night mode", "later today", "remind me later", "love you too much to say no", "stop", "/start",
    "what time is it", "play some music", "lights off", "hey eve turn on the lights",
])
def test_anything_else_is_not_small_talk(text: str) -> None:
    assert not small_talk.matches(text), text
    assert small_talk.gate(text, State()) == "no_match"


def test_normalise() -> None:
    assert small_talk.normalise("  Thanks,   EVE!! 🙏🙏 ") == "thanks"
    assert small_talk.normalise("You’re the best") == "youre the best"
    assert small_talk.normalise("😂😂😂") == "😂"
    assert small_talk.normalise("👍👍") == "👍"


@pytest.mark.parametrize("state, reason", [
    (State(attachment=True), "attachment"),
    (State(tapped=True), "tapped"),
    (State(awaiting_confirmation=True), "waiting"),
    (State(awaiting_answer=True), "waiting"),
    (State(running_task=True), "waiting"),
])
def test_a_match_still_goes_to_the_brain_when(state: State, reason: str) -> None:
    assert small_talk.gate("thanks", state) == reason
    assert small_talk.gate("thanks", State()) == ""                      # positive control


# ---- the switch ------------------------------------------------------------------------------------------------

@pytest.mark.parametrize("env, on", [({"EVE_SMALL_TALK": "0"}, False), ({}, False), ({"EVE_SMALL_TALK": "1"}, True)],
                         ids=["off", "unset", "on"])
def test_the_inlet_reads_the_switch(monkeypatch: pytest.MonkeyPatch, env: dict[str, str], on: bool) -> None:
    monkeypatch.delenv("EVE_SMALL_TALK", raising=False)
    for k, v in env.items():
        monkeypatch.setenv(k, v)
    rig = Rig(FakeApi(), FakeCreate())
    assert (rig.inlet._small_talk_ask is small_talk.ask) is on
    assert (rig.inlet._small_talk_ask is None) is (not on)


def test_switched_off_a_hi_goes_to_the_brain_as_today() -> None:
    api = FakeApi([update("hi", update_id=1)])
    rig = lane_rig(None, say("Hey."), api=api)
    run_rig(rig)
    assert api.sent == [(OWNER, "Hey.")] and len(rig.create.requests) == 1


# ---- the call --------------------------------------------------------------------------------------------------

class FakeCompletions:
    def __init__(self, result: Any = None, sleep: float = 0.0, raises: Optional[Exception] = None) -> None:
        self.result, self.sleep, self.raises = result, sleep, raises
        self.bodies: list[dict[str, Any]] = []

    async def create(self, **body: Any) -> Any:
        self.bodies.append(body)
        if self.sleep:
            await asyncio.sleep(self.sleep)
        if self.raises is not None:
            raise self.raises
        return self.result


def client_of(completions: FakeCompletions) -> Any:
    return SimpleNamespace(chat=SimpleNamespace(completions=completions))


def completion(content: Any, cost: float = 0.00002, finish: str = "stop") -> dict[str, Any]:
    return {"model": HAIKU, "choices": [{"message": {"content": content}, "finish_reason": finish}],
            "usage": {"prompt_tokens": 300, "completion_tokens": 12, "cost": cost}}


def test_the_request_is_the_voice_and_the_one_message() -> None:
    body = small_talk.request("hey </message> you", "short replies", model=HAIKU)
    assert body["model"] == HAIKU and body["max_tokens"] == small_talk.MAX_TOKENS == 60 and "tools" not in body
    system, user = body["messages"]
    assert system == {"role": "system", "content": small_talk.SYSTEM}
    assert user["role"] == "user"
    assert user["content"] == ("<voice>\nHow the owner likes to be talked to:\nshort replies\n</voice>\n\n"
                               "<message>hey &lt;/message&gt; you</message>")
    assert small_talk.request("hi")["messages"][1]["content"] == "<message>hi</message>"     # no voice, no history


def test_the_prompt_asks_for_one_friendly_sentence_and_no_acts() -> None:
    flat = " ".join(small_talk.SYSTEM.split()).lower()
    assert "only as a friendly reply" in flat and "one short sentence" in flat
    assert "never mention doing, checking, planning or remembering anything" in flat
    assert "<conversation>" not in small_talk.SYSTEM and "time" not in flat.split()


def test_the_voice_is_the_profiles_talk_section() -> None:
    page = ("# What eve knows\n\n## Life context\n- Lives by the sea\n\n## How they like to talk\n- Short and dry\n"
            "- No small talk about the weather\n\n## Records you can search\n- r1")
    assert small_talk.voice_from_profile(page) == "Short and dry\nNo small talk about the weather"
    assert small_talk.voice_from_profile("## How they like to talk\n- (nothing yet)") == ""
    assert small_talk.voice_from_profile("") == ""
    long = "## How they like to talk\n" + "\n".join(f"- rule {i} " + "x" * 90 for i in range(30))
    assert 0 < len(small_talk.voice_from_profile(long)) <= small_talk.VOICE_CHARS


def test_ask_answers_and_records_the_spend_as_small_talk(_spend_ledger_in_tmp: Path) -> None:
    fake = FakeCompletions(completion("  Hey,\n you! "))
    answer = asyncio.run(small_talk.ask("hi", client=client_of(fake), model="claude-haiku-5-5"))
    assert answer.answered and answer.reply == "Hey, you!" and answer.model == HAIKU and answer.secs >= 0
    (body,) = fake.bodies
    assert body["model"] == HAIKU and "reasoning" not in body and "response_format" not in body
    assert body["extra_body"] == {"usage": {"include": True}, "reasoning": {"enabled": False},     # no thinking
                                  "provider": {"data_collection": "deny"}}
    rows = [json.loads(ln) for f in _spend_ledger_in_tmp.glob("*.jsonl") for ln in f.read_text().splitlines()]
    assert [(r["f"], r["m"], r["usd"]) for r in rows] == [("small_talk", HAIKU, 0.00002)]


@pytest.mark.parametrize("fake, reason", [
    (FakeCompletions(sleep=1.0), "timeout"),
    (FakeCompletions(raises=RuntimeError("502 from the provider")), "error"),
    (FakeCompletions(completion(None)), "empty"),
    (FakeCompletions(completion("   ")), "empty"),
    (FakeCompletions({"error": "x"}), "empty"),
    (FakeCompletions("not a mapping"), "empty"),
    (FakeCompletions(completion("Hey there, it is so", finish="length")), "long"),
], ids=["timeout", "provider", "no-content", "blank", "no-choices", "not-a-dict", "cut"])
def test_every_failure_of_the_call_is_a_reason(fake: FakeCompletions, reason: str) -> None:
    answer = asyncio.run(small_talk.ask("hi", client=client_of(fake), timeout_s=0.1))
    assert not answer.answered and answer.reason == reason and answer.reply == ""


def test_no_key_is_a_reason(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(openrouter.KEY_ENV, raising=False)
    small_talk._CLIENT.clear()
    answer = asyncio.run(small_talk.ask("hi"))
    assert not answer.answered and answer.reason == "no_key"


def test_a_cancelled_call_stays_cancelled() -> None:
    async def go() -> None:
        task = asyncio.ensure_future(small_talk.ask("hi", client=client_of(FakeCompletions(sleep=5.0))))
        await asyncio.sleep(0.01)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task

    asyncio.run(go())


# ---- in the chat ---------------------------------------------------------------------------------------------------

def test_a_lane_turn_answers_writes_the_transcript_and_skips_the_brain(tmp_path: Path,
                                                                       caplog: pytest.LogCaptureFixture) -> None:
    lane, route = FakeLane("Hey you."), router()
    memory = FakeMemory(tmp_path / "m", profile="## How they like to talk\n- Dry humour")
    api = FakeApi([update("hey eve!", update_id=1)])
    rig = lane_rig(lane, memory=memory, api=api, toolset_mode="auto", toolset_route=route, grader=grader())
    with caplog.at_level(logging.INFO, logger="eve_bridge.telegram"):
        run_rig(rig)
    assert lane.calls == [("hey eve!", "Dry humour")]
    assert api.sent == [(OWNER, "Hey you.")]
    assert rig.create.requests == [] and route.calls == [] and rig.inlet._grader.calls == []
    # written as a normal turn: the transcript, and the history the next turn is shown
    assert [(ln["who"], ln["kind"], ln["text"]) for ln in memory.said() if ln["who"] != "system"] == [
        ("owner", "say", "hey eve!"), ("eve", "say", "Hey you.")]
    assert rig.closed == [[("user", "hey eve!"), ("eve", "Hey you.")]]
    logged = [r.getMessage() for r in caplog.records]
    assert any(m.startswith("telegram: turn answered in ") and m.endswith(f"(small talk, {HAIKU})") for m in logged)
    assert all("hey eve" not in m and "Hey you" not in m for m in logged)          # never the words


def test_a_phrase_not_on_the_list_never_calls_haiku() -> None:
    lane = FakeLane()
    api = FakeApi([update("ok", update_id=1)])
    rig = lane_rig(lane, say("Okay."), api=api)
    run_rig(rig)
    assert api.sent == [(OWNER, "Okay.")] and lane.calls == []


@pytest.mark.parametrize("lane", [FakeLane(raises=RuntimeError("boom")), FakeLane(reason="timeout"),
                                  FakeLane(reason="error"), FakeLane(reason="empty")],
                         ids=["raises", "timeout", "error", "empty"])
def test_a_broken_lane_falls_through_to_the_brain_unchanged(tmp_path: Path, lane: FakeLane) -> None:
    def rig_for(lane: Any, where: str) -> tuple[Rig, FakeMemory]:
        memory = FakeMemory(tmp_path / where)
        return lane_rig(lane, say("Hey."), memory=memory, api=FakeApi([update("hi", update_id=1)])), memory

    broken, broken_memory = rig_for(lane, "a")
    today, today_memory = rig_for(None, "b")
    run_rig(broken)
    run_rig(today)
    assert len(lane.calls) == 1
    assert broken.api.sent == today.api.sent == [(OWNER, "Hey.")]
    assert [r["input"] for r in broken.create.requests] == [r["input"] for r in today.create.requests]
    assert [(ln["who"], ln["text"]) for ln in broken_memory.said()] == \
        [(ln["who"], ln["text"]) for ln in today_memory.said()]                  # the owner's line written once


def broken_desk() -> bool:
    raise OSError("unreadable")


@pytest.mark.parametrize("busy, waits", [(lambda: True, True),          # the desk holds the Mac
                                         (broken_desk, True),            # can't read = waiting
                                         (lambda: False, False)],        # positive control
                         ids=["desk-busy", "unreadable", "nothing-waits"])
def test_eve_waiting_on_the_owner_sends_a_thanks_to_the_brain(busy: Any, waits: bool) -> None:
    lane = FakeLane("Anytime.")
    api = FakeApi([update("thanks", update_id=1)])
    rig = lane_rig(lane, say("You're welcome."), api=api)
    rig.inlet._busy = busy
    run_rig(rig)
    if waits:
        assert lane.calls == [] and api.sent == [(OWNER, "You're welcome.")] and len(rig.create.requests) == 1
    else:
        assert lane.calls == [("thanks", "")] and api.sent == [(OWNER, "Anytime.")] and rig.create.requests == []


def test_an_attachment_sends_a_hi_to_the_brain(tmp_path: Path) -> None:
    lane = FakeLane()
    rig = lane_rig(lane)
    upload = SimpleNamespace(kind="file", name="a.txt")

    async def go() -> None:
        assert not await rig.inlet._small_talk_lane(Inbound(chat_id=OWNER, user_id=OWNER, text="hi",
                                                            uploads=(upload,)))   # type: ignore[arg-type]
        assert not await rig.inlet._small_talk_lane(Inbound(chat_id=OWNER, user_id=OWNER, text="hi",
                                                            image=object()))     # type: ignore[arg-type]

    asyncio.run(go())
    assert lane.calls == []


def test_a_running_task_sends_the_message_to_the_brain() -> None:
    lane = FakeLane()
    api = FakeApi([update("open the calculator on the mac for me right now please", update_id=1)])
    rig = lane_rig(lane, call("start_task", {"goal": "open the calculator"}), say("Still on it."), api=api)

    async def during() -> None:
        assert rig.inlet.task_running
        api.feed(update("hi", update_id=2))
        await settle()
        rig.agents[0].release.set()

    run_rig(rig, during)
    assert lane.calls == []                                         # a task the owner may be steering
    assert len(rig.create.requests) == 2


def test_a_message_queued_behind_a_running_turn_is_the_brains() -> None:
    lane = FakeLane()
    gate = asyncio.Event()

    async def slow() -> dict[str, Any]:
        await gate.wait()
        return say("Long answer.")

    api = FakeApi([update("tell me a long story about a lighthouse keeper and a cat", update_id=1)])
    rig = lane_rig(lane, slow, say("Hi again."), api=api)

    async def during() -> None:
        api.feed(update("hi", update_id=2))
        await settle()
        gate.set()
        await settle()

    run_rig(rig, during)
    assert api.sent == [(OWNER, "Long answer."), (OWNER, "Hi again.")]
    assert lane.calls == []


# ---- the inlet's state, as the gate reads it ---------------------------------------------------------------------

def inbound(text: str = "hi", **kw: Any) -> Inbound:
    return Inbound(chat_id=OWNER, user_id=OWNER, text=text, **kw)


def test_the_inlet_state_is_clear_when_nothing_waits() -> None:
    rig = lane_rig(FakeLane())
    assert rig.inlet._small_talk_state(inbound()) == State()


def test_the_inlet_state_sees_every_wait() -> None:
    async def go() -> None:
        rig = lane_rig(FakeLane())
        inlet = rig.inlet
        loop = asyncio.get_running_loop()
        assert inlet._small_talk_state(inbound(tapped=True)).tapped
        assert inlet._small_talk_state(inbound(image=object())).attachment          # type: ignore[arg-type]
        assert inlet._small_talk_state(inbound(uploads=(object(),))).attachment
        # a permission prompt
        inlet._permission_future = loop.create_future()
        assert inlet._small_talk_state(inbound()).awaiting_confirmation
        inlet._permission_future.set_result("")
        # a task's question (the slot)
        inlet._pending_answer = loop.create_future()
        assert inlet._small_talk_state(inbound()).awaiting_answer
        inlet._pending_answer.set_result("")
        # a question with yes/no buttons in the list
        q = telegram._Question(loop.create_future(), OWNER, True, frozenset({"yes", "no"}), "Go?")
        inlet._questions.append(q)
        s = inlet._small_talk_state(inbound())
        assert s.awaiting_confirmation and s.awaiting_answer
        q.future.set_result("")
        # a relay on
        inlet.claude = True
        assert inlet._small_talk_state(inbound()).awaiting_answer
        inlet.claude = False
        inlet._codex_chat = OWNER
        assert inlet._small_talk_state(inbound()).awaiting_answer
        inlet._codex_chat = None
        # the desk has the Mac
        inlet._busy = lambda: True
        assert inlet._small_talk_state(inbound()).running_task
        inlet._busy = lambda: False
        # a purchase is open
        inlet._purchases = SimpleNamespace(active=lambda chat: True)
        assert inlet._small_talk_state(inbound()).awaiting_confirmation
        inlet._purchases = None
        # a "claude on" still deciding
        inlet._claude_on_job = loop.create_future()
        assert inlet._small_talk_state(inbound()).running_task
        inlet._claude_on_job.set_result(None)
        assert inlet._small_talk_state(inbound()) == State()                       # all cleared again

    asyncio.run(go())


def card(status: str = "active", waiting_for: Optional[str] = None, *phases: str) -> Any:
    return SimpleNamespace(status=status, waiting_for=waiting_for,
                           phases=[SimpleNamespace(status=p) for p in phases])


@pytest.mark.parametrize("cards, waiting, running", [
    ([], False, False),
    ([card("active", None, "queued")], False, False),
    ([card("waiting", "mac_slot", "queued")], False, False),
    ([card("waiting", "go", "queued")], True, False),
    ([card("waiting", "question")], True, False),
    ([card("waiting", "budget")], True, False),
    ([card("active", None, "done", "running")], False, True),
])
def test_the_chiefs_cards_are_read(cards: list[Any], waiting: bool, running: bool) -> None:
    rig = lane_rig(FakeLane())
    rig.inlet.chief = SimpleNamespace(ledger=SimpleNamespace(open=lambda: cards))
    assert rig.inlet._chief_waits() == (waiting, running)
    s = rig.inlet._small_talk_state(inbound())
    assert (s.awaiting_answer, s.running_task) == (waiting, running)


def test_an_unreadable_ledger_is_read_as_waiting() -> None:
    rig = lane_rig(FakeLane())

    def broken() -> list[Any]:
        raise OSError("disk")

    rig.inlet.chief = SimpleNamespace(ledger=SimpleNamespace(open=broken))
    assert rig.inlet._chief_waits() == (True, True)


# ---- the app ----------------------------------------------------------------------------------------------------

def test_an_app_hi_is_answered_by_the_lane_in_the_app(tmp_path: Path) -> None:
    from test_link_chat import real_rig

    lane, graded = FakeLane("Hey!"), grader()
    rig, memory = real_rig(tmp_path, small_talk_ask=lane, grader=graded)

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        status, _, body = await app_turn(port, token, "hi", "c-1")
        assert status == 200 and events(body) == (["Hey!"], True, 0)
        # the think grade is the owner asking for the thinking model: the brain, never the lane
        status, _, body = await __import__("test_link_chat").http(
            port, "POST", link_chat.TURN_PATH, {"text": "hey", "client_id": "c-2", "grade": "think"},
            token=token)
        assert status == 200 and events(body) == (["Thought about it."], True, 0)
        await server.close()

    rig.create.responses.append(say("Thought about it."))
    asyncio.run(go())
    assert [c[0] for c in lane.calls] == ["hi"]
    assert graded.calls == [] and len(rig.create.requests) == 1
    assert [(ln["who"], ln["text"], ln.get("via")) for ln in memory.said()][:2] == [
        ("owner", "hi", "app"), ("eve", "Hey!", "app")]


def test_words_on_a_call_are_never_the_lanes(tmp_path: Path) -> None:
    lane = FakeLane()
    rig = lane_rig(lane, say("Hello on the call."))
    heard: list[str] = []

    async def go() -> None:
        rig.inlet.hear("hi", call=heard.append)
        for _ in range(300):
            await asyncio.sleep(0.01)
            if rig.create.requests:
                break
        await settle()

    asyncio.run(go())
    assert lane.calls == [] and len(rig.create.requests) == 1


def test_the_lane_is_fast_against_a_slow_brain() -> None:
    """The point of it: a lane turn never waits on the brain (the brain's fake would take 2 s)."""
    async def slow() -> dict[str, Any]:
        await asyncio.sleep(2.0)
        return say("Slow hello.")

    api = FakeApi([update("hey", update_id=1)])
    rig = lane_rig(FakeLane("Hey."), slow, api=api)
    t0 = time.monotonic()
    run_rig(rig)
    assert api.sent == [(OWNER, "Hey.")] and time.monotonic() - t0 < 1.5


# ---- the owner's voice comes from the whole profile page -------------------------------------------------------

LONG_LIFE = "\n".join(f"- Life fact number {i}: " + "x" * 80 for i in range(40))      # ~4000 chars
TALK = "## How they like to talk\n- Short and dry\n- Never starts with a greeting back"
PAGE = f"---\nid: profile\nupdated: 2026-10-01\n---\n# What eve knows about its owner\n\n## Life context\n{LONG_LIFE}\n\n{TALK}\n"


def test_the_talking_section_survives_a_profile_longer_than_the_voice_cut(tmp_path: Path) -> None:
    store = tmp_path / "memory"
    store.mkdir()
    cfg = RecallConfig(store=store, notes=tmp_path / "notes")
    records.records_dir(cfg).mkdir(parents=True, exist_ok=True)
    (records.records_dir(cfg) / "profile.md").write_text(PAGE, encoding="utf-8")
    assert "How they like to talk" not in records.profile_for_voice(cfg)          # the cut drops it: the bug
    assert records.talking_section(cfg) == TALK
    assert small_talk.voice_from_profile(records.talking_section(cfg)) == "Short and dry\nNever starts with a greeting back"
    records.hide(cfg, "greeting back")
    assert records.talking_section(cfg) == "## How they like to talk\n- Short and dry"   # hidden lines stay hidden
    (records.records_dir(cfg) / "profile.md").write_text("# No sections yet\n", encoding="utf-8")
    assert records.talking_section(cfg) == ""


class TalkingMemory(FakeMemory):
    def talking_section(self) -> str:
        return TALK


def test_the_lane_reads_the_voice_from_the_whole_page(tmp_path: Path) -> None:
    lane = FakeLane("Hey.")
    memory = TalkingMemory(tmp_path / "m", profile=PAGE)            # its voice profile, cut at 3000, lacks the section
    rig = lane_rig(lane, memory=memory, api=FakeApi([update("hi", update_id=1)]))
    run_rig(rig)
    assert lane.calls == [("hi", "Short and dry\nNever starts with a greeting back")]
