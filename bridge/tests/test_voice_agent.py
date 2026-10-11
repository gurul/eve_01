"""voice_agent.py with a scripted Live connection, a fake speaker and a fake
computer agent: session setup, delegated tool dispatch, board-state mirroring,
barge-in, ask_user relay, idle timeout and the session cap.

The Live protocol has no transcript-done event, so a spoken turn closes on a
silence gap. `_say()` builds a turn and the tests advance the fake clock past
TURN_GAP_SECS to close it."""

from __future__ import annotations

import asyncio
import base64
import json
import threading
import time
from datetime import datetime, timedelta, timezone

import pytest

from eve_bridge import system_context, tool_gate, voice_agent, websearch
from eve_bridge.caption_pager import PagerConfig, caption_instructions
from eve_bridge.computer_agent import AgentEvent
from eve_bridge.transcripts import TranscriptConfig, Transcripts, render_line
from eve_bridge.voice_agent import (
    BACKEND_INSTRUCTIONS,
    BACKEND_MEMORY_RULES,
    FAREWELL_MAX_SECS,
    FAREWELL_QUIET_SECS,
    INSTRUCTIONS,
    MEMORY_TIMEOUT_LINE,
    TOOLS,
    TURN_GAP_SECS,
    VOICE_TODAY_HEADER,
    VoiceConfig,
    VoiceSession,
    configured,
    memory_block,
    session_config,
)

# ---- fakes ---------------------------------------------------------------------------

class _Recorder:
    """Records every client command under its dotted Live event name."""

    def __init__(self, conn: "FakeConnection", kind: str) -> None:
        self.conn, self.kind = conn, kind

    async def start(self, **kw):    # session.start
        self.conn.sent.append((f"{self.kind}.start", kw))

    async def update(self, **kw):   # session.update
        self.conn.sent.append((f"{self.kind}.update", kw))

    async def create(self, **kw):   # response.create / response.item.create
        self.conn.sent.append((f"{self.kind}.create", kw))

    async def append(self, **kw):   # session.input_audio.append / session.commentary.append
        self.conn.sent.append((f"{self.kind}.append", kw))

    async def mute(self, **kw):
        self.conn.sent.append((f"{self.kind}.mute", kw))

    async def unmute(self, **kw):
        self.conn.sent.append((f"{self.kind}.unmute", kw))

    async def close(self, **kw):
        self.conn.sent.append((f"{self.kind}.close", kw))


class FakeConnection:
    """Yields scripted events; after each event the test may push more via feed()."""

    def __init__(self, events: list[dict] | None = None) -> None:
        self.sent: list[tuple[str, dict]] = []
        self.queue: asyncio.Queue[dict | None] = asyncio.Queue()
        self.queue.put_nowait({"type": "session.started"})   # run() waits for this
        for e in events or []:
            self.queue.put_nowait(e)
        self.session = _Recorder(self, "session")
        self.session.input_audio = _Recorder(self, "session.input_audio")
        self.session.commentary = _Recorder(self, "session.commentary")
        self.session.thinking = _Recorder(self, "session.thinking")
        self.response = _Recorder(self, "response")
        self.response.item = _Recorder(self, "response.item")

    def feed(self, *events: dict | None) -> None:
        for e in events:
            self.queue.put_nowait(e)

    def __aiter__(self):
        return self

    async def __anext__(self):
        e = await self.queue.get()
        if e is None:
            raise StopAsyncIteration
        return e

    def kinds(self) -> list[str]:
        return [k for k, _ in self.sent]

    def tool_outputs(self) -> list[dict]:
        return [json.loads(kw["item"]["output"]) for k, kw in self.sent
                if k == "response.item.create" and kw["item"]["type"] == "function_call_output"]

    def user_messages(self) -> list[str]:
        return [kw["item"]["content"][0]["text"] for k, kw in self.sent
                if k == "response.item.create" and kw["item"]["type"] == "message"]

    def commentary(self) -> list[str]:
        """What the voice was handed to say (session.commentary.append), greeting first."""
        return [kw["content"] for k, kw in self.sent if k == "session.commentary.append"]


class FakeSpeaker:
    def __init__(self) -> None:
        self.played = b""
        self.stops = 0
        self._busy = False

    def play(self, pcm: bytes) -> None:
        self.played += pcm

    def stop(self) -> None:
        self.stops += 1
        self._busy = False

    @property
    def busy(self) -> bool:
        return self._busy


class FakeAgent:
    """Runs until released; reports events through on_event like the real one."""

    def __init__(self, on_event, ask_user, final: str = "Mail is open.", ask: str | None = None) -> None:
        self.on_event, self.ask_user, self.final_text, self.ask_q = on_event, ask_user, final, ask
        self.running = False
        self.steers: list[str] = []
        self.cancelled = False
        self.release = asyncio.Event()
        self.final: str | None = None

    async def run(self, goal: str) -> str:
        self.running = True
        self.on_event(AgentEvent("started", goal))
        self.on_event(AgentEvent("exec", "display(...)", 1))
        answer = None
        if self.ask_q:
            self.on_event(AgentEvent("ask", self.ask_q, 1))
            answer = await self.ask_user(self.ask_q)
        await self.release.wait()
        self.running = False
        if self.cancelled:
            self.on_event(AgentEvent("cancelled", "Stopped."))
            self.final = "Stopped."
            return self.final
        self.final = f"{self.final_text} (answer={answer})" if answer is not None else self.final_text
        self.on_event(AgentEvent("final", self.final, 2))
        return self.final

    def steer(self, text: str) -> bool:
        if not self.running:
            return False
        self.steers.append(text)
        return True

    def cancel(self, reason: str = "") -> None:
        self.cancelled = True
        self.reason = reason
        self.release.set()

    def status(self) -> dict:
        return {"running": self.running, "turn": 1, "goal": "g", "last": "looking", "final": self.final}


def _tool_call(name: str, call_id: str = "c1", **args) -> dict:
    """A backend function call, nested in a Live response.event."""
    return {"type": "response.event", "event": {
        "type": "response.output_item.done",
        "item": {"type": "function_call", "name": name, "call_id": call_id,
                 "arguments": json.dumps(args)}}}


def _delegated(did: str = "d1") -> dict:
    return {"type": "session.delegation.created",
            "delegation": {"id": did, "target": "responses", "type": "delegation"}}


def _backend(kind: str) -> dict:
    return {"type": "response.event", "event": {"type": kind}}


def _done() -> dict:
    return _backend("response.completed")


def _heard(text: str) -> dict:
    return {"type": "session.input_transcript.delta", "delta": text}


def _spoke(text: str) -> dict:
    return {"type": "session.output_transcript.delta", "delta": text}


# The watchdog looks every WATCHDOG_SECS in these tests (0.5 s in production); a wait of WATCHDOG_WAIT lets it
# look at least twice. The decisions ride the fake clock, so only the number of looks matters, not real time.
WATCHDOG_SECS = 0.02
WATCHDOG_WAIT = 0.05


@pytest.fixture(autouse=True)
def _short_route_delays(monkeypatch: pytest.MonkeyPatch):
    """The voice waits 0.6 s for itself to delegate a head or lesson request (voice_agent.py); the code reads the
    module global at call time, so the tests wait 0.05 s instead of the seven 0.8 s sleeps they had (2026-10-09)."""
    monkeypatch.setattr(voice_agent, "LOOK_ROUTE_DELAY_SECS", 0.05)
    monkeypatch.setattr(voice_agent, "LESSON_ROUTE_DELAY_SECS", 0.05)


def _session(conn: FakeConnection, agents: list[FakeAgent], clock: dict | None = None, **kw):
    states: list[str] = []
    clock = clock if clock is not None else {"now": 0.0}

    def factory(on_event, ask_user):
        a = agents[0] if len(agents) == 1 else agents.pop(0)
        a.on_event, a.ask_user = on_event, ask_user
        return a

    mic: asyncio.Queue[bytes] = asyncio.Queue()
    kw.setdefault("watchdog_secs", WATCHDOG_SECS)
    s = VoiceSession(conn, mic, FakeSpeaker(), factory, states.append,
                     config=kw.pop("config", VoiceConfig(idle_timeout_secs=20.0, max_session_secs=600.0,
                                                          output="audio")),
                     clock=lambda: clock["now"], **kw)
    return s, states, mic


async def _eventually(predicate, timeout=2.0):
    """Wait for a state transition, independent of Windows timer granularity."""
    async with asyncio.timeout(timeout):
        while not predicate():
            await asyncio.sleep(0.001)


# ---- config -------------------------------------------------------------------------------

def test_configured_defaults_and_env() -> None:
    c = configured({})
    assert c.model == "gpt-live-1" and c.voice == "marin" and c.idle_timeout_secs == 20.0
    assert c.backend_model == "gpt-6-astra" and c.backend_effort == "low"
    c = configured({"EVE_LIVE_MODEL": "gpt-live-1-preview", "EVE_VOICE_NAME": "cedar",
                    "EVE_LIVE_BACKEND_MODEL": "gpt-5-mini", "EVE_LIVE_BACKEND_EFFORT": "minimal",
                    "EVE_VOICE_IDLE_SECS": "45"})
    assert c.model == "gpt-live-1-preview" and c.voice == "cedar" and c.idle_timeout_secs == 45.0
    assert c.backend_model == "gpt-5-mini" and c.backend_effort == "minimal"
    assert configured({"EVE_LIVE_BACKEND_EFFORT": "turbo"}).backend_effort == "low"
    assert configured({"EVE_VOICE_IDLE_SECS": "1"}).idle_timeout_secs == 5.0     # floor


def test_session_config_shape() -> None:
    s = session_config(VoiceConfig(output="audio"))
    assert s["model"] == "gpt-live-1"
    # gpt-live-1 is full duplex and always speaks: no modality list, no turn detection
    assert "output_modalities" not in s and "tools" not in s and "type" not in s
    assert s["audio"]["format"] == {"type": "audio/pcm", "rate": 24000}
    assert s["audio"]["output"]["voice"] == "marin"
    d = s["delegation"]
    assert d["type"] == "responses" and d["responses"]["model"] == "gpt-6-astra"
    assert d["responses"]["reasoning"] == {"effort": "low"}
    assert [t.get("name", t["type"]) for t in d["responses"]["tools"]] == [
        "lesson", "start_task", "steer_task", "stop_task", "task_status", "answer_question",
        "go_explore", "end_conversation", "look", "move_head", "look_around", "find",
        "take_photo", "set_sound", "think_hard", "web_search"]
    assert all(t["type"] == "function" for t in TOOLS)
    # web search: the hosted Responses tool without an OpenRouter key, on by default and switchable off
    assert d["responses"]["tools"][-1] == {"type": "web_search"}
    off = session_config(VoiceConfig(output="audio", web_search=False))
    assert all(t["type"] == "function" for t in off["delegation"]["responses"]["tools"])


def test_session_config_offers_web_search_through_openrouter_or_the_hosted_tool() -> None:
    from eve_bridge import websearch

    exa = session_config(VoiceConfig(output="audio", search=websearch.SearchConfig(engine="openrouter-exa")))
    tools = exa["delegation"]["responses"]["tools"]
    assert tools[-1] == websearch.WEB_SEARCH_TOOL and tools[-1]["name"] == "web_search"     # Exa via OpenRouter
    hosted = session_config(VoiceConfig(output="audio", search=websearch.SearchConfig(engine="openai")))
    assert hosted["delegation"]["responses"]["tools"][-1] == {"type": "web_search"}
    assert configured({"OPENROUTER_API_KEY": "r"}).search.engine == "openrouter-perplexity"   # Perplexity by default
    assert configured({}).search.engine == "off"                                              # no key: no search


# ---- the conversation ------------------------------------------------------------------------

def test_setup_greeting_mic_pump_and_goodbye() -> None:
    conn = FakeConnection()
    s, states, mic = _session(conn, [FakeAgent(None, None)])

    async def go():
        mic.put_nowait(b"\x01\x02" * 100)
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)                        # let the mic pump forward the block
        conn.feed(_tool_call("end_conversation"), None)
        await task
    asyncio.run(go())
    assert conn.kinds()[:2] == ["session.start", "session.commentary.append"]
    assert conn.sent[0][1]["session"]["model"] == "gpt-live-1"
    assert "Greet" in conn.sent[1][1]["content"]
    appended = [kw["audio"] for k, kw in conn.sent if k == "session.input_audio.append"]
    assert appended and base64.b64decode(appended[0]) == b"\x01\x02" * 100
    assert conn.tool_outputs() == [{"ok": True}]
    # end_conversation does not ask for another response
    assert conn.kinds()[-1] == "response.item.create"
    assert states[0] == "wake" and states[-1] == "idle"


def test_audio_events_drive_speaker_and_states() -> None:
    pcm = base64.b64encode(b"\x00\x10" * 50).decode()
    conn = FakeConnection([
        _delegated(),
        {"type": "session.output_audio.delta", "delta": pcm},
        _spoke("Yeah?"),
        _done(),
        _heard("wait"),
        _tool_call("end_conversation"), None,
    ])
    s, states, _ = _session(conn, [FakeAgent(None, None)])
    asyncio.run(s.run())
    assert s.speaker.played == b"\x00\x10" * 50
    assert s.transcript == ["Yeah?"]
    assert states == ["wake", "thinking", "speaking", "listening", "idle"]
    assert s.speaker.stops >= 1          # speech_started stops playback (barge-in)


def test_start_task_is_refused_while_a_texted_task_has_the_mac() -> None:
    """telegram.py can own a running task too: two agents never share the mouse (mac_busy)."""
    busy = {"on": True}
    s, _, _ = _session(FakeConnection([]), [FakeAgent(None, None)], mac_busy=lambda: busy["on"])

    async def go():
        refused = s._start_task("open mail")
        assert refused["ok"] is False and "texted" in refused["reason"] and not s.task_running
        busy["on"] = False                               # the control: the same call starts once it is free
        assert s._start_task("open mail")["ok"] is True and s.task_running
        s._agent_task.cancel()
        await asyncio.gather(s._agent_task, *s._slow_tasks, return_exceptions=True)
    asyncio.run(go())


def test_start_task_runs_agent_and_reports_result() -> None:
    agent = FakeAgent(None, None, final="Your mail is open.")
    clock = {"now": 0.0}
    conn = FakeConnection([_tool_call("start_task", goal="open mail")])
    s, states, _ = _session(conn, [agent], clock=clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        assert s.task_running and "working" in states
        conn.feed(_done())            # after "On it."
        await asyncio.sleep(0.01)
        assert states[-1] == "working"
        agent.release.set()
        await asyncio.sleep(0.01)
        # the result goes to the VOICE to say, not into the backend (2026-09-10: "Okay, waiting.")
        assert conn.user_messages() == []
        assert "Your mail is open." in conn.commentary()[-1]
        conn.feed(_delegated(), _spoke("Your mail is open."), _done())
        await asyncio.sleep(0.05)
        clock["now"] += 2.0                             # the result has been said and gone quiet
        await asyncio.sleep(0.2)
        assert not s._ended.is_set()                    # ... and eve keeps listening (2026-09-10)
        assert states[-1] == "listening"
        conn.feed(_tool_call("end_conversation", "c9"))   # "bye eve" is what ends it
        await asyncio.sleep(0.05)
        assert s._ended.is_set()
        conn.feed(None)
        await task
    asyncio.run(go())
    assert conn.tool_outputs()[0] == {"ok": True, "goal": "open mail"}
    assert "done" in states and states[-1] == "idle"
    assert s.tool_calls[0] == ("start_task", {"goal": "open mail"})


def test_steer_stop_and_status_tools() -> None:
    agent = FakeAgent(None, None)
    conn = FakeConnection([_tool_call("steer_task", "c0", text="nothing yet"), _tool_call("start_task", "c1", goal="g")])
    s, states, _ = _session(conn, [agent])

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        conn.feed(_tool_call("steer_task", "c2", text="use safari"), _tool_call("task_status", "c3"),
                  _tool_call("stop_task", "c4"))
        await asyncio.sleep(0.02)
        # task_status answers with the task's own words (v3 safety review): the taint stays, but stopping a task
        # is routine (owner, 2026-10-09), so the stop runs with no spoken question
        conn.feed(_tool_call("stop_task", "c5"), _tool_call("end_conversation", "c6"), None)
        await task
    asyncio.run(go())
    outs = conn.tool_outputs()
    assert outs[0] == {"ok": False, "reason": "no task is running"}
    assert outs[1] == {"ok": True, "goal": "g"}
    assert outs[2] == {"ok": True} and agent.steers == ["use safari"]
    assert outs[3]["running"] is True and outs[3]["last"] == "looking"
    assert outs[4] == {"ok": True} and agent.cancelled and s._taint == tool_gate.TASK_RESULT
    assert outs[5]["ok"] is False
    assert conn.user_messages() == [] and any("Stopped." in c for c in conn.commentary())


def test_second_start_while_running_is_refused_and_agent_can_be_disabled() -> None:
    agent = FakeAgent(None, None)
    conn = FakeConnection([_tool_call("start_task", "c1", goal="a"), _tool_call("start_task", "c2", goal="b")])
    s, _, _ = _session(conn, [agent])

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        agent.release.set()
        conn.feed(_tool_call("end_conversation", "c3"), None)
        await task
    asyncio.run(go())
    assert conn.tool_outputs()[1]["reason"].startswith("a task is already running")

    conn2 = FakeConnection([_tool_call("start_task", "c1", goal="a"), _tool_call("end_conversation", "c2"), None])
    s2, _, _ = _session(conn2, [FakeAgent(None, None)], agent_enabled=False)
    asyncio.run(s2.run())
    assert "disabled" in conn2.tool_outputs()[0]["reason"]


def test_ask_user_is_spoken_and_answered_through_the_tool() -> None:
    agent = FakeAgent(None, None, final="Sent.", ask="Send the email to Sam?")
    conn = FakeConnection([_tool_call("start_task", "c1", goal="email sam")])
    s, states, _ = _session(conn, [agent])

    async def go():
        task = asyncio.create_task(s.run())
        await _eventually(lambda: bool(conn.user_messages()))
        assert conn.user_messages() == ["[task question] Send the email to Sam?"]
        assert states[-1] == "asking"
        # the owner's own yes, then the voice passes it on: the task's question tainted the conversation (v3
        # safety review), and an answer that is the owner's bare word is not asked about twice
        conn.feed(_heard("yes"))
        await asyncio.sleep(0.02)
        conn.feed(_tool_call("answer_question", "c2", answer="yes"))
        await _eventually(lambda: len(conn.tool_outputs()) >= 2)
        agent.release.set()
        await _eventually(lambda: any("Sent. (answer=yes)" in c for c in conn.commentary()))
        conn.feed(_tool_call("end_conversation", "c3"), None)
        await task
    asyncio.run(go())
    assert conn.tool_outputs()[1] == {"ok": True}
    # the backend got the question as context, the voice asked it and later said the result
    assert conn.user_messages() == ["[task question] Send the email to Sam?"]
    assert any("Send the email to Sam?" in c for c in conn.commentary())
    assert "Sent. (answer=yes)" in conn.commentary()[-1]


def test_answer_without_a_pending_question_is_refused() -> None:
    conn = FakeConnection([_tool_call("answer_question", "c1", answer="yes"), _tool_call("end_conversation", "c2"), None])
    s, _, _ = _session(conn, [FakeAgent(None, None)])
    asyncio.run(s.run())
    assert conn.tool_outputs()[0] == {"ok": False, "reason": "no question is pending"}


def test_idle_timeout_closes_the_session() -> None:
    clock = {"now": 0.0}
    conn = FakeConnection([_done()])
    s, states, _ = _session(conn, [FakeAgent(None, None)], clock=clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        clock["now"] = 25.0                              # > idle_timeout 20 s, no task
        await asyncio.sleep(WATCHDOG_WAIT)                         # one watchdog tick
        assert s._ended.is_set()
        conn.feed(None)
        await task
    asyncio.run(go())
    assert states[-1] == "idle"


def test_running_task_defers_idle_timeout_but_not_the_session_cap() -> None:
    clock = {"now": 0.0}
    agent = FakeAgent(None, None)
    conn = FakeConnection([_tool_call("start_task", "c1", goal="g")])
    s, _, _ = _session(conn, [agent], clock=clock, config=VoiceConfig(idle_timeout_secs=20.0, max_session_secs=100.0))

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        clock["now"] = 50.0
        await asyncio.sleep(WATCHDOG_WAIT)
        assert not s._ended.is_set()                     # task running: idle timeout does not apply
        clock["now"] = 101.0
        await asyncio.sleep(WATCHDOG_WAIT)
        assert s._ended.is_set()                         # session cap does
        conn.feed(None)
        await task
    asyncio.run(go())
    assert agent.cancelled                               # the cap cancels a running task


def test_unknown_tool_and_error_events_are_harmless() -> None:
    conn = FakeConnection([{"type": "error", "error": {"message": "boom"}}, _tool_call("teleport", "c1"),
                           _tool_call("end_conversation", "c2"), None])
    s, _, _ = _session(conn, [FakeAgent(None, None)])
    asyncio.run(s.run())
    assert conn.tool_outputs()[0] == {"ok": False, "reason": "unknown tool teleport"}


def test_tool_result_defers_response_create_until_response_done() -> None:
    """A backend function call is reported before its response completes, and the
    API rejects a second response.create while one is active (bench 2026-09-06 09:01)."""
    conn = FakeConnection([_delegated(), _tool_call("task_status", "c1")])
    s, _, _ = _session(conn, [FakeAgent(None, None)])

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        creates_before = sum(1 for k, _ in conn.sent if k == "response.create")
        assert creates_before == 0                       # the tool result waited; the greeting is commentary
        conn.feed(_done())
        await asyncio.sleep(0.01)
        creates_after = sum(1 for k, _ in conn.sent if k == "response.create")
        assert creates_after == 1                        # created once the response completed
        conn.feed(_tool_call("end_conversation", "c2"), None)
        await task
    asyncio.run(go())


def test_mic_is_muted_while_eve_speaks_and_for_a_tail() -> None:
    clock = {"now": 0.0}
    conn = FakeConnection()
    s, _, mic = _session(conn, [FakeAgent(None, None)], clock=clock)
    s.speaker._busy = True

    async def go():
        task = asyncio.create_task(s.run())
        mic.put_nowait(b"\x01" * 100)                     # while speaking: dropped
        await asyncio.sleep(0.01)
        s.speaker._busy = False
        clock["now"] = 0.2                                # inside the 0.4 s tail: dropped
        mic.put_nowait(b"\x02" * 100)
        await asyncio.sleep(0.01)
        clock["now"] = 1.0                                # after the tail: forwarded
        mic.put_nowait(b"\x03" * 100)
        await asyncio.sleep(0.01)
        conn.feed(_tool_call("end_conversation"), None)
        await task
    asyncio.run(go())
    appended = [base64.b64decode(kw["audio"]) for k, kw in conn.sent if k == "session.input_audio.append"]
    assert appended == [b"\x03" * 100]


def test_conversation_stays_open_after_a_task_until_goodbye_or_idle() -> None:
    """Owner request 2026-09-10: a finished task does not end the conversation. A goodbye
    does (previous test), and so does the idle timeout when nothing more is said."""
    agent = FakeAgent(None, None, final="Done.")
    clock = {"now": 0.0}
    conn = FakeConnection([_tool_call("start_task", "c1", goal="g")])
    s, states, _ = _session(conn, [agent], clock=clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        agent.release.set()
        await asyncio.sleep(0.01)
        conn.feed(_delegated(), _spoke("On it."), _done())
        await asyncio.sleep(0.05)
        clock["now"] = 2.0
        await asyncio.sleep(0.2)
        conn.feed(_spoke("Done."))                     # the result, spoken
        await asyncio.sleep(0.05)
        clock["now"] = 4.0
        await asyncio.sleep(0.2)
        assert not s._ended.is_set()                   # still listening after the result
        clock["now"] = 30.0                            # past the 20 s idle timeout
        await asyncio.sleep(WATCHDOG_WAIT)                       # the watchdog looks every WATCHDOG_SECS
        assert s._ended.is_set()
        conn.feed(None)
        await task
    asyncio.run(go())
    assert states[-1] == "idle"
    assert s.transcript == ["On it.", "Done."]
    creates = [k for k, _ in conn.sent if k == "response.create"]
    assert len(creates) == 1          # "On it" only: the result is commentary, and nothing follows it


CAPTIONS = VoiceConfig(idle_timeout_secs=20.0, max_session_secs=600.0, output="captions")


def _reply(*deltas: str) -> list[dict]:
    text = "".join(deltas)
    del text
    return [_delegated(), *(_spoke(d) for d in deltas), _done()]


def _captions_session(conn: FakeConnection, clock: dict, **kw):
    captions: list[dict] = []
    s, states, mic = _session(conn, [FakeAgent(None, None)], clock=clock, on_caption=captions.append,
                              caption_tick_secs=0.001, turn_gap_secs=0.2,
                              config=kw.pop("config", CAPTIONS), **kw)
    return s, states, captions


async def _quiet(clock: dict, at: float = 0.3) -> None:
    """Live sends no transcript-done event: a reply is final once it goes quiet."""
    clock["now"] = at
    await asyncio.sleep(0.02)


def test_captions_mode_streams_text_to_the_robot_and_plays_nothing() -> None:
    conn = FakeConnection(_reply("Ten past ", "three."))
    clock = {"now": 0.0}
    s, states, captions = _captions_session(conn, clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        await _quiet(clock)
        # page 0 goes up at the first whole word (hold 2 s while it fills), then the final
        # text refills it as the last page: 15 chars -> clamp(15/12 + 0.8 + 3, 6, 10) = 6 s,
        # less the 0.3 s the turn took to go quiet
        assert captions == [
            {"cmd": "caption", "page": 0, "of": 0, "lines": ["Ten past"], "hold_ms": 2000, "chirp": True, "final": False},
            {"cmd": "caption", "page": 0, "of": 1, "lines": ["Ten past three."], "hold_ms": 5700, "chirp": False,
             "final": True},
        ]
        assert states[-1] == "speaking"                 # the phase holds while the page is read
        clock["now"] = 6.4
        await asyncio.sleep(0.01)
        assert captions[-1] == {"cmd": "caption", "clear": True}
        assert states[-1] == "listening"
        conn.feed(_tool_call("end_conversation"), None)
        await task
    asyncio.run(go())
    assert s.speaker.played == b"" and s.transcript == ["Ten past three."]
    assert states[-1] == "idle"


def test_state_stays_speaking_until_the_last_page_is_held() -> None:
    conn = FakeConnection(_reply("Ten past ", "three."))
    clock = {"now": 0.0}
    s, states, captions = _captions_session(conn, clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        await _quiet(clock)
        assert states[-1] == "speaking" and captions[-1]["final"] is True
        clock["now"] = 3.0
        await asyncio.sleep(0.01)
        assert states[-1] == "speaking"                 # 3 s in: still on the page
        conn.feed(_heard("wait"))   # barge-in
        await asyncio.sleep(0.01)
        assert states[-1] == "listening" and captions[-1] == {"cmd": "caption", "clear": True}
        conn.feed(_tool_call("end_conversation"), None)
        await task
    asyncio.run(go())


def test_idle_watchdog_waits_for_captions() -> None:
    conn = FakeConnection(_reply("Ten past ", "three."))
    clock = {"now": 0.0}
    s, _, captions = _captions_session(conn, clock, config=VoiceConfig(idle_timeout_secs=5.0, max_session_secs=600.0,
                                                                        output="captions"))

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        await _quiet(clock)
        assert captions[-1]["hold_ms"] == 5700
        clock["now"] = 5.5                              # > idle 5 s, but the page is still held
        await asyncio.sleep(WATCHDOG_WAIT)
        assert not s._ended.is_set()
        clock["now"] = 7.0                              # page cleared at 6 s; idle since 0
        await asyncio.sleep(WATCHDOG_WAIT)
        assert s._ended.is_set() and captions[-1] == {"cmd": "caption", "clear": True}
        conn.feed(None)
        await task
    asyncio.run(go())


def test_run_drains_captions_before_idle() -> None:
    conn = FakeConnection(_reply("Ten past ", "three.") + [_tool_call("end_conversation"), None])
    clock = {"now": 0.0}
    s, states, captions = _captions_session(conn, clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.05)
        assert not task.done() and states[-1] != "idle"       # holding the page, not idle yet
        assert captions[-1]["final"] is True
        clock["now"] = 6.5
        await asyncio.sleep(0.05)
        assert task.done()
        await task
    asyncio.run(go())
    assert captions[-1] == {"cmd": "caption", "clear": True} and states[-1] == "idle"


def test_hush_clears_the_caption_at_once() -> None:
    conn = FakeConnection(_reply("Ten past ", "three."))
    clock = {"now": 0.0}
    s, states, captions = _captions_session(conn, clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        await _quiet(clock)
        assert captions[-1]["final"] is True
        task.cancel()                                   # the daemon's touch hush
        with pytest.raises(asyncio.CancelledError):
            await task
    asyncio.run(go())
    assert captions[-1] == {"cmd": "caption", "clear": True} and states[-1] == "idle"


def test_new_response_chains_pages() -> None:
    conn = FakeConnection(_reply("Ten past ", "three."))
    clock = {"now": 0.0}
    s, states, captions = _captions_session(conn, clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        await _quiet(clock)
        n = len(captions)
        clock["now"] = 0.5
        conn.feed(*_reply("Sure, ", "on it."))          # a second reply while the page is held
        await asyncio.sleep(0.02)
        assert states[-1] == "speaking" and "thinking" not in states[states.index("speaking"):]
        assert captions[n:] == []                       # nothing until the demoted first page ran out
        clock["now"] = 2.9                              # clamp(15/12 + 0.8, 2, 9) = 2.05 s after 0, dwell 2 s
        await asyncio.sleep(0.02)
        assert captions[n]["page"] == 0 and captions[n]["chirp"] is True and captions[n]["lines"] == ["Sure, on it."]
        assert not any("clear" in c for c in captions)
        clock["now"] = 9.0
        await asyncio.sleep(0.02)
        assert captions[-1] == {"cmd": "caption", "clear": True} and states[-1] == "listening"
        conn.feed(_tool_call("end_conversation"), None)
        await task
    asyncio.run(go())


def test_configured_caption_cps() -> None:
    assert configured({"EVE_CAPTION_CPS": "8"}).caption_cps == 8.0
    assert configured({"EVE_CAPTION_CPS": "2"}).caption_cps == 5.0
    assert configured({"EVE_CAPTION_CPS": "99"}).caption_cps == 30.0
    assert configured({"EVE_CAPTION_CPS": "fast"}).caption_cps == 12.0
    assert configured({}).caption_cps == 12.0


def test_session_config_captions_vs_audio() -> None:
    # gpt-live-1 always speaks, so the two modes differ only in the instructions and
    # in whether the daemon plays the audio it receives
    cap = session_config(VoiceConfig(output="captions"))
    assert cap["audio"]["output"]["voice"] == "marin"
    assert "17 characters" in cap["instructions"] and "one page at a time" in cap["instructions"]
    aud = session_config(VoiceConfig(output="audio"))
    assert aud["audio"]["output"]["voice"] == "marin"
    assert "17 characters" not in aud["instructions"]
    assert configured({}).output == "captions"
    assert configured({"EVE_VOICE_OUTPUT": "audio"}).output == "audio"
    assert configured({"EVE_VOICE_OUTPUT": "loud"}).output == "captions"
    # audio mode never touches the caption callback, even when one is wired
    captions: list[dict] = []
    conn = FakeConnection(_reply("Yeah?") + [_tool_call("end_conversation"), None])
    s, _, _ = _session(conn, [FakeAgent(None, None)], on_caption=captions.append)
    asyncio.run(s.run())
    assert captions == [] and s.transcript == ["Yeah?"]   # the turn is flushed as the session closes


def test_progress_events_become_caption_pages_while_working() -> None:
    """A task's helper sentences ("opened Safari") show on the robot as pages, at most
    one per PROGRESS_MIN_GAP_SECS, only when no reply page is up."""
    from eve_bridge.computer_agent import AgentEvent as Ev

    clock = {"now": 0.0}
    conn = FakeConnection()
    s, states, captions = _captions_session(conn, clock)
    s.state = "working"
    s._on_agent_event(Ev("progress", "opened Safari", 1))
    assert captions and captions[-1]["lines"] == ["opened Safari"] and captions[-1]["final"] is True
    n = len(captions)
    s._on_agent_event(Ev("progress", "typed the search", 1))      # same instant: gap not elapsed → dropped
    assert len(captions) == n
    clock["now"] = 20.0
    s._pager.reset(clock["now"])                                   # the page has cleared
    s._on_agent_event(Ev("progress", "typed the search", 2))
    assert captions[-1]["lines"] == ["typed the search"]


def test_codex_progress_is_never_spoken() -> None:
    """Steps of a Codex task are not read aloud (owner report 2026-09-26): the voice says
    "on it" and the result, nothing in between."""
    from eve_bridge.computer_agent import AgentEvent as Ev

    async def go():
        conn = FakeConnection()
        s, _, _ = _captions_session(conn, {"now": 0.0})
        s.agent = FakeAgent(None, None)
        s.agent.provider = "codex"
        s.state = "working"
        s._on_agent_event(Ev("progress", "Opening Safari", 1))
        s._on_agent_event(Ev("progress", "Typing the search", 1))
        await asyncio.sleep(0)
        await asyncio.sleep(0)
        return conn
    conn = asyncio.run(go())
    assert not any("Opening Safari" in c or "Typing the search" in c for c in conn.commentary())


def test_conversation_close_cancels_the_task_with_a_reason() -> None:
    agent = FakeAgent(None, None)
    conn = FakeConnection([_tool_call("start_task", "c1", goal="g")])
    s, _, _ = _session(conn, [agent])

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        s.end()
        conn.feed(None)
        await task
    asyncio.run(go())
    assert agent.cancelled and agent.reason == "the conversation closed"


# ---- "go explore" -------------------------------------------------------------------------------

def test_go_explore_ends_the_conversation_and_fires_the_callback() -> None:
    fired: list[bool] = []
    conn = FakeConnection([_tool_call("go_explore"), None])
    s, states, _ = _session(conn, [FakeAgent(None, None)], on_explore=lambda: fired.append(True))
    asyncio.run(s.run())
    assert conn.tool_outputs() == [{"ok": True}]
    assert fired == [True] and s.explore_requested
    assert conn.kinds()[-1] == "response.item.create"     # no further response is requested
    assert states[-1] == "idle"


def test_go_explore_is_refused_while_a_task_runs() -> None:
    agent = FakeAgent(None, None)
    conn = FakeConnection([_tool_call("start_task", "c1", goal="open mail")])
    fired: list[bool] = []
    s, _, _ = _session(conn, [agent], on_explore=lambda: fired.append(True))

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        conn.feed(_tool_call("go_explore", "c2"))
        await asyncio.sleep(0.02)
        agent.release.set()
        await asyncio.sleep(0.02)
        conn.feed(_tool_call("end_conversation", "c3"), None)
        await task
    asyncio.run(go())
    outs = conn.tool_outputs()
    assert outs[1] == {"ok": False, "reason": "a task is running; stop it first"}
    assert fired == [] and not s.explore_requested


def test_go_explore_without_a_callback_still_ends() -> None:
    conn = FakeConnection([_tool_call("go_explore"), None])
    s, states, _ = _session(conn, [FakeAgent(None, None)])
    asyncio.run(s.run())
    assert s.explore_requested and states[-1] == "idle"


def test_instructions_mention_go_explore() -> None:
    # the voice half only has to know to delegate; the tool workflow is the backend's
    from eve_bridge.voice_agent import BACKEND_INSTRUCTIONS, INSTRUCTIONS
    assert "explore" in INSTRUCTIONS
    assert "Go explore" in BACKEND_INSTRUCTIONS and "go_explore" in BACKEND_INSTRUCTIONS


def test_prompts_do_not_let_a_started_task_read_as_done() -> None:
    """2026-09-10: the backend said "Spotify playback requested." and the voice turned it into
    "It's playing now." 12 s before the task finished."""
    from eve_bridge.voice_agent import BACKEND_INSTRUCTIONS, INSTRUCTIONS
    assert "Starting a task is not finishing it" in INSTRUCTIONS and 'INSTEAD: "On it."' in INSTRUCTIONS
    # the backend stays silent after start_task (its "On it." doubled the voice's own), and
    # never asks a clarifying question (the "what city?" round trip cost 10.4 s)
    assert "reply with an empty message" in BACKEND_INSTRUCTIONS
    assert "return exactly: On it" not in BACKEND_INSTRUCTIONS
    assert "Outside lessons, never ask the owner a clarifying question" in BACKEND_INSTRUCTIONS


def test_idle_timeout_closes_when_the_server_goes_quiet() -> None:
    """3/3 logged idle closes hung until the next Live event (+91 s, +129 s, +93 min)."""
    clock = {"now": 0.0}
    conn = FakeConnection()                        # nothing arrives after session.started
    s, states, _ = _session(conn, [FakeAgent(None, None)], clock=clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.05)
        clock["now"] = 30.0                        # past the 20 s idle timeout
        await asyncio.wait_for(task, timeout=2.0)  # ends with NO further event — no feed(None)
    asyncio.run(go())
    assert s._ended.is_set() and states[-1] == "idle"


def test_started_task_goes_working_at_once_and_tells_the_voice_quietly() -> None:
    agent = FakeAgent(None, None)
    # "listening" first: task events never set "working" from there, so only start_task can
    conn = FakeConnection([_heard("open my mail"), _tool_call("start_task", goal="open mail")])
    s, states, _ = _session(conn, [agent])

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        assert states[-1] == "working"
        quiet = [kw["content"] for k, kw in conn.sent if k == "session.thinking.append"]
        assert len(quiet) == 1 and "open mail" in quiet[0] and "Nothing is done yet" in quiet[0]
        agent.release.set()
        await asyncio.sleep(0.02)
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())


def test_stopped_task_is_not_done_and_gives_the_owners_reason() -> None:
    agent = FakeAgent(None, None)
    conn = FakeConnection([_tool_call("start_task", "c1", goal="open mail")])
    s, states, _ = _session(conn, [agent])

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        conn.feed(_tool_call("stop_task", "c2"))
        await asyncio.sleep(0.02)
        assert agent.cancelled and agent.reason == "you asked me to stop"
        s._on_agent_event(AgentEvent("cancelled", "Stopped."))
        assert states[-1] == "listening"            # no done nod for a stopped task
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())


def test_closing_session_speaks_no_result() -> None:
    """A hush used Task.cancel() but the result path checked only `_ended`, so a hushed
    conversation still tried to speak its result (15:50:04.623)."""
    agent = FakeAgent(None, None, final="Mail is open.")
    conn = FakeConnection([_tool_call("start_task", "c1", goal="open mail")])
    s, _, _ = _session(conn, [agent])

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        task.cancel()                               # the hush path
        with pytest.raises(asyncio.CancelledError):
            await task
    asyncio.run(go())
    # the greeting is the only thing the voice was handed: no result of any kind after the hush
    assert len(conn.commentary()) == 1 and "Greet" in conn.commentary()[0]


# ---- eyes, head, standing orders (2026-09-10) -------------------------------------------------

def _thinking(conn: FakeConnection) -> list[str]:
    return [kw["content"] for k, kw in conn.sent if k == "session.thinking.append"]


class FakeScene:
    def __init__(self, view: dict | None = None) -> None:
        self.started = self.stopped = 0
        self.view = view or {"ok": True, "view": "A person holding a green mug.", "seen_at": "16:43:05",
                             "age_secs": 1.0, "stale": False}
        self.located: list[str] = []

    def start(self) -> None:
        self.started += 1

    async def stop(self) -> None:
        self.stopped += 1

    async def look(self, fresh_secs: float = 4.0, newer_than=None) -> dict:
        await asyncio.sleep(0)
        return self.view

    async def locate(self, target: str, newer_than=None) -> dict:
        self.located.append(target)
        return {"ok": True, "visible": True, "x": 0.0, "y": 0.0, "what": target, "yaw": 0, "pitch": 45}


class FakeHead:
    def __init__(self) -> None:
        self.moves: list[tuple] = []
        self.yaw, self.pitch = 0.0, 45.0

    def clock(self) -> float:
        return 0.0

    async def sleep(self, secs: float) -> None:
        await asyncio.sleep(0)

    async def move(self, yaw=None, pitch=None, relative=False, hold_secs=15.0) -> dict:
        self.moves.append((yaw, pitch, relative, hold_secs))
        if yaw is not None:
            self.yaw = self.yaw + yaw if relative else yaw
        if pitch is not None:
            self.pitch = self.pitch + pitch if relative else pitch
        return {"ok": True, "yaw": int(self.yaw), "pitch": int(self.pitch), "facing": "x", "hold_secs": hold_secs}

    async def face_owner(self) -> dict:
        return await self.move(0, 45, hold_secs=3.0)


def test_scene_watches_with_the_session_and_nothing_it_sees_is_pushed_to_the_voice() -> None:
    # Owner report 2026-09-10: pushed [vision] lines made eve narrate the room.
    # The watcher runs for the conversation, but a view reaches the voice only
    # through the look tool.
    conn = FakeConnection()
    scene = FakeScene()
    s, _, _ = _session(conn, [FakeAgent(None, None)], scene=scene)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.05)
        assert scene.started == 1
        assert not hasattr(scene, "on_note")
        assert not any("[vision" in t for t in _thinking(conn))
        conn.feed(_tool_call("end_conversation"), None)
        await task
    asyncio.run(go())
    assert scene.stopped == 1
    assert not any("[vision" in t for t in _thinking(conn))


def test_look_tool_answers_from_a_background_task() -> None:
    conn = FakeConnection([_tool_call("look", "c1")])
    s, _, _ = _session(conn, [FakeAgent(None, None)], scene=FakeScene())

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.05)
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    assert conn.tool_outputs()[0]["view"] == "A person holding a green mug."


def test_think_hard_answers_from_a_background_task_and_keeps_the_voice_company() -> None:
    asked: list[str] = []
    release = asyncio.Event()

    async def thinker(question: str) -> dict:
        asked.append(question)
        await release.wait()
        return {"ok": True, "answer": "Forty-two, because the question was the easy part."}

    conn = FakeConnection([_tool_call("think_hard", "c1", question="what is six times seven, and why?")])
    s, _, _ = _session(conn, [FakeAgent(None, None)], thinker=thinker)

    async def go():
        task = asyncio.create_task(s.run())
        await _eventually(lambda: bool(asked))
        # while the brain works: the voice was told to keep the owner company, nothing answered yet
        assert any("worked on in the background" in t for t in _thinking(conn))
        assert conn.tool_outputs() == []
        release.set()
        await _eventually(lambda: bool(conn.tool_outputs()))
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    assert asked == ["what is six times seven, and why?"]
    assert conn.tool_outputs()[0] == {"ok": True, "answer": "Forty-two, because the question was the easy part."}


def test_think_hard_without_a_thinker_or_when_it_fails_tells_the_backend_to_answer_itself() -> None:
    conn = FakeConnection([_tool_call("think_hard", "c1", question="why?")])
    s, _, _ = _session(conn, [FakeAgent(None, None)])

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.05)
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    assert conn.tool_outputs()[0]["ok"] is False and "answer as best you can" in conn.tool_outputs()[0]["reason"]

    async def broken(_q: str) -> dict:
        raise TimeoutError("90 s")

    conn = FakeConnection([_tool_call("think_hard", "c1", question="why?")])
    s, _, _ = _session(conn, [FakeAgent(None, None)], thinker=broken)
    asyncio.run(go())
    out = conn.tool_outputs()[0]
    assert out["ok"] is False and "did not answer in time" in out["reason"]


def test_look_without_vision_says_why() -> None:
    conn = FakeConnection([_tool_call("look", "c1")])
    s, _, _ = _session(conn, [FakeAgent(None, None)])

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.05)
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    assert conn.tool_outputs()[0] == {"ok": False, "reason": "vision is not set up on this computer"}


def test_move_head_passes_the_backend_numbers_through() -> None:
    head = FakeHead()
    conn = FakeConnection([_tool_call("move_head", "c1", yaw=-15, relative=True),
                           _tool_call("move_head", "c2", yaw=120, pitch=45, relative=False, hold_secs=30),
                           _tool_call("move_head", "c3", yaw="left", relative=True),
                           _tool_call("end_conversation", "c9"), None])
    s, _, _ = _session(conn, [FakeAgent(None, None)], head=head)
    asyncio.run(s.run())
    assert head.moves[0] == (-15.0, None, True, 15.0)
    assert head.moves[1] == (120.0, 45.0, False, 30.0)
    assert head.moves[2] == (None, None, True, 15.0)          # a non-number is dropped, not guessed
    assert conn.tool_outputs()[1]["yaw"] == 120


def test_look_around_and_find_run_through_head_and_scene() -> None:
    head, scene = FakeHead(), FakeScene()
    conn = FakeConnection([_tool_call("look_around", "c1")])
    s, _, _ = _session(conn, [FakeAgent(None, None)], head=head, scene=scene)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.1)
        conn.feed(_tool_call("find", "c2", target="the door"))
        await asyncio.sleep(0.1)
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    outs = conn.tool_outputs()
    around = next(o for o in outs if "views" in o)
    assert around["ok"] is True and len(around["views"]) == 5
    found = next(o for o in outs if "found" in o)
    assert found["found"] is True and scene.located == ["the door"]


@pytest.mark.parametrize("words", ["Okay, bye eve.", "I want you to leave now.", "Stop listening."])
def test_goodbye_in_the_owners_words_ends_the_session(words: str) -> None:
    clock = {"now": 0.0}
    conn = FakeConnection()
    s, states, mic = _session(conn, [FakeAgent(None, None)], clock=clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        conn.feed(_heard(words))
        await asyncio.sleep(0.01)
        conn.feed(_spoke("Bye!"))                       # eve's goodbye closes the user turn
        await asyncio.sleep(0.01)
        assert s._farewell and not s._ended.is_set()    # not before the goodbye has been said
        clock["now"] += TURN_GAP_SECS + 0.1             # the goodbye goes quiet...
        await asyncio.sleep(0.1)
        clock["now"] += 1.0
        await asyncio.sleep(WATCHDOG_WAIT)                        # ...and the watchdog closes the session
        assert s._ended.is_set()
        conn.feed(None)
        await task
    asyncio.run(go())
    assert states[-1] == "idle"
    assert any("[leaving]" in t for t in _thinking(conn))
    assert s.tool_calls == []                           # it never depended on the model calling a tool


def test_goodbye_closes_even_if_eve_says_nothing_back() -> None:
    clock = {"now": 0.0}
    conn = FakeConnection()
    s, _, _ = _session(conn, [FakeAgent(None, None)], clock=clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        conn.feed(_heard("goodbye"))
        await asyncio.sleep(0.01)
        clock["now"] += TURN_GAP_SECS + 0.1             # the user turn closes on the gap
        await asyncio.sleep(0.1)
        assert s._farewell and not s._ended.is_set()
        clock["now"] += FAREWELL_MAX_SECS
        await asyncio.sleep(WATCHDOG_WAIT)
        assert s._ended.is_set()
        conn.feed(None)
        await task
    asyncio.run(go())


def test_the_classifier_catches_a_goodbye_the_phrase_table_does_not_know() -> None:
    clock = {"now": 0.0}
    seen: list[str] = []

    async def intent(text: str):
        seen.append(text)
        return "leave" if "heading out" in text else None

    conn = FakeConnection()
    s, _, _ = _session(conn, [FakeAgent(None, None)], clock=clock, intent=intent)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        conn.feed(_heard("alright little guy, I'm heading out"), _spoke("See ya."))
        await asyncio.sleep(0.05)
        assert s._farewell
        clock["now"] += TURN_GAP_SECS + FAREWELL_QUIET_SECS + 0.5
        await asyncio.sleep(WATCHDOG_WAIT)
        assert s._ended.is_set()
        conn.feed(None)
        await task
    asyncio.run(go())
    assert seen == ["alright little guy, I'm heading out"]


def test_leave_the_tab_open_does_not_end_anything() -> None:
    clock = {"now": 0.0}

    async def intent(text: str):
        return None                                       # the model says: not about eve

    conn = FakeConnection()
    s, _, _ = _session(conn, [FakeAgent(None, None)], clock=clock, intent=intent)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        conn.feed(_heard("leave the tab open"), _spoke("On it."))
        await asyncio.sleep(0.05)
        clock["now"] += 8.0
        await asyncio.sleep(WATCHDOG_WAIT)
        assert not s._farewell and not s._ended.is_set()
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())


def test_goodbye_during_a_task_stops_the_mic_and_waits_for_the_result() -> None:
    agent = FakeAgent(None, None, final="Spotify is playing.")
    clock = {"now": 0.0}
    conn = FakeConnection([_tool_call("start_task", "c1", goal="play music")])
    s, _, mic = _session(conn, [agent], clock=clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        conn.feed(_done(), _heard("thanks, bye"), _spoke("Bye!"))
        await asyncio.sleep(0.02)
        assert s._farewell and s.task_running and not agent.cancelled   # the task is not killed
        before = len([k for k in conn.kinds() if k == "session.input_audio.append"])
        mic.put_nowait(b"\x00\x01" * 50)
        await asyncio.sleep(0.02)
        after = len([k for k in conn.kinds() if k == "session.input_audio.append"])
        assert after == before                                          # eve stopped listening
        clock["now"] += FAREWELL_MAX_SECS + 1.0
        await asyncio.sleep(WATCHDOG_WAIT)
        assert not s._ended.is_set()                                    # still waiting on the task
        agent.release.set()
        await asyncio.sleep(0.02)
        assert "Spotify is playing." in conn.commentary()[-1]           # the result is said first
        conn.feed(_spoke("Spotify is playing."))
        await asyncio.sleep(0.01)
        clock["now"] += TURN_GAP_SECS + FAREWELL_QUIET_SECS + 0.2
        await asyncio.sleep(WATCHDOG_WAIT)
        assert s._ended.is_set()
        conn.feed(None)
        await task
    asyncio.run(go())
    assert not agent.cancelled


def test_mute_and_unmute_by_voice_keep_the_conversation_going() -> None:
    clock = {"now": 0.0}
    sound = {"on": True}
    calls: list[bool] = []

    def on_sound(on: bool) -> None:
        calls.append(on)
        sound["on"] = on

    pcm = base64.b64encode(b"\x00\x10" * 50).decode()
    conn = FakeConnection()
    s, _, _ = _session(conn, [FakeAgent(None, None)], clock=clock, on_sound=on_sound,
                       muted=lambda: not sound["on"])

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        conn.feed(_heard("mute"), _spoke("Okay."))
        await asyncio.sleep(0.02)
        assert calls == [False] and not s._ended.is_set() and not s._farewell
        conn.feed({"type": "session.output_audio.delta", "delta": pcm})
        await asyncio.sleep(0.01)
        assert s.speaker.played == b""                                  # muted: the Mac plays nothing
        clock["now"] += TURN_GAP_SECS + 0.1
        conn.feed(_heard("unmute yourself"), _spoke("Back."))
        await asyncio.sleep(0.02)
        assert calls == [False, True]
        conn.feed({"type": "session.output_audio.delta", "delta": pcm})
        await asyncio.sleep(0.01)
        assert s.speaker.played == b"\x00\x10" * 50
        conn.feed(_tool_call("set_sound", "c1", on=False), _tool_call("set_sound", "c2", on="loud"))
        await asyncio.sleep(0.02)
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    notes = _thinking(conn)
    assert any(n.startswith("[sound] You are muted") for n in notes)
    assert any(n == "[sound] Your sound is back on." for n in notes)
    assert calls == [False, True, False]
    assert conn.tool_outputs()[0] == {"ok": True, "sound": "off"}
    assert conn.tool_outputs()[1] == {"ok": False, "reason": "on must be true or false"}


def test_a_slow_look_holds_off_the_idle_close_and_never_logs_the_view(caplog) -> None:
    clock = {"now": 0.0}
    gate = asyncio.Event()

    class SlowScene(FakeScene):
        async def look(self, fresh_secs: float = 4.0, newer_than=None) -> dict:
            await gate.wait()
            return self.view

    conn = FakeConnection([_tool_call("look", "c1")])
    s, _, _ = _session(conn, [FakeAgent(None, None)], clock=clock, scene=SlowScene())

    async def go():
        with caplog.at_level("INFO"):
            task = asyncio.create_task(s.run())
            await asyncio.sleep(0.02)
            clock["now"] += 60.0                       # far past the 20 s idle timeout
            await asyncio.sleep(WATCHDOG_WAIT)
            assert not s._ended.is_set()               # a look in flight is not idleness
            gate.set()
            await asyncio.sleep(0.05)
            assert conn.tool_outputs()[0]["ok"] is True
            conn.feed(_tool_call("end_conversation", "c9"), None)
            await task
    asyncio.run(go())
    assert not any("green mug" in r.getMessage() for r in caplog.records)


def test_a_head_request_the_voice_did_not_delegate_is_routed_to_the_backend() -> None:
    """17:43 on the bench: "look a bit up and to your left" got "Okay, looking." and no tool call."""
    conn = FakeConnection()
    s, _, _ = _session(conn, [FakeAgent(None, None)])

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        conn.feed(_heard("Look a bit up and to your left."), _spoke("Okay, looking."))
        await asyncio.sleep(voice_agent.LOOK_ROUTE_DELAY_SECS + 0.15)
        assert conn.user_messages() == ["[look request] Look a bit up and to your left."]
        assert conn.kinds()[-1] == "response.create"          # the backend is asked to act on it
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())


def test_a_lesson_request_the_voice_did_not_delegate_is_routed_to_the_backend() -> None:
    """11:44 on the bench: "teach me about Rust ownership" got a spoken explanation and no lesson."""
    conn = FakeConnection()
    s, _, _ = _session(conn, [FakeAgent(None, None)])

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        conn.feed(_heard("Teach me about Rust ownership."), _spoke("Sure. What have you done in Rust so far?"))
        await asyncio.sleep(voice_agent.LESSON_ROUTE_DELAY_SECS + 0.15)
        assert conn.user_messages() == ["[lesson request] Teach me about Rust ownership."]
        assert conn.kinds()[-1] == "response.create"          # the backend is asked to open the lesson
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())


def test_a_lesson_request_the_voice_delegated_is_not_routed_twice() -> None:
    clock = {"now": 0.0}
    conn = FakeConnection()
    s, _, _ = _session(conn, [FakeAgent(None, None)], clock=clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        clock["now"] = 5.0
        conn.feed(_heard("quiz me on French verbs"))
        await asyncio.sleep(0.01)
        clock["now"] = 5.3
        conn.feed(_delegated(), _spoke("Let's do it."))      # the voice delegated it itself
        await asyncio.sleep(voice_agent.LESSON_ROUTE_DELAY_SECS + 0.15)
        assert conn.user_messages() == []
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())


def test_the_backend_is_told_what_a_lesson_request_tag_means() -> None:
    from eve_bridge.voice_agent import BACKEND_INSTRUCTIONS
    assert "[lesson request]" in BACKEND_INSTRUCTIONS
    assert "never answer it without the tool" in BACKEND_INSTRUCTIONS


def test_the_lesson_wake_word_opens_in_lesson_setup_and_routes_the_owner_answer() -> None:
    """Say "lesson": the daemon opened the window, eve asks learn-or-bring, and the answer must reach
    the backend even when the voice just chats back."""
    conn = FakeConnection()
    s, states, _ = _session(conn, [FakeAgent(None, None)], lesson_wake=True)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.05)
        assert len(conn.user_messages()) == 1 and conn.user_messages()[0].startswith("[lesson setup] The owner said")
        assert "response.create" not in conn.kinds()          # the greeting asks; no backend turn yet
        conn.feed(_heard("Learn a topic. Fractions, grade four."), _spoke("Fractions, nice choice!"))
        await asyncio.sleep(voice_agent.LESSON_ROUTE_DELAY_SECS + 0.15)
        assert conn.user_messages()[-1] == "[lesson setup] Learn a topic. Fractions, grade four."
        assert conn.kinds()[-1] == "response.create"
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    assert states[0] == "wake"


def test_a_started_lesson_ends_lesson_setup_so_later_turns_are_not_routed() -> None:
    def learning(**args):
        return {"ok": True, "answer": "Here is your first problem."}
    conn = FakeConnection()
    s, _, _ = _session(conn, [FakeAgent(None, None)], lesson_wake=True, learning=learning)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.05)
        conn.feed(_tool_call("lesson", "c2", action="start", mode="learn", topic="Fractions", level="Grade 4"))
        await asyncio.sleep(0.05)
        await asyncio.gather(*s._slow_tasks)
        assert s._lesson_setup is False
        before = len(conn.user_messages())
        conn.feed(_heard("I think I add the tops."), _spoke("Hmm, try it."))
        await asyncio.sleep(voice_agent.LESSON_ROUTE_DELAY_SECS + 0.15)
        assert len(conn.user_messages()) == before                   # not a setup answer any more
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())


def test_a_head_request_the_voice_delegated_is_not_routed_twice() -> None:
    clock = {"now": 0.0}
    conn = FakeConnection()
    s, _, _ = _session(conn, [FakeAgent(None, None)], clock=clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        clock["now"] = 5.0
        conn.feed(_heard("look a bit more left"))
        await asyncio.sleep(0.01)
        clock["now"] = 5.3
        conn.feed(_delegated(), _spoke("Turning."))          # the voice delegated it itself
        await asyncio.sleep(voice_agent.LOOK_ROUTE_DELAY_SECS + 0.15)
        assert conn.user_messages() == []
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())


def test_the_classifier_routes_a_head_request_the_table_does_not_know() -> None:
    async def intent(text: str):
        return "look" if "keys" in text else None

    conn = FakeConnection()
    s, _, _ = _session(conn, [FakeAgent(None, None)], intent=intent)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        conn.feed(_heard("where did I leave my keys"), _spoke("Let me see."))
        await asyncio.sleep(voice_agent.LOOK_ROUTE_DELAY_SECS + 0.15)
        assert conn.user_messages() == ["[look request] where did I leave my keys"]
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())


def test_a_muted_session_starts_by_telling_the_voice() -> None:
    conn = FakeConnection([_tool_call("end_conversation"), None])
    s, _, _ = _session(conn, [FakeAgent(None, None)], muted=lambda: True)
    asyncio.run(s.run())
    assert _thinking(conn)[0].startswith("[sound] You are muted")



def test_lesson_dispatch_uses_existing_voice_and_returns_saved_step():
    called = []
    def learning(**args):
        called.append(args)
        return {"ok": True, "answer": "2x = 8. Subtract three from both sides."}
    conn = FakeConnection([])
    session, _, _ = _session(conn, [FakeAgent(None, None)], learning=learning)
    async def go():
        await session._tool("lesson", "math1", '{"action":"step"}')
        await asyncio.gather(*session._slow_tasks)
    asyncio.run(go())
    assert called == [{"action": "step"}]
    assert conn.tool_outputs()[0]["ok"] is True


def test_lesson_without_service_returns_an_actionable_error():
    conn = FakeConnection([])
    session, _, _ = _session(conn, [FakeAgent(None, None)])
    async def go():
        await session._tool("lesson", "math1", '{"action":"open"}')
        await asyncio.gather(*session._slow_tasks)
    asyncio.run(go())
    assert conn.tool_outputs()[0]["ok"] is False


def _run_lesson(learning, arguments):
    conn = FakeConnection([])
    session, _, _ = _session(conn, [FakeAgent(None, None)], learning=learning)

    async def go():
        await session._tool("lesson", "math1", arguments)
        await asyncio.gather(*session._slow_tasks)
    asyncio.run(go())
    return conn.tool_outputs()[0]


def test_lesson_drops_unknown_arguments():
    called = []

    def learning(**args):
        called.append(args)
        return {"ok": True, "answer": "Try the ones place."}
    out = _run_lesson(learning, '{"action":"step","bogus":1}')
    assert called == [{"action": "step"}]
    assert out["ok"] is True


def test_lesson_with_unreadable_arguments_does_not_open_the_whiteboard():
    called = []

    def learning(**args):
        called.append(args)
        return {"ok": True}
    for arguments in ("not json", "[1, 2]"):
        out = _run_lesson(learning, arguments)
        assert out["ok"] is False and "Unknown lesson action" in out["reason"]
    assert called == []


def test_lesson_stale_lesson_is_actionable():
    def learning(**args):
        raise KeyError("gone")
    out = _run_lesson(learning, '{"action":"hint"}')
    assert out["ok"] is False
    assert "no longer there" in out["reason"] and "lesson failed" not in out["reason"]


def test_lesson_schema_uses_the_shared_action_set():
    from eve_bridge.learning import LESSON_ACTIONS
    from eve_bridge.voice_agent import TOOLS
    tool = next(t for t in TOOLS if t["name"] == "lesson")
    assert tool["parameters"]["properties"]["action"]["enum"] == list(LESSON_ACTIONS)
    assert tool["parameters"]["additionalProperties"] is False


def test_the_lesson_tool_is_named_lesson_and_teaches_any_subject():
    # The voice model routes by tool name, so the name must not narrow the subject.
    from eve_bridge.voice_agent import BACKEND_INSTRUCTIONS, INSTRUCTIONS, TOOLS
    names = [t["name"] for t in TOOLS]
    assert "lesson" in names and "math_lesson" not in names
    tool = next(t for t in TOOLS if t["name"] == "lesson")
    assert "any subject" in tool["description"] and "math" not in tool["description"].lower()
    for text in (INSTRUCTIONS, BACKEND_INSTRUCTIONS):
        assert "teach me" in text and "any subject" in text
        assert "lesson" in text and "math_lesson" not in text


# ---- think out loud ------------------------------------------------------------------------------

LESSON = {"id": "L1", "topic": "Algebra", "level": "Grade 5", "problem": "Solve 2x + 3 = 11.", "stage": "working",
          "ideas": "typed first", "events": [{"action": "step", "step": "2x = 11 - 3"}]}


def _listening(conn, clock, **kw):
    notices: list = []
    saved: list = []

    def save(lesson_id, text):
        saved.append((lesson_id, text))
        return True
    kw.setdefault("think_aloud", LESSON)
    s, states, mic = _session(conn, [FakeAgent(None, None)], clock=clock, on_spoken_idea=save,
                              on_think_aloud=lambda on, lid: notices.append((on, lid)), **kw)
    return s, states, notices, saved


def test_session_config_for_listening_carries_the_rules_and_the_lesson() -> None:
    plain = session_config(VoiceConfig())
    listening = session_config(VoiceConfig(), think_aloud=LESSON)
    assert "[think out loud]" not in plain["instructions"]
    assert "think out loud" not in plain["delegation"]["responses"]["instructions"].lower()
    assert listening["instructions"].startswith(plain["instructions"])
    assert "Never say the final answer" in listening["instructions"]
    assert "Solve 2x + 3 = 11." in listening["instructions"]
    backend = listening["delegation"]["responses"]["instructions"]
    assert "Never state the final answer" in backend and "Solve 2x + 3 = 11." in backend


def test_a_listening_session_opens_without_the_wake_greeting_and_reports_on_and_off() -> None:
    clock = {"now": 0.0}
    conn = FakeConnection([])
    s, states, notices, _ = _listening(conn, clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        assert notices == [(True, "L1")]
        s.stop_think_aloud()
        conn.feed(None)
        await task
    asyncio.run(go())
    assert states[0] == "listening" and "wake" not in states
    assert "I'm listening" in conn.commentary()[0]
    assert "Solve 2x + 3 = 11." in conn.sent[0][1]["session"]["instructions"]
    assert notices == [(True, "L1"), (False, None)] and s.end_reason == "stopped"


def test_listening_waits_through_pauses_and_stops_after_a_minute_of_silence() -> None:
    clock = {"now": 0.0}
    conn = FakeConnection([])
    s, _, notices, _ = _listening(conn, clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        clock["now"] = 45.0                   # past the 20 s idle timeout: a thinking pause, not the end
        await asyncio.sleep(WATCHDOG_WAIT)
        assert not s._ended.is_set()
        clock["now"] = 61.5
        await asyncio.sleep(WATCHDOG_WAIT)
        assert s._ended.is_set() and s.end_reason == "silence"
        conn.feed(None)
        await task
    asyncio.run(go())
    assert notices[-1] == (False, None)


def test_listening_still_ends_at_the_session_cap() -> None:
    clock = {"now": 0.0}
    conn = FakeConnection([])
    s, _, _, _ = _listening(conn, clock, config=VoiceConfig(idle_timeout_secs=20.0, max_session_secs=100.0,
                                                            output="audio"))

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        for t in (40.0, 80.0, 101.0):         # someone keeps talking: silence never closes it
            clock["now"] = t
            s._last_activity = t
            await asyncio.sleep(WATCHDOG_WAIT)
        assert s._ended.is_set() and s.end_reason == "cap"
        conn.feed(None)
        await task
    asyncio.run(go())


def test_spoken_thinking_is_saved_to_the_lesson_and_never_logged_or_remembered(caplog) -> None:
    caplog.set_level("INFO")
    clock = {"now": 0.0}
    conn = FakeConnection([])
    s, _, _, saved = _listening(conn, clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        conn.feed(_heard("I think I take away three from both sides"))
        await asyncio.sleep(0.02)
        clock["now"] = 2.0                    # past the turn gap: the turn closes
        await _eventually(lambda: saved)
        conn.feed(_spoke("Nice start. What is left on the right side?"))
        await asyncio.sleep(0.02)
        clock["now"] = 4.0
        await asyncio.sleep(0.1)
        s.stop_think_aloud()
        conn.feed(None)
        await task
    asyncio.run(go())
    assert saved == [("L1", "I think I take away three from both sides")]
    assert s.turns == []                      # nothing reaches chat memory
    assert "take away three" not in caplog.text and "What is left" not in caplog.text
    assert "spoken idea saved ok=True" in caplog.text


def test_saying_done_says_goodbye_and_is_not_saved_as_an_idea() -> None:
    clock = {"now": 0.0}
    conn = FakeConnection([])
    s, _, _, saved = _listening(conn, clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        conn.feed(_heard("okay I'm done thinking"))
        await asyncio.sleep(0.02)
        clock["now"] = 2.0
        await _eventually(lambda: s._farewell)
        clock["now"] = 2.0 + FAREWELL_MAX_SECS + 1
        await _eventually(lambda: s._ended.is_set())
        conn.feed(None)
        await task
    asyncio.run(go())
    assert saved == [] and s.end_reason == "goodbye"


def test_while_listening_lesson_ideas_never_overwrites_and_stop_listening_says_goodbye() -> None:
    called: list = []

    def learning(**args):
        called.append(args)
        return {"ok": True, "answer": "One step."}
    conn = FakeConnection([])
    s, _, _, _ = _listening(conn, {"now": 0.0}, learning=learning)

    async def go():
        await s._tool("lesson", "m1", '{"action":"ideas","text":"erase it all"}')
        await s._tool("lesson", "m2", '{"action":"stop-listening"}')
        await asyncio.gather(*s._slow_tasks)
        await s._tool("start_task", "t1", '{"goal":"open Safari"}')
        await s._tool("go_explore", "t2", "{}")
    asyncio.run(go())
    assert called == []
    outs = conn.tool_outputs()
    assert outs[0]["ok"] is True and "Already saved" in outs[0]["answer"]
    assert outs[1]["ok"] is True and s._farewell
    assert outs[2]["ok"] is False and outs[3]["ok"] is False
    assert s.agent is None and not s.explore_requested


def test_a_step_while_listening_waits_for_what_the_learner_just_said() -> None:
    order: list = []

    def save(lesson_id, text):
        order.append("saved")
        return True

    def learning(**args):
        order.append(args["action"])
        return {"ok": True, "answer": "2x = 8"}
    conn = FakeConnection([])
    s, _, _ = _session(conn, [FakeAgent(None, None)], think_aloud=LESSON, on_spoken_idea=save, learning=learning)

    async def go():
        s._transcript_delta("user", "can you show me one step")   # the turn is still open when the tool is called
        await s._tool("lesson", "m1", '{"action":"step"}')
        await asyncio.gather(*s._slow_tasks)
    asyncio.run(go())
    assert order == ["saved", "step"]
    assert conn.tool_outputs()[0]["ok"] is True


def test_an_open_conversation_switches_to_listening_without_a_second_session() -> None:
    clock = {"now": 0.0}
    conn = FakeConnection([])
    notices: list = []
    s, _, _ = _session(conn, [FakeAgent(None, None)], clock=clock,
                       on_think_aloud=lambda on, lid: notices.append((on, lid)))

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.02)
        out = await s.enter_think_aloud(LESSON)
        again = await s.enter_think_aloud(LESSON)
        clock["now"] = 30.0                   # past the conversation's idle timeout
        await asyncio.sleep(WATCHDOG_WAIT)
        assert not s._ended.is_set()
        s.stop_think_aloud()
        conn.feed(None)
        await task
        return out, again
    out, again = asyncio.run(go())
    assert out == {"ok": True} and again["already"] is True
    assert "Greet them" in conn.commentary()[0]
    assert any("[think out loud]" in t and "Solve 2x + 3 = 11." in t for t in _thinking(conn))
    updates = [kw for k, kw in conn.sent if k == "session.update"]
    assert len(updates) == 1
    assert "Never state the final answer" in updates[0]["session"]["delegation"]["responses"]["instructions"]
    assert notices == [(True, "L1"), (False, None)]


def test_a_closing_conversation_refuses_to_switch() -> None:
    conn = FakeConnection([])
    s, _, _ = _session(conn, [FakeAgent(None, None)])
    s.end()
    out = asyncio.run(s.enter_think_aloud(LESSON))
    assert out["ok"] is False and s.think_aloud_lesson_id is None


def test_outside_listening_eves_words_are_logged_as_before(caplog) -> None:
    """Positive control for the no-words log test: the same reply is logged when nobody is thinking out loud."""
    caplog.set_level("INFO")
    conn = FakeConnection([])
    s, _, _ = _session(conn, [FakeAgent(None, None)])
    s._close_turn("assistant", "Nice start. What is left on the right side?")
    assert "What is left" in caplog.text


def test_while_listening_only_goodbye_and_mute_act_and_nothing_is_starred(tmp_path) -> None:
    sounds: list = []
    conn = FakeConnection([])
    memory = FakeMemory(tmp_path)
    s, _, _, saved = _listening(conn, {"now": 0.0}, memory=memory, on_sound=sounds.append)
    starred = memory.starred
    classified: list = []

    async def classify(text):
        classified.append(text)
        return "leave"
    s.intent = classify

    async def go():
        s._close_turn("user", "I think the answer has a 3 in it")
        s._close_turn("user", "remember that")
        s._close_turn("user", "look to your left")
        s._close_turn("user", "mute")
        await asyncio.sleep(0.05)
    asyncio.run(go())
    assert starred == [] and not s._farewell and classified == []
    assert memory.lines(s.conv) == []          # and nothing reaches the transcript
    assert sounds == [False]
    assert [t for _, t in saved] == ["I think the answer has a 3 in it", "remember that", "look to your left"]


def test_spoken_ideas_are_saved_in_the_order_spoken_even_when_one_save_is_slow(tmp_path) -> None:
    saved: list = []

    def save(lesson_id, text):
        if text == "first":
            time.sleep(0.1)            # a slow disk write must not let the next idea jump ahead
        saved.append(text)
        return True
    s, _, _ = _session(FakeConnection([]), [FakeAgent(None, None)], clock={"now": 0.0},
                       on_spoken_idea=save, think_aloud=LESSON, on_think_aloud=lambda on, lid: None)

    async def go():
        s._close_turn("user", "first")
        s._close_turn("user", "second")
        s._close_turn("user", "third")
        await asyncio.gather(*s._idea_saves, return_exceptions=True)
        while s._idea_saves:
            await asyncio.sleep(0.01)
    asyncio.run(go())
    assert saved == ["first", "second", "third"]


def test_a_running_task_refuses_listening() -> None:
    agent = FakeAgent(None, None)
    conn = FakeConnection([])
    s, _, _ = _session(conn, [agent])

    async def go():
        s._started.set()
        await s._tool("start_task", "t1", '{"goal":"open Mail"}')
        out = await s.enter_think_aloud(LESSON)
        agent.cancel()
        await asyncio.gather(s._agent_task, return_exceptions=True)
        return out
    out = asyncio.run(go())
    assert out["ok"] is False and "computer task" in out["reason"]


# ---- take_photo -----------------------------------------------------------------------------

def test_take_photo_hands_the_note_to_the_daemon_and_returns_the_caption() -> None:
    asked: list[str] = []

    async def on_photo(note: str) -> dict:
        asked.append(note)
        return {"ok": True, "path": "/n/photos/2026-09-13/120000-7.jpg", "caption": "a red bike by the door",
                "answer": "Kept it: a red bike by the door"}

    conn = FakeConnection([_tool_call("take_photo", "c1", note="my bike")])
    s, _, _ = _session(conn, [FakeAgent(None, None)], on_photo=on_photo)

    async def go():
        task = asyncio.create_task(s.run())
        await _eventually(lambda: bool(conn.tool_outputs()))
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    assert asked == ["my bike"]
    assert conn.tool_outputs()[0]["ok"] is True and conn.tool_outputs()[0]["caption"] == "a red bike by the door"


def test_take_photo_without_a_camera_or_when_it_stalls_says_why() -> None:
    conn = FakeConnection([_tool_call("take_photo", "c1", note="")])
    s, _, _ = _session(conn, [FakeAgent(None, None)])

    async def go():
        task = asyncio.create_task(s.run())
        await _eventually(lambda: bool(conn.tool_outputs()))
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    out = conn.tool_outputs()[0]
    assert out["ok"] is False and "no camera" in out["reason"]

    async def stalls(_note: str) -> dict:
        await asyncio.sleep(5)
        return {"ok": True}

    conn = FakeConnection([_tool_call("take_photo", "c1", note="")])
    s, _, _ = _session(conn, [FakeAgent(None, None)], on_photo=stalls)
    s.PHOTO_TIMEOUT_SECS = 0.02
    asyncio.run(go())
    out = conn.tool_outputs()[0]
    assert out["ok"] is False and "did not answer in time" in out["reason"]


def test_live_expression_callback_receives_streamed_reply_and_user_turn():
    async def run():
        seen = []
        s, _, _ = _session(FakeConnection(), [], on_expression=lambda who, text: seen.append((who, text)))
        s._transcript_delta("assistant", "That is such wonderful news!")
        assert seen == [("assistant", "That is such wonderful news!")]
        s._close_turn("user", "I found something interesting.")
        assert seen[-1] == ("user", "I found something interesting.")
    asyncio.run(run())


def test_the_records_profile_reaches_the_voice_prompt_and_is_absent_without_one(tmp_path) -> None:
    plain = session_config(VoiceConfig())
    assert session_config(VoiceConfig(), profile="")["instructions"] == plain["instructions"]
    page = "## Life context\n- Their name is Sam."
    with_page = session_config(VoiceConfig(), profile=page)["instructions"]
    from eve_bridge.voice_agent import profile_block
    assert page in with_page and with_page.replace(profile_block(page), "") == plain["instructions"]
    assert "without reciting it" in with_page
    conn = FakeConnection([_tool_call("end_conversation"), None])
    s, _, _ = _session(conn, [FakeAgent(None, None)], memory=FakeMemory(tmp_path, voice_profile=page))
    asyncio.run(s.run())
    started = [kw for name, kw in conn.sent if name == "session.start"]
    assert started and page in started[0]["session"]["instructions"]


# ---- memory: capture, prompts, lookups ----------------------------------------------------------------
#
# memory.py is built beside this file; FakeMemory is its surface as voice_agent uses it, over a REAL
# transcript store in a tmp folder, so every capture assertion reads what actually reached the disk.

MEMORY_TOOL_NAMES = ["memory_search", "memory_read", "forget_preview", "forget_apply"]


def _memory_schemas(voice: bool = False) -> list[dict]:
    names = MEMORY_TOOL_NAMES + (["recent_conversation"] if voice else [])
    return [{"type": "function", "name": n, "description": "d", "strict": True,
             "parameters": {"type": "object", "properties": {}, "required": [], "additionalProperties": False}}
            for n in names]


class FakeMemory:
    def __init__(self, tmp_path, profile: str = "", voice_profile: str = "", wall=None, handle=None) -> None:
        self.transcripts = Transcripts(TranscriptConfig(enabled=True, root=tmp_path / "transcripts"), wall=wall)
        assert self.transcripts.enabled
        self._profile, self._voice = profile, voice_profile
        self.starred: list[str] = []
        self.calls: list[tuple] = []
        self.handle = handle

    def profile(self) -> str:
        return self._profile

    def profile_for_voice(self) -> str:
        return self._voice

    def today(self, max_chars: int, **kw) -> str:
        return self.transcripts.today_block(max_chars, **kw)

    def star(self, text: str):
        self.starred.append(text)
        return f"- {text} (2026-09-23)"

    def tools(self, *, voice: bool = False) -> list[dict]:
        return _memory_schemas(voice)

    def handle_tool(self, name, args, *, since=None, channel=""):
        self.calls.append((name, dict(args), since, channel))
        if self.handle is not None:
            return self.handle(name, args)
        if name == "recent_conversation":
            return {"ok": True, "lines": [render_line(ln) for ln in
                                          self.transcripts.lines_since(since, exclude_ch=channel)]}
        return {"ok": True, "hits": []}

    def lines(self, conv: str) -> list[dict]:
        return list(self.transcripts.conv_lines(conv)) if conv else []


def _run_until_output(conn: FakeConnection, s: VoiceSession, n: int = 1) -> None:
    async def go():
        task = asyncio.create_task(s.run())
        await _eventually(lambda: len(conn.tool_outputs()) >= n, timeout=5.0)
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())


FRONT_TOOLS = ["lesson", "start_task", "steer_task", "stop_task", "task_status", "answer_question", "go_explore",
               "end_conversation", "look", "move_head", "look_around", "find", "take_photo", "set_sound",
               "think_hard"]


def test_with_no_memory_both_prompts_are_byte_identical_to_a_memory_less_session(monkeypatch, tmp_path) -> None:
    """The pin: memory off (or every block empty) gives the exact prompt of the build before memory."""
    monkeypatch.setattr(system_context, "context", lambda: "\n\nCLOCK")
    for out in ("captions", "audio"):
        cfg = VoiceConfig(output=out)
        caps = caption_instructions(PagerConfig(read_cps=cfg.caption_cps)) if out == "captions" else ""
        got = session_config(cfg)
        assert got["instructions"] == INSTRUCTIONS + "\n\nCLOCK" + caps
        assert got["delegation"]["responses"]["instructions"] == BACKEND_INSTRUCTIONS + "\n\nCLOCK"
        assert got["delegation"]["responses"]["tools"] == TOOLS + websearch.tools_for(cfg.search)
        assert session_config(cfg, "", None, "", backend_profile=" ", today="", backend_today="\n",
                              memory_tools=[]) == got
        # the brief keeps its place, right after the clock
        assert (session_config(cfg, "You last talked yesterday.")["instructions"]
                == INSTRUCTIONS + "\n\nCLOCK" + memory_block("You last talked yesterday.") + caps)
    # and a real session with no memory starts with exactly that, writing nothing anywhere
    conn = FakeConnection([_tool_call("end_conversation"), None])
    s, _, _ = _session(conn, [FakeAgent(None, None)])
    s._close_turn("user", "hello there eve")
    asyncio.run(s.run())
    started = [kw["session"] for name, kw in conn.sent if name == "session.start"]
    assert started == [session_config(s.config)]
    assert s.conv == "" and list(tmp_path.iterdir()) == []


def test_the_live_front_tool_list_is_unchanged_and_the_front_has_no_memory_tools() -> None:
    assert [t["name"] for t in TOOLS] == FRONT_TOOLS
    cfg = session_config(VoiceConfig(output="audio"), memory_tools=_memory_schemas(True))
    assert "tools" not in cfg                                   # the Live front itself calls nothing
    back = cfg["delegation"]["responses"]["tools"]
    assert back[:len(TOOLS)] == TOOLS and [t["name"] for t in back[-5:]] == MEMORY_TOOL_NAMES + ["recent_conversation"]


def test_memory_blocks_go_before_the_clock_and_the_backend_gets_the_full_profile_today_and_rule(monkeypatch) -> None:
    monkeypatch.setattr(system_context, "context", lambda: "\n\nCLOCK")
    cfg = session_config(VoiceConfig(output="audio"), "BRIEF-LINE", None, "VOICE-PAGE", backend_profile="FULL-PAGE",
                         today="10:00 (texted) Owner: TODAY-FRONT", backend_today="10:00 (texted) Owner: TODAY-BACK",
                         memory_tools=_memory_schemas(True))
    front = cfg["instructions"]
    assert front.startswith(INSTRUCTIONS)
    order = [front.index(x) for x in ("VOICE-PAGE", VOICE_TODAY_HEADER, "TODAY-FRONT", "CLOCK", "BRIEF-LINE")]
    assert order == sorted(order)
    assert "FULL-PAGE" not in front and "TODAY-BACK" not in front and "memory_search" not in front
    back = cfg["delegation"]["responses"]["instructions"]
    assert back.startswith(BACKEND_INSTRUCTIONS + BACKEND_MEMORY_RULES)
    assert "memory tools first" in back
    order = [back.index(x) for x in ("FULL-PAGE", "TODAY-BACK", "CLOCK")]
    assert order == sorted(order)
    assert "VOICE-PAGE" not in back and "TODAY-FRONT" not in back and "BRIEF-LINE" not in back
    # the rule rides with the tools: no tools, no rule
    assert "memory tools first" not in session_config(VoiceConfig(output="audio"), backend_profile="FULL-PAGE"
                                                      )["delegation"]["responses"]["instructions"]


def test_think_aloud_gets_no_memory_blocks_and_no_memory_tools() -> None:
    kw = dict(backend_profile="FULL-PAGE", today="TODAY-FRONT", backend_today="TODAY-BACK",
              memory_tools=_memory_schemas(True))
    listening = session_config(VoiceConfig(output="audio"), "BRIEF-LINE", LESSON, "VOICE-PAGE", **kw)
    plain = session_config(VoiceConfig(output="audio"), think_aloud=LESSON)
    assert listening == plain
    text = json.dumps(listening)
    for word in ("BRIEF-LINE", "VOICE-PAGE", "FULL-PAGE", "TODAY-FRONT", "TODAY-BACK", "memory_search"):
        assert word not in text
    # positive control: the same blocks outside think out loud are there
    assert "TODAY-BACK" in json.dumps(session_config(VoiceConfig(output="audio"), **kw))


def test_a_session_with_memory_offers_the_memory_tools_and_logs_its_prompt_sizes(tmp_path, caplog) -> None:
    caplog.set_level("INFO")
    memory = FakeMemory(tmp_path, profile="FULL-PAGE", voice_profile="VOICE-PAGE")
    conn = FakeConnection([_tool_call("end_conversation"), None])
    s, _, _ = _session(conn, [FakeAgent(None, None)], memory=memory, today="TODAY-FRONT", backend_today="TODAY-BACK",
                       brief="BRIEF-LINE")
    asyncio.run(s.run())
    started = [kw["session"] for name, kw in conn.sent if name == "session.start"][0]
    names = [t.get("name") for t in started["delegation"]["responses"]["tools"]]
    assert names[-5:] == MEMORY_TOOL_NAMES + ["recent_conversation"]
    assert "VOICE-PAGE" in started["instructions"] and "TODAY-FRONT" in started["instructions"]
    assert "FULL-PAGE" in started["delegation"]["responses"]["instructions"]
    assert "TODAY-BACK" in started["delegation"]["responses"]["instructions"]
    assert (f"voice: prompt front {len(started['instructions'])} chars, backend "
            f"{len(started['delegation']['responses']['instructions'])} chars") in caplog.text
    # control: no memory, no memory tools
    conn = FakeConnection([_tool_call("end_conversation"), None])
    s, _, _ = _session(conn, [FakeAgent(None, None)])
    asyncio.run(s.run())
    started = [kw["session"] for name, kw in conn.sent if name == "session.start"][0]
    assert "memory_search" not in [t.get("name") for t in started["delegation"]["responses"]["tools"]]


def test_closed_turns_are_written_as_say_lines_of_one_conversation(tmp_path) -> None:
    memory = FakeMemory(tmp_path)
    s, _, _ = _session(FakeConnection([]), [FakeAgent(None, None)], memory=memory)

    async def go():
        s._close_turn("user", "I moved the dentist to Thursday.")
        s._close_turn("assistant", "Thursday it is.")
    asyncio.run(go())
    lines = memory.lines(s.conv)
    assert [(ln["ch"], ln["who"], ln["kind"], ln["text"]) for ln in lines] == [
        ("voice", "owner", "say", "I moved the dentist to Thursday."), ("voice", "eve", "say", "Thursday it is.")]
    assert s.conv.startswith("v-")
    given, _, _ = _session(FakeConnection([]), [FakeAgent(None, None)], memory=memory, conv="v-1-abcd")
    assert given.conv == "v-1-abcd"


def test_a_think_aloud_session_writes_nothing_to_the_transcript(tmp_path) -> None:
    memory = FakeMemory(tmp_path)
    conn = FakeConnection([])
    s, _, _, saved = _listening(conn, {"now": 0.0}, memory=memory)

    async def go():
        s._close_turn("user", "I think x is four")
        s._close_turn("assistant", "What makes you say four?")
        await asyncio.sleep(0.02)
    asyncio.run(go())
    assert saved and memory.lines(s.conv) == [] and memory.transcripts.days() == []


def test_a_web_search_result_is_kept_as_one_clipped_tool_line_and_the_close_marker_follows(tmp_path,
                                                                                         monkeypatch) -> None:
    monkeypatch.setattr(websearch, "search", lambda q, cfg: {"ok": True, "answer": "A" * 3000})
    memory = FakeMemory(tmp_path)
    conn = FakeConnection([_tool_call(websearch.TOOL_NAME, "c1", query="the forecast")])
    s, _, _ = _session(conn, [FakeAgent(None, None)], memory=memory)
    _run_until_output(conn, s)
    lines = memory.lines(s.conv)
    tool = [ln for ln in lines if ln["kind"] == "tool"]
    assert len(tool) == 1 and tool[0]["tool"] == websearch.TOOL_NAME and tool[0]["who"] == "eve"
    assert tool[0]["text"] == "A" * 1500
    assert [ln["kind"] for ln in lines].count("close") == 1 and lines[-1]["kind"] == "close"
    # a failed search is not an answer: nothing kept, and a conversation that kept nothing gets no marker
    monkeypatch.setattr(websearch, "search", lambda q, cfg: {"ok": False, "reason": "the search timed out"})
    memory = FakeMemory(tmp_path / "second")
    conn = FakeConnection([_tool_call(websearch.TOOL_NAME, "c1", query="the forecast")])
    s, _, _ = _session(conn, [FakeAgent(None, None)], memory=memory)
    _run_until_output(conn, s)
    assert memory.lines(s.conv) == []


def test_a_finished_task_result_is_kept_as_a_tool_line(tmp_path) -> None:
    memory = FakeMemory(tmp_path)
    agent = FakeAgent(None, None, final="Mail is open.")
    conn = FakeConnection([_tool_call("start_task", "c1", goal="open Mail")])
    s, _, _ = _session(conn, [agent], memory=memory)

    async def go():
        task = asyncio.create_task(s.run())
        await _eventually(lambda: bool(conn.tool_outputs()))
        agent.release.set()
        await _eventually(lambda: agent.final is not None)
        await asyncio.sleep(0.02)
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    tool = [ln for ln in memory.lines(s.conv) if ln["kind"] == "tool"]
    assert [(ln["tool"], ln["text"]) for ln in tool] == [("start_task", "Mail is open.")]


def test_a_hush_keeps_the_open_turn_and_writes_the_close_marker(tmp_path) -> None:
    memory = FakeMemory(tmp_path)
    conn = FakeConnection([])
    s, _, _ = _session(conn, [FakeAgent(None, None)], memory=memory)

    async def go():
        task = asyncio.create_task(s.run())
        await _eventually(lambda: s._started.is_set())
        conn.feed(_heard("the blue one, not the red"))
        await _eventually(lambda: bool(s._turns.text))
        task.cancel()                                   # a touch on the robot
        with pytest.raises(asyncio.CancelledError):
            await task
    asyncio.run(go())
    assert [(ln["who"], ln["kind"], ln["text"]) for ln in memory.lines(s.conv)] == [
        ("owner", "say", "the blue one, not the red"), ("system", "close", "")]


def test_recent_conversation_returns_a_text_sent_after_the_session_opened(tmp_path) -> None:
    """The cross-channel positive control: the Live instructions cannot change after session.start, so a
    text sent mid-conversation reaches voice only through this tool."""
    w = {"now": datetime(2026, 9, 23, 12, 0, 0, tzinfo=timezone.utc)}
    memory = FakeMemory(tmp_path, wall=lambda: w["now"])
    tg = memory.transcripts.new_conv("telegram")
    assert memory.transcripts.append("telegram", tg, "owner", "say", "BEFORE-LINE")
    w["now"] += timedelta(seconds=10)
    conn = FakeConnection([])
    s, _, _ = _session(conn, [FakeAgent(None, None)], memory=memory)

    async def go():
        task = asyncio.create_task(s.run())
        await _eventually(lambda: s._started.is_set())
        w["now"] += timedelta(seconds=10)
        assert memory.transcripts.append("telegram", tg, "owner", "say", "AFTER-LINE")
        conn.feed(_tool_call("recent_conversation", "c1"))
        await _eventually(lambda: bool(conn.tool_outputs()))
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    out = json.dumps(conn.tool_outputs()[0])
    assert "AFTER-LINE" in out and "BEFORE-LINE" not in out
    assert memory.calls == [("recent_conversation", {}, datetime(2026, 9, 23, 12, 0, 10, tzinfo=timezone.utc),
                             "voice")]


def test_a_slow_lookup_returns_the_timeout_line_in_about_2_5_s_while_the_loop_keeps_running(tmp_path) -> None:
    release = threading.Event()

    def stuck(name, args):
        release.wait(5.0)                                # a lookup that would take 5 s
        return {"ok": True, "hits": []}
    memory = FakeMemory(tmp_path, handle=stuck)
    s, _, _ = _session(FakeConnection([]), [FakeAgent(None, None)], memory=memory)

    async def go():
        ticks = 0

        async def ticker():
            nonlocal ticks
            while True:
                await asyncio.sleep(0.05)
                ticks += 1
        other = asyncio.create_task(ticker())
        t0 = time.monotonic()
        out = await s._memory_tool("memory_search", {"query": "dentist"})
        took = time.monotonic() - t0
        other.cancel()
        release.set()
        return out, took, ticks
    out, took, ticks = asyncio.run(go())
    assert out == {"ok": False, "reason": MEMORY_TIMEOUT_LINE}
    assert 2.3 <= took < 3.5
    assert ticks >= 30                                   # the other coroutine ran all along


def test_memory_results_are_clipped_to_valid_json_and_the_log_has_no_query(tmp_path, caplog) -> None:
    caplog.set_level("INFO")
    memory = FakeMemory(tmp_path, handle=lambda name, args: {
        "ok": True, "hits": [{"text": "word " * 40, "day": "2026-09-20"} for _ in range(50)]})
    conn = FakeConnection([_tool_call("memory_search", "c1", query="secret-query")])
    s, _, _ = _session(conn, [FakeAgent(None, None)], memory=memory)
    _run_until_output(conn, s)
    raw = [kw["item"]["output"] for k, kw in conn.sent
           if k == "response.item.create" and kw["item"]["type"] == "function_call_output"][0]
    assert len(raw) <= 1500 and json.loads(raw)["clipped"] is True and json.loads(raw)["ok"] is True
    assert memory.calls[0][:2] == ("memory_search", {"query": "secret-query"}) and memory.calls[0][3] == "voice"
    assert "voice: memory_search → ok=True hits=50 in" in caplog.text
    assert "secret-query" not in caplog.text
    tool = [ln for ln in memory.lines(s.conv) if ln["kind"] == "tool"]
    assert len(tool) == 1 and tool[0]["tool"] == "memory_search" and len(tool[0]["text"]) <= 1500
    # a small result goes through untouched
    memory = FakeMemory(tmp_path / "small")
    conn = FakeConnection([_tool_call("memory_search", "c1", query="q")])
    s, _, _ = _session(conn, [FakeAgent(None, None)], memory=memory)
    _run_until_output(conn, s)
    assert conn.tool_outputs()[0] == {"ok": True, "hits": []}


def test_memory_tools_are_refused_once_think_aloud_begins(tmp_path) -> None:
    memory = FakeMemory(tmp_path)
    conn = FakeConnection([])
    s, _, _ = _session(conn, [FakeAgent(None, None)], memory=memory)
    s._memory_tools()
    s._think_aloud = LESSON
    asyncio.run(s._tool("memory_search", "c1", '{"query": "q"}'))
    assert conn.tool_outputs()[0]["ok"] is False and memory.calls == []


def test_think_hard_passes_the_profile_and_today_as_context(tmp_path) -> None:
    memory = FakeMemory(tmp_path, profile="PROFILE-PAGE")
    tg = memory.transcripts.new_conv("telegram")
    memory.transcripts.append("telegram", tg, "owner", "say", "TODAY-TEXT")
    seen: dict = {}

    async def thinker(question: str, context: str = "") -> dict:
        seen.update(question=question, context=context)
        return {"ok": True, "answer": "Pasta."}
    s, _, _ = _session(FakeConnection([]), [FakeAgent(None, None)], thinker=thinker, memory=memory)
    assert asyncio.run(s._think_hard("what should I cook?")) == {"ok": True, "answer": "Pasta."}
    assert seen["question"] == "what should I cook?"
    assert "PROFILE-PAGE" in seen["context"] and "TODAY-TEXT" in seen["context"]


def test_remember_that_stars_the_owners_last_turn_through_memory(tmp_path) -> None:
    memory = FakeMemory(tmp_path)
    conn = FakeConnection([])
    s, _, _ = _session(conn, [FakeAgent(None, None)], memory=memory)

    async def go():
        s._close_turn("user", "My sister's birthday is in May.")
        s._close_turn("assistant", "Lovely, May it is then.")
        s._close_turn("user", "remember that")
        await _eventually(lambda: any("written that down" in t for t in _thinking(conn)))
    asyncio.run(go())
    assert memory.starred == ["My sister's birthday is in May."]


def test_remember_that_never_stars_eves_own_words(tmp_path, caplog) -> None:
    caplog.set_level("INFO")
    memory = FakeMemory(tmp_path)
    conn = FakeConnection([])
    s, _, _ = _session(conn, [FakeAgent(None, None)], memory=memory)

    async def go():
        s._close_turn("assistant", "Octopuses have three hearts, you know.")
        s._close_turn("user", "remember that")
        await _eventually(lambda: any("said nothing before it" in t for t in _thinking(conn)))
    asyncio.run(go())
    assert memory.starred == []
    # no memory lent: nothing to star with, and nothing breaks
    s, _, _ = _session(FakeConnection([]), [FakeAgent(None, None)])

    async def bare():
        s._close_turn("user", "My sister's birthday is in May.")
        s._close_turn("user", "remember that")
        await asyncio.sleep(0.02)
    asyncio.run(bare())
    assert "starred" not in caplog.text


# ---- the spend meter (spend.py) ------------------------------------------------------------------------

def test_the_live_minutes_and_each_backend_call_are_metered(_spend_ledger_in_tmp) -> None:
    from datetime import date

    from eve_bridge import spend

    conn = FakeConnection([
        {"type": "session.usage.updated", "usage": {"seconds": 30.0}},
        _delegated(),
        {"type": "response.event", "event": {"type": "response.output_item.done", "item": {"type": "web_search_call"}}},
        {"type": "response.event", "event": {"type": "response.completed", "response": {
            "model": "gpt-6-astra", "usage": {"input_tokens": 1_000_000, "output_tokens": 0}}}},
        {"type": "session.usage.updated", "usage": {"seconds": 90.0}},    # cumulative: the largest one wins
        {"type": "session.usage.updated", "usage": {"seconds": 60.0}},
    ])
    s, _states, _mic = _session(conn, [FakeAgent(None, None)])

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        conn.feed(_tool_call("end_conversation"), None)
        await task
    asyncio.run(go())
    rows = spend.day_rows(date.today().isoformat(), _spend_ledger_in_tmp)
    backend, live = rows
    assert (backend["f"], backend["m"], backend["usd"]) == ("voice", "gpt-6-astra", pytest.approx(10.0 + 0.01))
    assert backend["tok"]["searches"] == 1
    assert (live["f"], live["m"], live["usd"], live["tok"]) == ("voice", "gpt-live-1", pytest.approx(0.075),
                                                                {"secs": 90})


def test_a_session_without_usage_events_is_metered_by_its_own_clock(_spend_ledger_in_tmp) -> None:
    from datetime import date

    from eve_bridge import spend

    clock = {"now": 0.0}
    conn = FakeConnection()
    s, _states, _mic = _session(conn, [FakeAgent(None, None)], clock=clock)

    async def go():
        task = asyncio.create_task(s.run())
        await asyncio.sleep(0.01)
        clock["now"] = 120.0
        conn.feed(_tool_call("end_conversation"), None)
        await task
    asyncio.run(go())
    [row] = spend.day_rows(date.today().isoformat(), _spend_ledger_in_tmp)
    assert row["usd"] == pytest.approx(0.10) and row["note"] == "clock-timed: no usage event"


# ---- Meet: "join my meeting" out loud (meet.py) ---------------------------------------------------

class _FakeMeeter:
    def __init__(self) -> None:
        self.joined: list[str] = []
        self.left = 0

    async def join(self, meeting: str) -> dict:
        self.joined.append(meeting)
        return {"ok": True, "joining": "Design review"}

    def leave(self) -> dict:
        self.left += 1
        return {"ok": True, "leaving": "Design review"}


def test_the_meeting_tools_are_offered_only_with_a_meeter_and_never_in_a_lesson() -> None:
    from eve_bridge import voice_agent

    names = lambda cfg: [t.get("name") for t in cfg["delegation"]["responses"]["tools"]]  # noqa: E731
    cfg = VoiceConfig()
    assert "join_meeting" not in names(voice_agent.session_config(cfg))
    with_meet = names(voice_agent.session_config(cfg, meet_tools=voice_agent.MEET_TOOLS))
    assert "join_meeting" in with_meet and "leave_meeting" in with_meet
    lesson = names(voice_agent.session_config(cfg, think_aloud={"id": "l1", "topic": "fractions"},
                                              meet_tools=voice_agent.MEET_TOOLS))
    assert "join_meeting" not in lesson


def test_join_meeting_out_loud_reaches_the_meeter_and_leave_too() -> None:
    meeter = _FakeMeeter()
    conn = FakeConnection([_tool_call("join_meeting", "c1", meeting="my 3pm")])
    s, _, _ = _session(conn, [FakeAgent(None, None)], meeter=meeter)

    async def go():
        task = asyncio.create_task(s.run())
        await _eventually(lambda: bool(conn.tool_outputs()))
        # the join answered with an invite's title (outside words, v3 safety review): leaving is routine, no question
        conn.feed(_tool_call("leave_meeting", "c2"), None)
        await _eventually(lambda: len(conn.tool_outputs()) >= 2)
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    assert meeter.joined == ["my 3pm"] and meeter.left == 1
    assert conn.tool_outputs()[0] == {"ok": True, "joining": "Design review"}
    assert conn.tool_outputs()[1]["leaving"] == "Design review"


def test_join_meeting_without_a_meeter_says_why() -> None:
    conn = FakeConnection([_tool_call("join_meeting", "c1", meeting="now")])
    s, _, _ = _session(conn, [FakeAgent(None, None)])

    async def go():
        task = asyncio.create_task(s.run())
        await _eventually(lambda: bool(conn.tool_outputs()))
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    assert conn.tool_outputs()[0]["ok"] is False and "off" in conn.tool_outputs()[0]["reason"]


class _FakeLights:
    def __init__(self) -> None:
        self.calls: list = []

    def tools(self) -> list:
        from eve_bridge import lights
        return lights.TOOLS

    def instructions(self) -> str:
        return "The lights:\n- floor lamp"

    async def handle(self, name: str, args: dict) -> dict:
        self.calls.append((name, args))
        return {"ok": True, "done": ["floor lamp"], "failed": [], "change": "off"}


def test_the_lights_are_offered_to_the_backend_only_with_lights_and_never_in_a_lesson() -> None:
    from eve_bridge import voice_agent

    names = lambda cfg: [t.get("name") for t in cfg["delegation"]["responses"]["tools"]]  # noqa: E731
    cfg = VoiceConfig()
    assert "lights_set" not in names(voice_agent.session_config(cfg))
    on = voice_agent.session_config(cfg, lights=_FakeLights())
    assert {"lights_set", "lights_status"} <= set(names(on))
    assert "floor lamp" in on["delegation"]["responses"]["instructions"]
    lesson = voice_agent.session_config(cfg, think_aloud={"id": "l1", "topic": "fractions"}, lights=_FakeLights())
    assert "lights_set" not in names(lesson) and "floor lamp" not in lesson["delegation"]["responses"]["instructions"]


def test_lights_out_loud_reach_the_lights() -> None:
    hub = _FakeLights()
    args = {"target": "all", "power": False, "brightness": None, "color": None}
    conn = FakeConnection([_tool_call("lights_set", "c1", **args)])
    s, _, _ = _session(conn, [FakeAgent(None, None)], lights=hub)

    async def go():
        task = asyncio.create_task(s.run())
        await _eventually(lambda: bool(conn.tool_outputs()))
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    assert hub.calls == [("lights_set", args)]
    assert conn.tool_outputs()[0]["done"] == ["floor lamp"]


class _FakeSpotify:
    def __init__(self) -> None:
        self.calls: list = []

    def tools(self) -> list:
        from eve_bridge import spotify
        return spotify.TOOLS

    def instructions(self) -> str:
        from eve_bridge import spotify
        return spotify.INSTRUCTIONS

    async def handle(self, name: str, args: dict) -> dict:
        self.calls.append((name, args))
        return {"ok": True, "line": "Paused."}


def test_spotify_is_offered_to_the_backend_only_when_logged_in_and_never_in_a_lesson() -> None:
    from eve_bridge import voice_agent

    names = lambda cfg: [t.get("name") for t in cfg["delegation"]["responses"]["tools"]]  # noqa: E731
    cfg = VoiceConfig()
    assert "spotify_control" not in names(voice_agent.session_config(cfg))
    on = voice_agent.session_config(cfg, spotify=_FakeSpotify())
    assert {"spotify_now_playing", "spotify_control"} <= set(names(on))
    assert "spotify_control" in on["delegation"]["responses"]["instructions"]
    lesson = voice_agent.session_config(cfg, think_aloud={"id": "l1", "topic": "fractions"}, spotify=_FakeSpotify())
    assert "spotify_control" not in names(lesson)


def test_music_out_loud_reaches_spotify() -> None:
    hub = _FakeSpotify()
    args = {"action": "pause", "query": None, "kind": None, "volume": None, "volume_change": None, "device": None}
    conn = FakeConnection([_tool_call("spotify_control", "c1", **args)])
    s, _, _ = _session(conn, [FakeAgent(None, None)], spotify=hub)

    async def go():
        task = asyncio.create_task(s.run())
        await _eventually(lambda: bool(conn.tool_outputs()))
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    assert hub.calls == [("spotify_control", args)]
    assert conn.tool_outputs()[0]["line"] == "Paused."


class _FakeTV:
    def __init__(self) -> None:
        self.calls: list = []

    def tools(self) -> list:
        from eve_bridge import firetv
        return firetv.TOOLS

    def instructions(self, screenshot: bool = True) -> str:
        from eve_bridge import firetv
        return firetv.INSTRUCTIONS if screenshot else firetv.INSTRUCTIONS_NO_SCREENSHOT

    async def handle(self, name: str, args: dict) -> dict:
        self.calls.append((name, args))
        return {"ok": True, "line": "TV volume 30%."}


def test_the_tv_is_offered_out_loud_only_when_paired_never_in_a_lesson_and_without_the_screenshot() -> None:
    from eve_bridge import voice_agent

    names = lambda cfg: [t.get("name") for t in cfg["delegation"]["responses"]["tools"]]  # noqa: E731
    cfg = VoiceConfig()
    off = voice_agent.session_config(cfg)
    assert not {"firetv_status", "firetv_control", "firetv_screenshot"} & set(names(off))
    assert "firetv_control" not in off["delegation"]["responses"]["instructions"]
    on = voice_agent.session_config(cfg, firetv=_FakeTV())
    assert {"firetv_status", "firetv_control"} <= set(names(on))
    assert "firetv_screenshot" not in names(on)            # its picture goes to a chat; a voice has none
    assert "firetv_control" in on["delegation"]["responses"]["instructions"]
    assert voice_agent.VOICE_FIRETV_NOTE in on["delegation"]["responses"]["instructions"]
    assert "firetv_screenshot" not in on["delegation"]["responses"]["instructions"]   # no tool it doesn't have
    lesson = voice_agent.session_config(cfg, think_aloud={"id": "l1", "topic": "fractions"}, firetv=_FakeTV())
    assert not {"firetv_status", "firetv_control", "firetv_screenshot"} & set(names(lesson))
    assert "firetv_control" not in lesson["delegation"]["responses"]["instructions"]


def test_the_tv_out_loud_reaches_the_tv() -> None:
    hub = _FakeTV()
    args = {"action": "volume", "app": None, "query": None, "text": None, "volume": 30, "volume_change": None,
            "repeat": None}
    conn = FakeConnection([_tool_call("firetv_control", "c1", **args)])
    s, _, _ = _session(conn, [FakeAgent(None, None)], firetv=hub)

    async def go():
        task = asyncio.create_task(s.run())
        await _eventually(lambda: bool(conn.tool_outputs()))
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    assert hub.calls == [("firetv_control", args)]
    assert conn.tool_outputs()[0]["line"] == "TV volume 30%."


class _FakeCamera:
    def __init__(self) -> None:
        self.calls: list = []

    def tools(self) -> list:
        from eve_bridge import obsbot_panel
        return obsbot_panel.TOOLS

    def instructions(self) -> str:
        from eve_bridge import obsbot_panel
        return obsbot_panel.INSTRUCTIONS

    async def handle(self, name: str, args: dict) -> dict:
        self.calls.append((name, args))
        return {"ok": True, "pan": 20, "tilt": 0, "line": "Camera pointing 20° left."}


def test_the_camera_is_offered_out_loud_only_when_there_is_one_and_never_in_a_lesson() -> None:
    from eve_bridge import voice_agent

    names = lambda cfg: [t.get("name") for t in cfg["delegation"]["responses"]["tools"]]  # noqa: E731
    cfg = VoiceConfig()
    off = voice_agent.session_config(cfg)
    assert not {"camera_status", "camera_control"} & set(names(off))
    on = voice_agent.session_config(cfg, obsbot=_FakeCamera())
    assert {"camera_status", "camera_control"} <= set(names(on))
    assert "camera_control" in on["delegation"]["responses"]["instructions"]
    lesson = voice_agent.session_config(cfg, think_aloud={"id": "l1", "topic": "fractions"}, obsbot=_FakeCamera())
    assert not {"camera_status", "camera_control"} & set(names(lesson))


def test_the_camera_out_loud_reaches_the_camera() -> None:
    hub = _FakeCamera()
    args = {"action": "turn", "direction": "left", "degrees": 20, "pan": None, "tilt": None, "zoom": None,
            "zoom_change": None, "mode": None, "preset": None}
    conn = FakeConnection([_tool_call("camera_control", "c1", **args)])
    s, _, _ = _session(conn, [FakeAgent(None, None)], obsbot=hub)

    async def go():
        task = asyncio.create_task(s.run())
        await _eventually(lambda: bool(conn.tool_outputs()))
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    assert hub.calls == [("camera_control", args)]
    assert conn.tool_outputs()[0]["line"] == "Camera pointing 20° left."
