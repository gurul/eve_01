"""Replays of the counterexamples in verification/Eve/DeskCall.lean against the real code.

Each test is one trace from that file's ``current_violates`` theorem, driven through the real classes with fakes
(no board, no microphone, no OpenAI). Each failed on the code before its fix.

  P2  A phone call starts during a desk call (PhoneCalls._serve does not ask about the desk). When the desk call
      ends, its Call.run cleared the chat's one listener, which was the phone's: the rest of the phone call was
      texted, not spoken. The real TelegramInlet holds the listener here.
  P1  A tap on the board while the "listening" ring is still being sent: the release ran first, and the press
      then opened the Mac microphone with the button up. Nothing closed it until the call ended.
  P1  A release during a wake-word conversation was dropped with the press, so the microphone stayed open.
  P3  The desk call ended while the button was still held, and the wake word came back mid-hold.
  CallScope.lean `current_hearing_call` (the hearing call): a phone call took the listener while a desk call was on;
      the desk's `listening` stayed True, so its next words were marked with the PHONE's reader and answered on
      the phone. Words are now marked with the call that heard them, and that call takes the listener back.

Async tests use an explicit asyncio.run, the repo convention.
"""
from __future__ import annotations

import asyncio
import json
from types import MethodType
from typing import Any

from test_daemon_touch import _Conversation, _daemon
from test_desk_call import Mic, Speaker, Voice
from test_telegram import OWNER, FakeApi, FakeCreate, Rig, say, settle

from eve_bridge import desk_call, phone_call, telegram
from eve_bridge.daemon import Daemon


def open_mics() -> list[Mic]:
    return [m for m in Mic.made if m.opened and not m.closed]


def test_a_desk_call_that_ends_leaves_the_phone_calls_listener() -> None:
    """DeskCall.lean `p2Trace`: desk press, phone call, desk call ends. The phone call is still on, so the chat must
    still read out to it."""
    rig = Rig(FakeApi(), FakeCreate(*[say("ok") for _ in range(4)]))
    rig.inlet._chat_id = OWNER

    async def go() -> None:
        sent: list[dict[str, Any]] = []

        async def send(obj: dict[str, Any]) -> None:
            sent.append(obj)

        Mic.made.clear()
        desk = desk_call.DeskCalls(rig.inlet, Voice(), send, busy=lambda: phones.active is not None,
                                   mic_factory=Mic, speaker_factory=Speaker)
        phones = phone_call.PhoneCalls(lambda _init: OWNER, rig.inlet, Voice())

        await desk.press(True)                                   # the desk call takes the chat's listener
        await settle()
        assert desk.active and rig.inlet._call_say is not None

        # The owner calls from the phone. A DeskLink stands in for the phone's WebSocket: it is exactly what
        # Call needs, and the hello is the first message, as the Mini App sends it.
        ws = desk_call.DeskLink(send, Speaker())
        ws.inbox.put_nowait((phone_call.OP_TEXT, json.dumps({"initData": "signed"}).encode()))
        serving = asyncio.ensure_future(phones._serve(ws))  # type: ignore[arg-type]
        await settle()
        phone = phones.active
        # An open phone call does not take the chat until its first press; the model's `phoneStart` is that
        # press.
        assert phone is not None and rig.inlet._call_say is not None and rig.inlet._call_say != phone.say
        ws.inbox.put_nowait((phone_call.OP_TEXT, json.dumps({"type": "talk"}).encode()))
        await settle()
        assert rig.inlet._call_say == phone.say

        await desk.press(False)
        await desk.stop()                                        # the desk call ends (quiet, or stopped)
        await settle()
        assert not desk.active and phones.active is phone
        assert rig.inlet._call_say == phone.say, "the desk call's end took the phone call's listener"

        await ws.close()
        await asyncio.gather(serving, return_exceptions=True)
        await settle()
        assert rig.inlet._call_say is None                       # the phone call's own end clears it

    asyncio.run(go())


def test_a_tap_during_the_ring_send_leaves_the_microphone_closed() -> None:
    """DeskCall.lean `p1TapTrace`: the press sets the ring to listening, the send waits on the serial link, the
    release arrives, then the press resumes. The microphone must not be open with the button up."""
    async def go() -> None:
        gate = asyncio.Event()

        async def send(obj: dict[str, Any]) -> None:
            if obj == {"cmd": "agent", "state": "listening"}:
                await gate.wait()                                # the serial link is busy

        Mic.made.clear()
        desk = desk_call.DeskCalls(_Brain(), Voice(), send, mic_factory=Mic, speaker_factory=Speaker)
        down = asyncio.ensure_future(desk.press(True))
        await settle()
        await desk.press(False)                                  # the release: its own message, its own task
        gate.set()
        await down
        await settle()
        assert not desk.held
        assert open_mics() == [], "the microphone is open with the button up"
        await desk.stop()

    asyncio.run(go())


class _Brain:
    def __init__(self) -> None:
        self.say: Any = None

    def listen(self, say: Any, owner: Any = None) -> bool:
        if say is not None or owner is None or self.say == owner:
            self.say = say
        return True

    def hear(self, text: str) -> None:
        pass


def daemon_with_desk() -> Any:
    d = _daemon()

    async def send(obj: dict[str, Any]) -> None:
        pass

    Mic.made.clear()
    d._desk_calls = desk_call.DeskCalls(_Brain(), Voice(), send, mic_factory=Mic, speaker_factory=Speaker)
    for name in ("_desk_calls_get", "_wake_suppressed"):
        setattr(d, name, MethodType(getattr(Daemon, name), d))
    return d


def test_a_release_during_a_conversation_still_closes_the_microphone() -> None:
    """DeskCall.lean `p1ConvTrace`: press, a conversation opens (a think-aloud lesson needs no wake word),
    release. The release must reach the desk."""
    async def go() -> None:
        d = daemon_with_desk()
        await d._handle_ble({"cmd": "ptt", "on": True})
        assert len(open_mics()) == 1
        d._conversation = _Conversation()
        await d._handle_ble({"cmd": "ptt", "on": False})
        assert open_mics() == [], "the release was dropped: the microphone is still open"
        await d._desk_calls.stop()

    asyncio.run(go())


def test_the_wake_word_stays_off_while_the_button_is_held_after_the_call_ends() -> None:
    """DeskCall.lean `p3Trace`: press, then the desk call ends with the button still down (here a stop; live,
    a press that lands while the quiet end's cleanup awaits). The wake word stays off until the release."""
    async def go() -> None:
        d = daemon_with_desk()
        await d._handle_ble({"cmd": "ptt", "on": True})
        await d._desk_calls.stop()
        assert not d._desk_calls.active
        assert d._wake_suppressed(), "the button is held, yet the wake word is back on"
        await d._handle_ble({"cmd": "ptt", "on": False})
        assert not d._wake_suppressed()

    asyncio.run(go())


def test_an_ending_call_clears_only_its_own_listener() -> None:
    """TelegramInlet.listen, the guard on its own: ``listen(None, owner=say)`` clears only ``say``."""
    rig = Rig(FakeApi(), FakeCreate(*[say("ok") for _ in range(4)]))
    rig.inlet._chat_id = OWNER
    desk_said: list[str] = []
    phone_said: list[str] = []

    async def go() -> None:
        inlet = rig.inlet
        assert inlet.listen(desk_said.append)
        assert inlet.listen(phone_said.append)                  # a second call takes the chat
        assert inlet.listen(None, owner=desk_said.append)       # the first ends: not its listener any more
        assert inlet._call_say == phone_said.append
        telegram._FROM_CALL.set(phone_said.append)  # the call's own work: only that is read on it (CallScope.lean)
        inlet._to_call(OWNER, "still on the phone", None)
        assert phone_said == ["still on the phone"] and desk_said == []
        assert inlet.listen(None, owner=phone_said.append)
        assert inlet._call_say is None
        await settle()

    asyncio.run(go())


def drain(q: "asyncio.Queue[str]") -> list[str]:
    out: list[str] = []
    while not q.empty():
        out.append(q.get_nowait())
    return out


def test_words_heard_on_the_desk_are_read_on_the_desk_while_the_phone_holds_the_listener() -> None:
    """CallScope.lean `hearingTrace` [callStart, callStart, heardOn 1, spawn 0, reply 1]: listen(desk), listen(phone),
    then the desk hears "what time is it". Its answer is read on the desk, not on the phone."""
    rig = Rig(FakeApi(), FakeCreate(say("ok"), say("ok"), say("ok")))   # the warm-ups
    rig.inlet._chat_id = OWNER

    async def go() -> None:
        async def send(obj: dict[str, Any]) -> None:
            pass

        desk = phone_call.Call(desk_call.DeskLink(send, Speaker()), rig.inlet, Voice())  # type: ignore[arg-type]
        phone = phone_call.Call(desk_call.DeskLink(send, Speaker()), rig.inlet, Voice())  # type: ignore[arg-type]
        assert desk._take_chat()
        await settle()
        assert phone._take_chat()                                   # the phone's first press takes the chat
        await settle()
        assert desk.listening and rig.inlet._call_say == phone.say  # the desk still thinks it holds it
        await desk.heard(b"\x01\x00" * phone_call.SAMPLE_RATE, False, 0.0)    # a one-second press on the desk
        await settle()
        desk_read, phone_read = drain(desk.lines), drain(phone.lines)
        assert phone_read == [], f"the desk's words were answered on the phone: {phone_read}"
        assert rig.inlet._call_say == desk.say, "the hearing call did not take the listener back"
        assert len(desk_read) == 1, desk_read                     # the answer (the clock answers by code)

    asyncio.run(go())
