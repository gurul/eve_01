"""desk_call.py: the board's hold-to-talk button runs the Mini App's push-to-talk call on the desk. A fake chat,
a fake voice (no OpenAI), a fake microphone that the test feeds, and a fake speaker that records what it
played."""
from __future__ import annotations

import asyncio
import functools
from typing import Any

import pytest

from eve_bridge import desk_call, phone_call


class Brain:
    def __init__(self, reply: str = "It is **three** o'clock.") -> None:
        self.reply, self.heard, self.say = reply, [], None

    def listen(self, say, owner=None) -> bool:
        if say is not None or owner is None or self.say == owner:    # as TelegramInlet.listen
            self.say = say
        return True

    def hear(self, text: str) -> None:
        self.heard.append(text)
        if self.say is not None:
            self.say(self.reply)


class Voice:
    def __init__(self) -> None:
        self.transcribed: list[int] = []
        self.spoken: list[str] = []

    def transcribe(self, pcm: bytes) -> str:
        self.transcribed.append(len(pcm))
        return "what time is it"

    def speak_stream(self, text: str):
        self.spoken.append(text)
        yield b"\x01\x00" * 2400

    def live_ears(self):
        return None


class Mic:
    made: list["Mic"] = []

    def __init__(self, device, on_block) -> None:
        self.device, self.on_block, self.opened, self.closed = device, on_block, False, False
        Mic.made.append(self)

    def open(self) -> bool:
        self.opened = True
        return True

    def close(self) -> None:
        self.closed = True


class Speaker:
    def __init__(self) -> None:
        self.played = bytearray()
        self.flushes = 0
        self.closed = False

    def play(self, pcm: bytes) -> None:
        self.played.extend(pcm)

    def flush(self) -> None:
        self.flushes += 1

    def close(self) -> None:
        self.closed = True


async def until(pred, secs: float = 5.0) -> None:
    async with asyncio.timeout(secs):
        while not pred():
            await asyncio.sleep(0.01)


def calls(brain=None, voice=None, busy=lambda: False):
    sent: list[dict[str, Any]] = []
    speakers: list[Speaker] = []

    async def send(obj: dict[str, Any]) -> None:
        sent.append(obj)

    def speaker() -> Speaker:
        speakers.append(Speaker())
        return speakers[-1]

    Mic.made.clear()
    desk = desk_call.DeskCalls(brain, voice, send, mic_device="yeti", busy=busy,
                               mic_factory=Mic, speaker_factory=speaker)
    return desk, sent, speakers


def run(test):
    """Plain pytest (no async plugin here, as in test_phone_call.py): each test runs its own loop."""
    @functools.wraps(test)
    def sync(*args: Any, **kwargs: Any) -> None:
        asyncio.run(test(*args, **kwargs))
    return sync


def rings(sent: list[dict[str, Any]]) -> list[str]:
    return [m["state"] for m in sent if m.get("cmd") == "agent"]


@run
async def test_hold_talk_release_is_one_message_to_the_chat_and_the_reply_plays_on_the_mac() -> None:
    brain, voice = Brain(), Voice()
    desk, sent, speakers = calls(brain, voice)

    await desk.press(True)
    mic = Mic.made[-1]
    assert mic.opened and mic.device == "yeti"
    # one second of the owner talking, from the mic thread
    for _ in range(20):
        mic.on_block(b"\x02\x00" * 1200)
    await asyncio.sleep(0.05)
    await desk.press(False)
    assert mic.closed

    await until(lambda: len(speakers[0].played) > 0)
    assert brain.heard == ["what time is it"]
    assert voice.transcribed == [phone_call.BYTES_PER_SEC]            # the whole press, uploaded
    assert voice.spoken and "three" in voice.spoken[0] and "**" not in voice.spoken[0]
    await until(lambda: "speaking" in rings(sent))
    assert rings(sent)[:3] == ["listening", "thinking", "speaking"]

    await desk.stop()
    assert rings(sent)[-1] == "idle" and speakers[0].closed


@run
async def test_a_tap_is_not_words() -> None:
    brain, voice = Brain(), Voice()
    desk, sent, _ = calls(brain, voice)
    await desk.press(True)
    Mic.made[-1].on_block(b"\x00\x00" * 1200)                         # 50 ms
    await asyncio.sleep(0.05)
    await desk.press(False)
    await asyncio.sleep(0.1)
    assert brain.heard == [] and voice.transcribed == []
    await desk.stop()


@run
async def test_a_press_while_eve_talks_interrupts_it() -> None:
    brain, voice = Brain(), Voice()
    desk, _, speakers = calls(brain, voice)
    await desk.press(True)
    for _ in range(10):
        Mic.made[-1].on_block(b"\x02\x00" * 1200)
    await asyncio.sleep(0.05)
    await desk.press(False)
    await until(lambda: len(speakers[0].played) > 0)
    flushes = speakers[0].flushes
    await desk.press(True)                                             # same call, a second press
    await until(lambda: speakers[0].flushes > flushes)
    assert len(Mic.made) == 2 and len(speakers) == 1
    await desk.press(False)
    await desk.stop()


@run
async def test_the_desk_waits_while_the_phone_is_on_a_call() -> None:
    brain, voice = Brain(), Voice()
    desk, sent, _ = calls(brain, voice, busy=lambda: True)
    await desk.press(True)
    assert Mic.made == [] and not desk.active and sent == []
    await desk.press(False)


@run
async def test_without_the_chat_or_voice_the_button_does_nothing() -> None:
    desk, sent, _ = calls(None, None)
    await desk.press(True)
    await desk.press(False)
    assert Mic.made == [] and sent == [] and not desk.active


@run
async def test_the_call_stays_open_while_eve_is_still_thinking(monkeypatch: pytest.MonkeyPatch) -> None:
    # A chat that has not answered yet (a slow tool): the ring says thinking, the desk keeps pinging, and the
    # call outlives QUIET_SECS instead of ending as "the phone went quiet".
    monkeypatch.setattr(phone_call, "QUIET_SECS", 0.3)
    monkeypatch.setattr(desk_call, "PING_SECS", 0.1)
    brain, voice = Brain(reply=""), Voice()
    desk, sent, _ = calls(brain, voice)
    await desk.press(True)
    for _ in range(10):
        Mic.made[-1].on_block(b"\x02\x00" * 1200)
    await asyncio.sleep(0.05)
    await desk.press(False)
    await until(lambda: brain.heard == ["what time is it"])
    await asyncio.sleep(1.0)
    assert desk.active and rings(sent)[-1] == "thinking"
    await desk.stop()


@run
async def test_once_eve_is_done_the_call_ends_when_quiet(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(phone_call, "QUIET_SECS", 0.3)
    monkeypatch.setattr(desk_call, "PING_SECS", 0.1)
    desk, sent, _ = calls(Brain(), Voice())
    await desk.press(True)
    for _ in range(10):
        Mic.made[-1].on_block(b"\x02\x00" * 1200)
    await asyncio.sleep(0.05)
    await desk.press(False)
    await until(lambda: not desk.active)
    assert rings(sent)[-1] == "idle"



# ---- the head follows whoever is talking on a desk call (follow.py) ---------------------------------

@run
async def test_a_desk_call_is_a_conversation_to_the_head_follower(monkeypatch: pytest.MonkeyPatch) -> None:
    """2026-09-27: "Eve doesn't follow person anymore". Voice had moved to the Voice PE's button, and the
    call's states went only to the ring, so the follower (which needs a conversation phase) never woke."""
    monkeypatch.setattr(phone_call, "QUIET_SECS", 0.3)
    monkeypatch.setattr(desk_call, "PING_SECS", 0.1)
    phases: list[str] = []

    async def on_phase(phase: str) -> None:
        phases.append(phase)

    sent: list[dict[str, Any]] = []

    async def send(obj: dict[str, Any]) -> None:
        sent.append(obj)

    desk = desk_call.DeskCalls(Brain(), Voice(), send, mic_factory=Mic, speaker_factory=Speaker, on_phase=on_phase)
    await desk.press(True)
    assert phases == ["listening"]
    for _ in range(10):
        Mic.made[-1].on_block(b"\x02\x00" * 1200)
    await asyncio.sleep(0.05)
    await desk.press(False)
    await until(lambda: not desk.active)
    assert phases[:3] == ["listening", "thinking", "speaking"]
    # after the reply the ring goes quiet, but the person is still there until the call ends
    assert phases[-2:] == ["listening", "idle"]
    assert "idle" not in phases[:-1]


@run
async def test_during_a_desk_call_eve_turns_to_the_face_it_sees() -> None:
    from eve_bridge.follow import SpeakerFollower
    from eve_bridge.vision import FaceResult

    t = [100.0]
    looks: list[dict[str, Any]] = []

    async def board(obj: dict[str, Any]) -> bool:
        if obj.get("cmd") == "look":
            looks.append(obj)
        return True

    follower = SpeakerFollower(board, clock=lambda: t[0])
    desk = desk_call.DeskCalls(Brain(), Voice(), board, mic_factory=Mic, speaker_factory=Speaker,
                               on_phase=follower.on_phase)
    await desk.press(True)
    assert follower.active
    for _ in range(12):                      # the owner sits well to eve's left of where the head points
        t[0] += 0.25
        await follower.on_faces([FaceResult(bx=-60, by=10, size=22, conf=90)], 0.0, 45.0)
    assert looks and looks[0]["yaw"] < -10
    await desk.stop()
    assert not follower.active and follower.phase == "idle"

# ---- eve's voice on the Voice PE (BoardSpeaker) ---------------------------------------------------

class Clock:
    def __init__(self) -> None:
        self.t = 100.0
        self.slept: list[float] = []

    def __call__(self) -> float:
        return self.t

    async def sleep(self, secs: float) -> None:
        self.slept.append(secs)
        self.t += secs


def board(clock: Clock):
    lines: list[dict[str, Any]] = []

    async def send(obj: dict[str, Any]) -> None:
        lines.append(obj)

    return desk_call.BoardSpeaker(send, clock=clock, sleep=clock.sleep), lines


@run
async def test_the_voice_goes_to_the_board_in_decodable_pieces() -> None:
    import base64
    clock = Clock()
    spk, lines = board(clock)
    pcm = bytes(range(256)) * 40                                   # 10240 bytes: 4800 + 4800 + 640
    await spk.play(pcm)
    assert [len(base64.b64decode(line["d"])) for line in lines] == [4800, 4800, 640]
    assert b"".join(base64.b64decode(line["d"]) for line in lines) == pcm
    assert all(line["cmd"] == "pcm" for line in lines)


@run
async def test_the_daemon_stays_at_most_three_seconds_ahead_of_the_board() -> None:
    clock = Clock()
    spk, lines = board(clock)
    ten_secs = b"\x00\x00" * phone_call.SAMPLE_RATE * 10
    await spk.play(ten_secs)
    assert len(lines) == 100
    assert spk.ahead() <= desk_call.LEAD_SECS + 1e-9
    # 10 s sent with a 3 s lead: about 7 s of waiting, never a burst the 12 s board buffer cannot hold
    assert 6.8 <= sum(clock.slept) <= 7.1


@run
async def test_after_a_pause_the_board_has_played_out_and_a_new_burst_starts() -> None:
    clock = Clock()
    spk, _ = board(clock)
    await spk.play(b"\x00\x00" * phone_call.SAMPLE_RATE)          # 1 s
    clock.t += 5                                                    # the board played it out long ago
    assert spk.ahead() == 0.0
    await spk.play(b"\x00\x00" * phone_call.SAMPLE_RATE)
    assert clock.slept == [] and abs(spk.ahead() - 1.0) < 1e-9


@run
async def test_a_flush_empties_the_board() -> None:
    clock = Clock()
    spk, lines = board(clock)
    await spk.play(b"\x00\x00" * 4800)
    await spk.flush()
    assert lines[-1] == {"cmd": "pcm_flush"} and spk.ahead() == 0.0


@run
async def test_an_interrupt_on_a_board_call_flushes_the_board() -> None:
    brain, voice = Brain(), Voice()
    sent: list[dict[str, Any]] = []

    async def send(obj: dict[str, Any]) -> None:
        sent.append(obj)

    Mic.made.clear()
    desk = desk_call.DeskCalls(brain, voice, send, mic_factory=Mic,
                               speaker_factory=lambda: desk_call.BoardSpeaker(send))
    await desk.press(True)
    for _ in range(10):
        Mic.made[-1].on_block(b"\x02\x00" * 1200)
    await asyncio.sleep(0.05)
    await desk.press(False)
    await until(lambda: any(m.get("cmd") == "pcm" for m in sent))
    await desk.press(True)
    await until(lambda: {"cmd": "pcm_flush"} in sent)
    await desk.press(False)
    await desk.stop()


# -- the microphone: a device table gone stale is refreshed once (live 2026-09-28 15:24, a JBL joined) --

class FakeStream:
    def __init__(self) -> None:
        self.started = False

    def start(self) -> None:
        self.started = True

    def stop(self) -> None:
        pass

    def close(self) -> None:
        pass


def fake_sounddevice(fail_times: int) -> Any:
    import types

    sd = types.SimpleNamespace(calls=[], streams=[], fails=fail_times)
    sd.default = types.SimpleNamespace(device=[7, 2])

    def raw_input_stream(**kw: Any) -> FakeStream:
        sd.calls.append("open")
        if sd.fails > 0:
            sd.fails -= 1
            raise RuntimeError("Error opening RawInputStream: Internal PortAudio error [PaErrorCode -9986]")
        s = FakeStream()
        sd.streams.append(s)
        return s

    sd.RawInputStream = raw_input_stream
    sd._terminate = lambda: sd.calls.append("terminate")
    sd._initialize = lambda: sd.calls.append("initialize")
    sd.query_devices = lambda device=None, kind=None: {"name": "Yeti Stereo Microphone"}
    return sd


def test_a_stale_audio_device_table_is_refreshed_and_the_mic_opens(monkeypatch, caplog) -> None:
    import sys

    sd = fake_sounddevice(fail_times=1)
    monkeypatch.setitem(sys.modules, "sounddevice", sd)
    mic = desk_call.Mic(None, lambda b: None)
    with caplog.at_level("INFO"):
        assert mic.open() is True
    assert sd.calls == ["open", "terminate", "initialize", "open"]
    assert sd.streams[-1].started
    assert "refreshing the audio device list" in caplog.text and "microphone Yeti" in caplog.text
    mic.close()


def test_a_mic_that_fails_after_the_refresh_is_a_clean_false(monkeypatch, caplog) -> None:
    import sys

    sd = fake_sounddevice(fail_times=2)
    monkeypatch.setitem(sys.modules, "sounddevice", sd)
    mic = desk_call.Mic(None, lambda b: None)
    assert mic.open() is False
    assert sd.calls == ["open", "terminate", "initialize", "open"]
    assert "could not open the microphone" in caplog.text
