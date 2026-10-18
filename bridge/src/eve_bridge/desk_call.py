"""Hold to talk on the board: the Telegram Mini App's push-to-talk call, on the desk.

The owner asked on 2026-09-26 for the Voice PE's button to work "like telegram": hold it, talk, let go, and
eve answers. That is phone_call.Call — each press is one message to the same brain the owner texts, every
reply read back — with the phone swapped for the desk:

* **The button** (``{"cmd":"ptt","on":bool}`` from the board) is the phone's ``talk`` / ``done``.
* **The Mac microphone** (the same device the wake word uses, ``EVE_MIC``) is the phone's audio. It is
  opened only while the button is held.
* **The speaker** plays eve's reply, where the phone played it: the Voice PE's own speaker when it is the
  connected controller (``BoardSpeaker``, streamed as ``{"cmd":"pcm"}`` lines; owner, 2026-09-26: "route the
  sound of this through voice pe"), else the Mac's (``Speaker``).
* **The board's ring** shows the call's state through ``{"cmd":"agent","state":..}``: listening, thinking,
  speaking.
* **The head follower** (follow.py) hears the call as a conversation through ``on_phase``: ``listening``
  from the call's start (between presses too, since the person is still there), ``thinking`` and ``speaking``
  from the ring, ``idle`` when the call ends. Without it eve never looked at whoever was talking on a desk
  call: the ring's states went straight to the board and never reached the follower.

``DeskLink`` is the stand-in for phone_call.WebSocket: the only change to the call itself is where its bytes
come from and go to. A call starts on the first press and ends after ``phone_call.QUIET_SECS`` without one,
like a phone that went quiet; the next press starts a new one. One call at a time across phone and desk: the
chat has a single listener.
"""
from __future__ import annotations

import asyncio
import base64
import contextlib
import inspect
import json
import logging
import queue
import threading
import time
from typing import Any, Awaitable, Callable, Optional

from .phone_call import BYTES_PER_SEC, OP_BIN, OP_TEXT, SAMPLE_RATE, Brain, Call, Closed, Voice

log = logging.getLogger(__name__)

BLOCK_SECS = 0.05          # mic block handed to the call: 50 ms, 2400 samples


class Mic:
    """The Mac microphone while the button is held: 24 kHz mono int16 blocks to ``on_block`` (any thread)."""

    def __init__(self, device: Optional[str], on_block: Callable[[bytes], None]) -> None:
        self.device, self.on_block = device, on_block
        self._stream: Any = None

    def open(self) -> bool:
        if self._stream is not None:
            return True
        try:
            import sounddevice as sd

            from .ears import _find_input_device, input_device_name, open_input

            dev = _find_input_device(sd, self.device) if self.device else None

            def callback(indata: Any, _frames: int, _time: Any, _status: Any) -> None:
                self.on_block(bytes(indata))

            # open_input refreshes PortAudio's device table and retries once when the open fails: a device
            # that joined after the daemon started (a Bluetooth speaker's mic made the default) is otherwise
            # a stale entry and paInternalError (live 2026-09-28 15:24).
            self._stream = open_input(sd, lambda: sd.RawInputStream(
                samplerate=SAMPLE_RATE, channels=1, dtype="int16", blocksize=int(SAMPLE_RATE * BLOCK_SECS),
                device=dev, callback=callback), "desk call")
            log.info("desk call: microphone %s", input_device_name(sd, dev))
            return True
        except Exception as e:  # noqa: BLE001 — no mic: the press is empty and the call says so
            log.warning("desk call: could not open the microphone (%s)", e)
            self._stream = None
            return False

    def close(self) -> None:
        stream, self._stream = self._stream, None
        if stream is not None:
            with contextlib.suppress(Exception):
                stream.stop()
                stream.close()


class Speaker:
    """eve's reply on the Mac speaker. Writes block on a player thread; ``flush`` drops what is queued."""

    def __init__(self) -> None:
        self._q: queue.Queue[Optional[bytes]] = queue.Queue()
        self._gen = 0
        self._thread: Optional[threading.Thread] = None

    def play(self, pcm: bytes) -> None:
        if self._thread is None or not self._thread.is_alive():
            self._thread = threading.Thread(target=self._run, name="desk-call-speaker", daemon=True)
            self._thread.start()
        self._q.put((self._gen, pcm))  # type: ignore[arg-type]

    def flush(self) -> None:
        self._gen += 1
        with contextlib.suppress(queue.Empty):
            while True:
                self._q.get_nowait()

    def close(self) -> None:
        self.flush()
        self._q.put(None)

    def _run(self) -> None:
        try:
            import sounddevice as sd

            out = sd.RawOutputStream(samplerate=SAMPLE_RATE, channels=1, dtype="int16")
            out.start()
        except Exception as e:  # noqa: BLE001
            log.warning("desk call: could not open the speaker (%s)", e)
            return
        try:
            while (item := self._q.get()) is not None:
                gen, pcm = item  # type: ignore[misc]
                if gen == self._gen:
                    out.write(pcm)
        finally:
            with contextlib.suppress(Exception):
                out.stop()
                out.close()


Send = Callable[[dict[str, Any]], Awaitable[Any]]

PCM_PIECE_BYTES = 4800     # 100 ms of 24 kHz mono int16 per {"cmd":"pcm"} line (6.4 KB of base64)
LEAD_SECS = 3.0            # how far ahead of the board's playback the daemon may be; its buffer holds 12 s


class BoardSpeaker:
    """eve's reply on the Voice PE's speaker (firmware chirp.cpp, the voice buffer). The audio goes as base64
    lines of at most ``PCM_PIECE_BYTES``, paced to stay at most ``LEAD_SECS`` ahead of real time: speech
    arrives faster than it plays, and the board's buffer is finite. A flush drops what the board still holds."""

    def __init__(self, send: Send, clock: Callable[[], float] = time.monotonic,
                 sleep: Callable[[float], Awaitable[Any]] = asyncio.sleep) -> None:
        self.send, self.clock, self.sleep = send, clock, sleep
        self.started: Optional[float] = None    # when the current burst began playing, roughly
        self.queued = 0.0                       # seconds of audio sent in the current burst
        self._odd = b""                         # a reply's last byte, half a sample, waiting for the next reply

    def ahead(self) -> float:
        """Seconds of audio the board has not played yet, by the clock."""
        if self.started is None:
            return 0.0
        return max(0.0, self.queued - (self.clock() - self.started))

    async def play(self, pcm: bytes) -> None:
        # Only whole int16 samples go out. An odd last byte is kept for the next call: sent, it split every later
        # sample on the board across two; dropped, the next reply would start half a sample in.
        pcm = self._odd + pcm
        end = len(pcm) - len(pcm) % 2
        self._odd = pcm[end:]
        for i in range(0, end, PCM_PIECE_BYTES):
            piece = pcm[i:min(i + PCM_PIECE_BYTES, end)]
            if self.ahead() == 0.0:              # the board ran dry: a new burst starts now
                self.started, self.queued = self.clock(), 0.0
            wait = self.ahead() + len(piece) / BYTES_PER_SEC - LEAD_SECS   # room for the whole piece
            if wait > 0:
                await self.sleep(wait)
            await self.send({"cmd": "pcm", "d": base64.b64encode(piece).decode("ascii")})
            self.queued += len(piece) / BYTES_PER_SEC

    async def flush(self) -> None:
        self.started, self.queued, self._odd = None, 0.0, b""
        await self.send({"cmd": "pcm_flush"})

    def close(self) -> None:
        """What is on the board plays out; nothing is left here to drop."""

Phase = Callable[[str], Awaitable[None]]

# The call's states on the ring (firmware agent states). The call's "listening" means waiting for the next
# press, so the ring goes quiet; it turns blue only while the button is held (DeskCalls._down).
RING = {"thinking": "thinking", "speaking": "speaking"}
PING_SECS = 10.0           # the phone pings every 10 s; the desk pings while eve is still working on a reply


class DeskLink:
    """What phone_call.Call needs from its WebSocket, served by the board, the Mac mic and the Mac speaker."""

    def __init__(self, send_board: Send, speaker: Any, on_phase: Optional[Phase] = None) -> None:
        self.send_board, self.speaker, self.on_phase = send_board, speaker, on_phase
        self.closed = False
        self.inbox: asyncio.Queue[tuple[int, bytes]] = asyncio.Queue()
        self.ring = "idle"
        self.phase = "idle"

    # ---- the call's side ----
    async def recv(self) -> tuple[int, bytes]:
        if self.closed:
            raise Closed()
        op, data = await self.inbox.get()
        if op < 0:
            raise Closed()
        return op, data

    async def send_bytes(self, data: bytes) -> None:
        if self.closed:
            raise Closed()
        played = self.speaker.play(data)
        if inspect.isawaitable(played):         # BoardSpeaker; the Mac Speaker queues and returns
            await played

    async def send_json(self, obj: dict[str, Any]) -> None:
        if self.closed:
            raise Closed()
        kind = obj.get("type")
        if kind == "flush":
            flushed = self.speaker.flush()
            if inspect.isawaitable(flushed):
                await flushed
        elif kind == "state":
            await self.set_ring(RING.get(str(obj.get("state")), "idle"))
            if obj.get("note"):
                log.info("desk call: %s", obj["note"])
        elif kind == "heard":
            log.info("desk call: heard %d chars", len(str(obj.get("text") or "")))

    async def close(self, code: int = 1000) -> None:
        if not self.closed:
            self.closed = True
            self.inbox.put_nowait((-1, b""))

    # ---- the desk's side ----
    def talk(self) -> None:
        self.inbox.put_nowait((OP_TEXT, json.dumps({"type": "talk"}).encode()))

    def done(self) -> None:
        self.inbox.put_nowait((OP_TEXT, json.dumps({"type": "done"}).encode()))

    def ping(self) -> None:
        self.inbox.put_nowait((OP_TEXT, json.dumps({"type": "ping"}).encode()))

    def audio(self, pcm: bytes) -> None:
        self.inbox.put_nowait((OP_BIN, pcm))

    async def set_ring(self, state: str) -> None:
        if state != self.ring:
            self.ring = state
            with contextlib.suppress(Exception):
                await self.send_board({"cmd": "agent", "state": state})
        # The ring goes quiet between presses, but the call is still a conversation with someone at the desk.
        await self.set_phase(state if state in ("thinking", "speaking") else "listening")

    async def set_phase(self, phase: str) -> None:
        if phase != self.phase:
            self.phase = phase
            if self.on_phase is not None:
                try:
                    await self.on_phase(phase)
                except Exception:  # noqa: BLE001 — the head is an audience; the call goes on without it
                    log.exception("desk call: phase hook failed")


class DeskCalls:
    """Presses from the board in, one Call at a time. ``busy`` says whether the chat already has a listener
    (a phone call), so the desk never takes it from the phone."""

    def __init__(self, brain: Optional[Brain], voice: Optional[Voice], send_board: Send,
                 mic_device: Optional[str] = None, busy: Callable[[], bool] = lambda: False,
                 mic_factory: Callable[..., Any] = Mic, speaker_factory: Callable[[], Any] = Speaker,
                 on_phase: Optional[Phase] = None) -> None:
        self.brain, self.voice, self.send_board, self.on_phase = brain, voice, send_board, on_phase
        self.mic_device, self.busy = mic_device, busy
        self.mic_factory, self.speaker_factory = mic_factory, speaker_factory
        self.link: Optional[DeskLink] = None
        self.task: Optional[asyncio.Task[Any]] = None
        self.mic: Any = None
        self.held = False

    @property
    def active(self) -> bool:
        return self.task is not None and not self.task.done()

    async def press(self, on: bool) -> None:
        if on == self.held:
            return
        self.held = on
        if on:
            await self._down()
        else:
            self._up()

    async def _down(self) -> None:
        if self.brain is None or self.voice is None:
            log.warning("desk call: hold to talk needs the Telegram chat and an OpenAI key; not set up")
            return
        if not self.active:
            if self.busy():
                log.info("desk call: the phone is on a call; the button waits")
                return
            await self._start()
        assert self.link is not None
        loop = asyncio.get_running_loop()
        link = self.link

        def block(pcm: bytes) -> None:
            loop.call_soon_threadsafe(link.audio, pcm)

        link.talk()
        # The mic opens before the ring's await, in the same step as the press. Each line from the board is
        # its own task, so the release can run during that send: opened after it, the mic stayed open with the
        # button up until the call ended (verification/Eve/DeskCall.lean).
        self.mic = self.mic_factory(self.mic_device, block)
        self.mic.open()
        await link.set_ring("listening")

    def _up(self) -> None:
        if self.mic is not None:
            self.mic.close()
            self.mic = None
        if self.link is not None and self.active:
            self.link.done()

    async def _start(self) -> None:
        speaker = self.speaker_factory()
        link = self.link = DeskLink(self.send_board, speaker, self.on_phase)
        call = Call(link, self.brain, self.voice)  # type: ignore[arg-type]

        async def keepalive() -> None:
            # Call.run ends a call after QUIET_SECS without a message. A reply that takes longer (a tool, a
            # long read-out) must not be cut off, so ping while eve thinks or speaks; once it goes quiet the
            # call ends QUIET_SECS later, like a phone put down.
            while not link.closed:
                await asyncio.sleep(PING_SECS)
                if link.ring in ("thinking", "speaking") or self.held:
                    link.ping()

        async def run() -> None:
            log.info("desk call: started from the board's button")
            reason = "?"
            await link.set_phase("listening")
            pinger = asyncio.create_task(keepalive())
            try:
                reason = await call.run()
            finally:
                pinger.cancel()
                if self.mic is not None:
                    self.mic.close()
                    self.mic = None
                self.held = False
                speaker.close()
                await link.set_ring("idle")
                await link.set_phase("idle")
                await link.close()
                log.info("desk call: ended (%s), %d press(es)", reason, call.presses)

        self.task = asyncio.create_task(run(), name="desk-call")

    async def stop(self) -> None:
        if self.link is not None:
            await self.link.close()
        if self.task is not None:
            await asyncio.gather(self.task, return_exceptions=True)
