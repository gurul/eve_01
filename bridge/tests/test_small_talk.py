"""The small-talk lane (small_talk.py and its wiring in telegram.py): the code gate, the hand-off on every error, the
reply's parsing and checks, the transcript, the switch, and that a lane turn skips the routers and the brain while
a hand-off reaches them with the same words. No network: Haiku is a fake everywhere."""

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
from test_telegram import NOW, OWNER, FakeApi, FakeCreate, FakeMemory, Rig, call, run_rig, say, settle, update

from eve_bridge import chat_log, link_chat, openrouter, records, small_talk, telegram, toolset_router
from eve_bridge.recall import RecallConfig
from eve_bridge.small_talk import State
from eve_bridge.telegram import Inbound

HAIKU = "anthropic/claude-haiku-5.5"


# ---- fakes ------------------------------------------------------------------------------------------------------

class FakeLane:
    """small_talk.ask, scripted: answers ``reply`` (or hands off with ``reason``, or raises); records each call."""

    def __init__(self, reply: str = "Hey you.", reason: str = "", raises: Optional[Exception] = None) -> None:
        self.reply, self.reason, self.raises = reply, reason, raises
        self.calls: list[tuple[str, list[tuple[str, str]], str]] = []

    async def __call__(self, text: str, history: Any, voice: str = "") -> small_talk.Answer:
        self.calls.append((text, list(history), voice))
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


# ---- the gate (no model, no I/O) -----------------------------------------------------------------------------

def test_the_gate_passes_a_short_plain_message_with_nothing_waiting() -> None:
    assert small_talk.gate("hi", State()) == ""
    assert small_talk.gate("good morning eve, how are you doing", State()) == ""      # 7 words
    assert small_talk.gate("thanks!", State(), [("user", "send it"), ("eve", "Sent.")]) == ""


@pytest.mark.parametrize("text, state, reason", [
    ("", State(), small_talk.EMPTY),
    ("   ", State(), small_talk.EMPTY),
    ("hi", State(attachment=True), small_talk.ATTACHMENT),
    ("hi", State(tapped=True), small_talk.TAPPED),
    ("/start", State(), small_talk.COMMAND),
    ("one two three four five six seven eight nine", State(), small_talk.LONG),
    ("x" * 61, State(), small_talk.LONG),
    ("hi", State(owned=True), small_talk.OWNED),
    ("hi", State(awaiting_confirmation=True), small_talk.AWAITING_CONFIRMATION),
    ("hi", State(awaiting_answer=True), small_talk.AWAITING_ANSWER),
    ("hi", State(running_task=True), small_talk.RUNNING_TASK),
], ids=["empty", "blank", "attachment", "tapped", "command", "words", "chars", "owned", "confirmation", "answer",
        "task"])
def test_every_gate_condition_stands_the_lane_aside(text: str, state: State, reason: str) -> None:
    assert small_talk.gate(text, state) == reason
    # positive control: the same message with nothing set passes (when it is short and plain)
    if reason not in (small_talk.EMPTY, small_talk.COMMAND, small_talk.LONG):
        assert small_talk.gate(text, State()) == ""


def test_the_limits_are_inclusive() -> None:
    assert small_talk.gate("one two three four five six seven eight", State()) == ""
    assert small_talk.gate("x" * 60, State()) == ""


def test_eves_recent_question_makes_the_next_message_its_answer() -> None:
    asked = [("user", "book a table"), ("eve", "For how many people?")]
    now = 10_000.0
    assert small_talk.gate("hi", State(), asked, last_eve_at=now - 60, now=now) == small_talk.EVE_ASKED
    assert small_talk.gate("hi", State(), asked, last_eve_at=now - small_talk.QUESTION_WINDOW_S, now=now) \
        == small_talk.EVE_ASKED
    # older than the window: a new conversation
    assert small_talk.gate("hi", State(), asked, last_eve_at=now - small_talk.QUESTION_WINDOW_S - 1, now=now) == ""
    # no time known: the cautious reading
    assert small_talk.gate("hi", State(), asked) == small_talk.EVE_ASKED
    # a question mark with a closing quote, a bracket or an emoji after it
    for line in ('Want the "blue" one?"', "Which one (left or right?)", "Ready? 🙂"):
        assert small_talk.gate("hi", State(), [("eve", line)]) == small_talk.EVE_ASKED, line
    # eve's last line is not a question (an earlier one was): passes
    assert small_talk.gate("hi", State(), [("eve", "Which one?"), ("user", "a"), ("eve", "Done.")]) == ""
    # the owner's question is not eve's
    assert small_talk.gate("hi", State(), [("eve", "Morning."), ("user", "you there?")]) == ""


def test_code_owned_phrasings_are_seen_as_owned() -> None:
    for text in ("stop", "what time is it", "/spend", "claude on", "pause the music"):
        assert small_talk.code_owned(text), text
    for text in ("hi", "thanks eve", "how are you"):
        assert not small_talk.code_owned(text), text


# ---- the switch ---------------------------------------------------------------------------------------------

def test_the_switch(monkeypatch: pytest.MonkeyPatch) -> None:
    assert small_talk.SHIPPED is False                                   # flipped only after the blind bar
    assert small_talk.enabled({}) is False
    assert small_talk.enabled({"EVE_SMALL_TALK": "1"}) is True
    assert small_talk.enabled({"EVE_SMALL_TALK": "0"}) is False
    monkeypatch.setattr(small_talk, "SHIPPED", True)
    assert small_talk.enabled({}) is True
    assert small_talk.enabled({"EVE_SMALL_TALK": "0"}) is False         # off always wins
    assert small_talk.model_name({}) == HAIKU
    assert small_talk.model_name({"EVE_SMALL_TALK_MODEL": "openai/gpt-6-luna"}) == "openai/gpt-6-luna"


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


# ---- the reply: parsing and code's checks ---------------------------------------------------------------------

@pytest.mark.parametrize("content, parsed", [
    ('{"handoff": false, "reply": "Hey!", "reply_mentions_an_act": false}', (False, "Hey!", False)),
    ('{"handoff": true, "reply": "", "reply_mentions_an_act": false}', (True, "", False)),
    ('{"handoff": false, "reply": "Done.", "reply_mentions_an_act": true}', (False, "Done.", True)),
    ('```json\n{"handoff": false, "reply": " Hi ", "reply_mentions_an_act": false}\n```', (False, "Hi", False)),
    ('Sure: {"handoff": false, "reply": "Hi", "reply_mentions_an_act": false} ok', (False, "Hi", False)),
    ('{"handoff": false, "reply": "Hi"}', None),
    ('{"handoff": false, "reply": "Hi", "reply_mentions_an_act": "no"}', None),
    ('{"handoff": "false", "reply": "Hi", "reply_mentions_an_act": false}', None),
    ('{"handoff": 0, "reply": "Hi", "reply_mentions_an_act": false}', None),
    ('{"reply": "Hi", "reply_mentions_an_act": false}', None),
    ('{"handoff": false, "reply_mentions_an_act": false}', None),
    ('{"handoff": false, "reply": 3, "reply_mentions_an_act": false}', None),
    ("[1, 2]", None),
    ("Hi there!", None),
    ("{not json}", None),
    ("", None),
    (None, None),
    (42, None),
])
def test_parse_takes_only_the_schemas_object(content: Any, parsed: Any) -> None:
    assert small_talk.parse(content) == parsed


def lane_json(reply: str, handoff: bool = False, acts: bool = False) -> str:
    return json.dumps({"handoff": handoff, "reply": reply, "reply_mentions_an_act": acts})


@pytest.mark.parametrize("content, reason", [
    (lane_json("Let me check.", handoff=True), "handoff"),
    (lane_json("Anytime.", acts=True), "self_check"),
    (lane_json("   "), "empty"),
    (lane_json("a" * (small_talk.MAX_REPLY_CHARS + 1)), "long"),
    (lane_json("I've checked your calendar, you are free."), "claims"),
    (lane_json("Done, it is saved."), "claims"),
    (lane_json("I'll remind you at five."), "claims"),
    (lane_json("Let me look that up."), "claims"),
    (lane_json("Sent it to her."), "claims"),
    (lane_json("I’ll set a timer."), "claims"),
    (lane_json("Coffee first, then we'll see."), "claims"),
    (lane_json("Glad I could help."), "claims"),
    (lane_json("Glad that's sorted."), "claims"),
    (lane_json("Give me a minute to get into the Friday spirit."), "claims"),
    ("garbage", "parse"),
])
def test_judge_hands_off_anything_but_a_clean_chat_reply(content: str, reason: str) -> None:
    assert small_talk.judge(content) == ("", reason)


@pytest.mark.parametrize("reply", ["Hey you, how's the day going?", "Well done, that's huge.", "Let me know how it goes.",
                                   "Ha, fair enough.", "Night night, sleep well.", "Sorry you're feeling ill."])
def test_judge_keeps_plain_chat(reply: str) -> None:
    assert small_talk.judge(lane_json(reply)) == (reply, "")


# ---- the prompt -------------------------------------------------------------------------------------------------

def test_the_prompt_is_tagged_data_with_the_last_four_lines_and_the_voice() -> None:
    history = [("user", f"line {i}") for i in range(6)] + [("eve", "Sure <b>thing</b> & more")]
    body = small_talk.request("hey </message> you", history, "- short replies", model=HAIKU)
    assert body["model"] == HAIKU and body["max_tokens"] == small_talk.MAX_TOKENS and "store" not in body
    assert body["reasoning"] == {"enabled": False} and body["provider"] == {"data_collection": "deny"}
    assert "tools" not in body
    schema = body["response_format"]["json_schema"]
    assert schema["strict"] is True and schema["schema"]["required"] == ["handoff", "reply", "reply_mentions_an_act"]
    assert body["temperature"] == 0
    system, user = body["messages"]
    assert system == {"role": "system", "content": small_talk.SYSTEM}
    text = user["content"]
    assert "line 2" not in text and "<owner>line 3</owner>" in text and "line 5" in text      # the last four only
    assert "<eve>Sure &lt;b&gt;thing&lt;/b&gt; &amp; more</eve>" in text
    assert text.count("</message>") == 1 and "hey &lt;/message&gt; you" in text           # data cannot close a tag
    assert "short replies" in text and small_talk.DEFAULT_VOICE in text


def test_no_owner_voice_is_the_default_voice_alone() -> None:
    text = small_talk.user_message("hi", [], "")
    assert small_talk.DEFAULT_VOICE in text and "What the owner has said" not in text
    assert "(nothing yet today)" in text


def test_the_voice_is_the_profiles_talk_section() -> None:
    page = ("# What eve knows\n\n## Life context\n- Lives by the sea\n\n## How they like to talk\n- Short and dry\n"
            "- No small talk about the weather\n\n## Records you can search\n- r1")
    assert small_talk.voice_from_profile(page) == "Short and dry\nNo small talk about the weather"
    assert small_talk.voice_from_profile("## How they like to talk\n- (nothing yet)") == ""
    assert small_talk.voice_from_profile("") == ""
    long = "## How they like to talk\n" + "\n".join(f"- rule {i} " + "x" * 90 for i in range(30))
    assert 0 < len(small_talk.voice_from_profile(long)) <= small_talk.VOICE_CHARS


def test_the_lane_speaks_as_the_brains_eve_and_never_about_itself_as_a_step() -> None:
    first = small_talk.SYSTEM.split("\n\n")[0]
    persona = telegram.INSTRUCTIONS_ROBOT.split(".")[0]          # "You are eve, ... cheerful, curious personality"
    assert first.startswith(persona.replace("\n", " ")) or " ".join(first.split()).startswith(" ".join(persona.split()))
    assert "lights, the music, the TV" in " ".join(first.split())
    flat = " ".join(small_talk.SYSTEM.split()).lower()
    assert "first reader" not in flat and "fuller" not in flat
    assert "who or what you are, or what you can do" in flat and "never mention this step" in flat   # a hand-off


def test_the_system_prompt_names_no_tool() -> None:
    import re

    names = set(re.findall(r"\b[a-z]+_[a-z_]+\b", small_talk.SYSTEM))
    assert names == {"reply_mentions_an_act"}                          # the schema's own key, and no other name
    assert set(small_talk.RESPONSE_FORMAT["json_schema"]["schema"]["required"]) >= names


# ---- the call: every error is a hand-off ------------------------------------------------------------------------

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


def completion(content: Any, cost: float = 0.00002) -> dict[str, Any]:
    return {"model": HAIKU, "choices": [{"message": {"content": content}}],
            "usage": {"prompt_tokens": 300, "completion_tokens": 12, "cost": cost}}


def test_ask_answers_and_records_the_spend_as_small_talk(_spend_ledger_in_tmp: Path) -> None:
    fake = FakeCompletions(completion(lane_json("Hey.")))
    answer = asyncio.run(small_talk.ask("hi", [], client=client_of(fake), model="claude-haiku-5-5"))
    assert answer.answered and answer.reply == "Hey." and answer.model == HAIKU and answer.secs >= 0
    (body,) = fake.bodies
    assert body["model"] == HAIKU and "store" not in body and "reasoning" not in body and "provider" not in body
    assert body["extra_body"] == {"usage": {"include": True}, "reasoning": {"enabled": False},     # no thinking
                                  "provider": {"data_collection": "deny"}}
    rows = [json.loads(ln) for f in _spend_ledger_in_tmp.glob("*.jsonl") for ln in f.read_text().splitlines()]
    assert [(r["f"], r["m"], r["usd"]) for r in rows] == [("small_talk", HAIKU, 0.00002)]


@pytest.mark.parametrize("fake, reason", [
    (FakeCompletions(sleep=1.0), "timeout"),
    (FakeCompletions(raises=RuntimeError("502 from the provider")), "error"),
    (FakeCompletions(completion("Hi there!")), "parse"),
    (FakeCompletions(completion(None)), "parse"),
    (FakeCompletions({"error": "x"}), "parse"),
    (FakeCompletions("not a mapping"), "parse"),
    (FakeCompletions(completion(lane_json("", handoff=True))), "handoff"),
], ids=["timeout", "provider", "prose", "no-content", "no-choices", "not-a-dict", "haiku"])
def test_every_failure_of_the_call_is_a_hand_off(fake: FakeCompletions, reason: str) -> None:
    answer = asyncio.run(small_talk.ask("hi", [], client=client_of(fake), timeout_s=0.1))
    assert not answer.answered and answer.reason == reason and answer.reply == ""


class ScriptedCompletions:
    """Each call takes its scripted (seconds, result or exception), in order; records when each started."""

    def __init__(self, *script: tuple[float, Any]) -> None:
        self.script, self.starts = list(script), []

    async def create(self, **body: Any) -> Any:
        self.starts.append(time.perf_counter())
        secs, result = self.script.pop(0)
        await asyncio.sleep(secs)
        if isinstance(result, Exception):
            raise result
        return result


def test_a_slow_call_is_raced_by_a_second_one() -> None:
    fake = ScriptedCompletions((5.0, completion(lane_json("Slow."))),
                               (0.05, completion(lane_json("Fast."))))
    answer = asyncio.run(small_talk.ask("hi", [], client=client_of(fake), timeout_s=1.0, hedge_s=0.2))
    assert answer.reply == "Fast." and 0.2 <= answer.secs < 0.6 and len(fake.starts) == 2
    assert fake.starts[1] - fake.starts[0] >= 0.19                        # the second started at the hedge mark


def test_a_fast_call_is_never_hedged() -> None:
    fake = ScriptedCompletions((0.01, completion(lane_json("Hi."))))
    answer = asyncio.run(small_talk.ask("hi", [], client=client_of(fake), timeout_s=1.0, hedge_s=0.2))
    assert answer.reply == "Hi." and len(fake.starts) == 1


def test_a_failed_call_is_tried_once_more_then_handed_off() -> None:
    ok = ScriptedCompletions((0.0, RuntimeError("502")), (0.01, completion(lane_json("Hi."))))
    assert asyncio.run(small_talk.ask("hi", [], client=client_of(ok), timeout_s=1.0, hedge_s=0.5)).reply == "Hi."
    bad = ScriptedCompletions((0.0, RuntimeError("502")), (0.0, ValueError("bad gateway")))
    answer = asyncio.run(small_talk.ask("hi", [], client=client_of(bad), timeout_s=1.0, hedge_s=0.5))
    assert (answer.reason, answer.error) == ("error", "ValueError") and len(bad.starts) == 2


def test_both_slow_is_a_timeout_at_the_budget() -> None:
    fake = ScriptedCompletions((5.0, None), (5.0, None))
    answer = asyncio.run(small_talk.ask("hi", [], client=client_of(fake), timeout_s=0.4, hedge_s=0.1))
    assert answer.reason == "timeout" and 0.4 <= answer.secs < 0.8 and len(fake.starts) == 2


def test_no_key_is_a_hand_off(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(openrouter.KEY_ENV, raising=False)
    small_talk._CLIENT.clear()
    answer = asyncio.run(small_talk.ask("hi", []))
    assert not answer.answered and answer.reason == "no_key"


def test_a_cancelled_call_stays_cancelled() -> None:
    async def go() -> None:
        task = asyncio.ensure_future(small_talk.ask("hi", [], client=client_of(FakeCompletions(sleep=5.0))))
        await asyncio.sleep(0.01)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task

    asyncio.run(go())


# ---- in the chat: a lane turn skips everything; a hand-off changes nothing -------------------------------------

def test_a_lane_turn_answers_and_skips_the_routers_and_the_brain(tmp_path: Path,
                                                                caplog: pytest.LogCaptureFixture) -> None:
    lane, route = FakeLane("Hey you."), router()
    memory = FakeMemory(tmp_path / "m", profile="## How they like to talk\n- Dry humour")
    api = FakeApi([update("hi there", update_id=1)])
    rig = lane_rig(lane, memory=memory, api=api, toolset_mode="auto", toolset_route=route, grader=grader())
    with caplog.at_level(logging.INFO, logger="eve_bridge.telegram"):
        run_rig(rig)
    assert api.sent == [(OWNER, "Hey you.")]
    assert rig.create.requests == [] and route.calls == [] and rig.inlet._grader.calls == []
    assert lane.calls == [("hi there", [], "Dry humour")]
    # written as a normal turn: the transcript, and the history the next turn is shown
    assert [(ln["who"], ln["kind"], ln["text"]) for ln in memory.said() if ln["who"] != "system"] == [
        ("owner", "say", "hi there"), ("eve", "say", "Hey you.")]
    assert rig.closed == [[("user", "hi there"), ("eve", "Hey you.")]]
    lines = [r.getMessage() for r in caplog.records]
    assert any(m.startswith("telegram: turn answered in ") and m.endswith(f"(small talk, {HAIKU})") for m in lines)
    assert all("hi there" not in m and "Hey you" not in m for m in lines)          # never the words


def test_a_hand_off_reaches_the_brain_with_the_same_words_and_history(tmp_path: Path,
                                                                     caplog: pytest.LogCaptureFixture) -> None:
    def rig_for(lane: Any, where: str) -> tuple[Rig, Recorder, FakeMemory]:
        route = router()
        memory = FakeMemory(tmp_path / where)
        api = FakeApi([update("anything new with you", update_id=1)])
        return lane_rig(lane, say("Two meetings."), memory=memory, api=api, toolset_mode="auto",
                        toolset_route=route), route, memory

    lane = FakeLane(reason="handoff")
    handed, handed_route, handed_memory = rig_for(lane, "a")
    today, today_route, today_memory = rig_for(None, "b")
    with caplog.at_level(logging.INFO, logger="eve_bridge.telegram"):
        run_rig(handed)
    run_rig(today)
    assert len(lane.calls) == 1
    assert handed.api.sent == today.api.sent == [(OWNER, "Two meetings.")]
    (a,), (b,) = handed.create.requests, today.create.requests
    assert a["input"] == b["input"] and a["instructions"] == b["instructions"] and a["tools"] == b["tools"]
    assert handed_route.calls == today_route.calls == [("anything new with you", "")]
    assert [(ln["who"], ln["text"]) for ln in handed_memory.said()] == \
        [(ln["who"], ln["text"]) for ln in today_memory.said()]
    assert any(r.getMessage() == "telegram: small talk handed off (handoff) after 0.4 s" for r in caplog.records)


@pytest.mark.parametrize("lane", [FakeLane(raises=RuntimeError("boom")), FakeLane(reason="timeout"),
                                  FakeLane(reason="error")], ids=["raises", "timeout", "error"])
def test_a_broken_lane_costs_nothing(lane: FakeLane) -> None:
    api = FakeApi([update("hi", update_id=1)])
    rig = lane_rig(lane, say("Hey."), api=api)
    run_rig(rig)
    assert api.sent == [(OWNER, "Hey.")] and len(rig.create.requests) == 1 and len(lane.calls) == 1


def test_eves_question_sends_the_next_message_to_the_brain() -> None:
    lane = FakeLane()
    api = FakeApi([update("help me plan a dinner party for some friends this saturday", update_id=1)],
                  [update("pasta", update_id=2)])
    rig = lane_rig(lane, say("Sure, for how many people?"), say("Pasta for two, got it."), api=api)
    run_rig(rig)
    assert api.sent == [(OWNER, "Sure, for how many people?"), (OWNER, "Pasta for two, got it.")]
    assert lane.calls == []                       # the first is long; the second answers eve's question


def test_after_a_plain_reply_a_hi_is_the_lanes() -> None:
    """Positive control for the test above: the same chat, eve's reply not a question."""
    lane = FakeLane("Hey again.")
    api = FakeApi([update("help me plan a dinner party for some friends this saturday", update_id=1)],
                  [update("hi", update_id=2)])
    rig = lane_rig(lane, say("Pasta for six sounds right."), api=api)
    run_rig(rig)
    assert api.sent == [(OWNER, "Pasta for six sounds right."), (OWNER, "Hey again.")]
    assert lane.calls == [("hi", [("user", "help me plan a dinner party for some friends this saturday"),
                                  ("eve", "Pasta for six sounds right.")], "")]


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


def test_the_quick_answer_fallback_never_takes_the_lane(monkeypatch: pytest.MonkeyPatch) -> None:
    lane = FakeLane()
    api = FakeApi([update("what time is it", update_id=1)])
    rig = lane_rig(lane, say("Half past."), api=api)

    async def no_answer(ask: Any) -> None:
        return None

    monkeypatch.setattr(rig.inlet._quick, "answer", no_answer)
    run_rig(rig)
    assert api.sent == [(OWNER, "Half past.")] and lane.calls == []


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
            port, "POST", link_chat.TURN_PATH, {"text": "hi again", "client_id": "c-2", "grade": "think"},
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


# ---- the chat log: the lane writes in arrival order, and shows Haiku only the conversation going on now ----------

def log_rig(tmp_path: Path, lane: Any, *responses: Any, memory: Optional[FakeMemory] = None,
            **kw: Any) -> tuple[Rig, chat_log.ChatLog]:
    log = chat_log.ChatLog(tmp_path / "chat" / "log.json", wall=lambda: NOW)
    return lane_rig(lane, *responses, memory=memory, chat_log=log, **kw), log


def real_lane(fake: "FakeCompletions") -> Any:
    """The production call (prompt, schema, checks) over a fake client: the prompt it sends is in fake.bodies."""
    async def lane(text: str, history: Any, voice: str = "") -> small_talk.Answer:
        return await small_talk.ask(text, history, voice, client=client_of(fake), record_spend=False)

    return lane


def said(log: chat_log.ChatLog) -> list[tuple[str, str]]:
    return [(ln.who, ln.text) for ln in log.lines(OWNER) if ln.who in ("user", "eve")]


@pytest.mark.parametrize("quiet_mins, shown", [(11, False), (5, True)], ids=["after-a-quiet-spell", "same-talk"])
def test_after_a_quiet_spell_haiku_is_shown_no_earlier_lines(tmp_path: Path, quiet_mins: int, shown: bool) -> None:
    fake = FakeCompletions(completion(json.dumps({"handoff": False, "reply": "Morning!",
                                                  "reply_mentions_an_act": False})))
    api = FakeApi([update("hi", update_id=1)])
    rig, log = log_rig(tmp_path, real_lane(fake), api=api)
    log.append(OWNER, "user", "the trip to the coast is on", at=NOW - quiet_mins * 60 - 30)
    log.append(OWNER, "eve", "The coast sounds lovely.", at=NOW - quiet_mins * 60)
    run_rig(rig)
    assert api.sent == [(OWNER, "Morning!")]
    prompt = fake.bodies[0]["messages"][1]["content"]
    assert ("The coast sounds lovely." in prompt) is shown            # the positive control shows it
    assert ("(nothing yet today)" in prompt) is not shown


def test_after_a_restart_haiku_is_shown_no_earlier_lines(tmp_path: Path) -> None:
    fake = FakeCompletions(completion(json.dumps({"handoff": False, "reply": "Hey.", "reply_mentions_an_act": False})))
    api = FakeApi([update("hi", update_id=1)])
    rig, log = log_rig(tmp_path, real_lane(fake), api=api)
    log.append(OWNER, "user", "the trip to the coast is on", at=NOW - 60)
    log.append(OWNER, "eve", "The coast sounds lovely.", at=NOW - 30)
    for ln in log.lines(OWNER):
        ln.boot = "an-earlier-run"
    run_rig(rig)
    assert "(nothing yet today)" in fake.bodies[0]["messages"][1]["content"]


def test_eves_question_before_a_quiet_spell_still_keeps_the_message_the_brains(tmp_path: Path) -> None:
    """The conversation Haiku would see is over, but eve asked within QUESTION_WINDOW_S: the gate still sees it."""
    lane = FakeLane()
    api = FakeApi([update("yes", update_id=1)])
    rig, log = log_rig(tmp_path, lane, say("Booked the usual."), api=api)
    log.append(OWNER, "eve", "Want me to book the usual table?", at=NOW - 15 * 60)
    run_rig(rig)
    assert lane.calls == [] and api.sent == [(OWNER, "Booked the usual.")]


class InterleavedLane(FakeLane):
    """A FakeLane during whose call another message is handled by a code path (lights), as _handle does at once."""

    def __init__(self, *a: Any, **kw: Any) -> None:
        super().__init__(*a, **kw)
        self.inlet: Any = None

    async def __call__(self, text: str, history: Any, voice: str = "") -> small_talk.Answer:
        self.inlet._note("user", "lights off", "command")
        self.inlet._note("eve", "Lights are off.")
        return await super().__call__(text, history, voice)


def test_a_lane_reply_keeps_the_owners_line_where_it_arrived(tmp_path: Path) -> None:
    lane = InterleavedLane("Hey you.")
    api = FakeApi([update("hi", update_id=1)])
    rig, log = log_rig(tmp_path, lane, api=api)
    lane.inlet = rig.inlet
    run_rig(rig)
    assert said(log) == [("user", "hi"), ("eve", "Lights are off."), ("eve", "Hey you.")]


def test_a_hand_off_after_other_lines_shows_the_brain_its_message_last_and_writes_it_once(tmp_path: Path) -> None:
    lane = InterleavedLane(reason="handoff")
    memory = FakeMemory(tmp_path / "m")
    api = FakeApi([update("hi", update_id=1)])
    rig, log = log_rig(tmp_path, lane, say("Hello."), memory=memory, api=api)
    lane.inlet = rig.inlet
    run_rig(rig)
    assert api.sent == [(OWNER, "Hello.")] and len(lane.calls) == 1
    (request,) = rig.create.requests
    texts = [c["text"] for item in request["input"] if item.get("type") == "message" for c in item["content"]]
    assert texts[-1] == "hi" and "Lights are off." not in texts      # the history ends at the message
    assert said(log) == [("user", "hi"), ("eve", "Lights are off."), ("eve", "Hello.")]
    owner_lines = [ln["text"] for ln in memory.said() if ln["who"] == "owner"]
    assert owner_lines.count("hi") == 1


@pytest.mark.parametrize("with_log", [True, False], ids=["chat-log", "ram-turns"])
def test_a_hand_off_shows_the_brain_what_it_would_have_seen(tmp_path: Path, with_log: bool) -> None:
    """The owner's line written by the lane before its call changes nothing the brain is shown or writes."""
    def rig_for(lane: Any, where: str) -> tuple[Rig, FakeMemory]:
        memory = FakeMemory(tmp_path / where / "m")
        api = FakeApi([update("anything new with you", update_id=1)])
        if with_log:
            rig, _ = log_rig(tmp_path / where, lane, say("Two meetings."), memory=memory, api=api)
        else:
            rig = lane_rig(lane, say("Two meetings."), memory=memory, api=api)
        rig.inlet._note("user", "help me plan a dinner party for some friends this saturday")    # an earlier turn
        rig.inlet._note("eve", "Pasta for six sounds right.")
        return rig, memory

    lane = FakeLane(reason="handoff")
    handed, handed_memory = rig_for(lane, "a")
    today, today_memory = rig_for(None, "b")
    run_rig(handed)
    run_rig(today)
    assert len(lane.calls) == 1
    assert handed.api.sent == today.api.sent
    assert [r["input"] for r in handed.create.requests] == [r["input"] for r in today.create.requests]
    assert [r["instructions"] for r in handed.create.requests] == [r["instructions"] for r in today.create.requests]
    assert [(ln["who"], ln["kind"], ln["text"]) for ln in handed_memory.said()] == \
        [(ln["who"], ln["kind"], ln["text"]) for ln in today_memory.said()]


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
    assert lane.calls == [("hi", [], "Short and dry\nNever starts with a greeting back")]
