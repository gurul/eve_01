"""Calling eve from the phone: push to talk in the Telegram Mini App, with everything texting can do.

The owner asked on 2026-09-25 for voice calls with eve over Telegram, then for push to talk "instead of live",
and for "full functionality like i am texting it". A Telegram bot cannot take a Telegram call, so the call lives in
eve's Mini App (miniapp.py): **Call eve** opens the microphone in Telegram's in-app browser, and a WebSocket
carries it to the daemon through the same Cloudflare tunnel the Mini App already uses.

* **It is the chat, spoken.** Each press of the button is one message to the same brain the owner texts
  (telegram.py): the audio is transcribed and handed to the chat exactly as if it had been typed, so every tool
  the chat has works on a call — mail, calendar, Drive, watch, Meet, notes, tasks on the Mac, app building,
  memory, the relays. Everything eve then says in the chat is also spoken on the call, and the chat keeps the
  whole call in writing: "🎙 what you said", then eve's replies.
* **Push to talk.** The phone sends audio only while the button is held (``talk``, the audio, ``done``). A press
  while eve is speaking interrupts it: the phone drops what it was playing and eve stops reading.
* **The wire.** ``GET /api/call`` upgrades to a WebSocket (RFC 6455, written here: the Mini App server is a small
  hand-written HTTP server). Audio is 24 kHz mono 16-bit PCM both ways, in binary frames. Text frames carry JSON:
  the phone's first message is its Telegram ``initData``; then ``talk``, ``done`` and ``end``; eve sends
  ``state``, ``heard`` (what it transcribed), ``flush`` (drop the queued audio) and ``ended``.
* **Only the owner.** Nothing starts until the first message carries initData signed by this bot for an owner
  (miniapp.check_init_data, the check every Mini App call makes), within ``AUTH_SECS``. The upgrade is refused
  from any origin but the Mini App's own. One call at a time.
* **Notes on the go.** The owner asked on 2026-10-04: "when i say start taking notes the phone should hold the
  port until i click action button to turn it off, this allows me to take notes on the go". On an Eve Link call
  whose hello says it can (``"can": ["notes"]``), take_notes records the phone's microphone instead of the desk's
  (:class:`PhoneNotes`): the call stops being push to talk, every audio frame goes to the notes, and the next
  press of the Action Button (``notes_stop``) writes them up, texts them to the owner and says "Notes saved." A
  call that drops keeps what was heard, and the same phone calling back within ``NOTES_GRACE_SECS`` carries on.
"""
from __future__ import annotations

import asyncio
import base64
import contextlib
import contextvars
import hashlib
import inspect
import json
import logging
import os
import re
import struct
import time
from typing import Any, Awaitable, Callable, Iterator, Optional, Protocol, cast

from . import spend

log = logging.getLogger(__name__)

WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
SAMPLE_RATE = 24000                     # what the phone sends and OpenAI's "pcm" speech format is
BYTES_PER_SEC = SAMPLE_RATE * 2
AUTH_SECS = 10.0                        # the first message (initData) must come this soon
MAX_FRAME_BYTES = 256 * 1024            # a 100 ms block is 4800 bytes; anything this big is not audio
MAX_PRESS_SECS = 120.0                  # one press holds at most this much audio; the rest is dropped
MIN_PRESS_SECS = 0.35                   # shorter than this is a tap, not words
MAX_CALL_SECS = 60 * 60                 # a call this long is ended
QUIET_SECS = 30.0                       # nothing from the phone this long (it sends a ping every 10 s): it is gone
MAX_SPOKEN_CHARS = 1500
# A call holds the chat (its listener) from its first press, not from when it opens, and an idle one gives it back to
# the desk's button. An open call is often an unused one (Eve Link keeps it up for 45 s after a turn): on
# 2026-09-30 one blocked the Voice PE ("desk call: the phone is on a call; the button waits") and would have read the
# owner's texted replies out on the phone. Idle: no press, transcription, reply or speech for this long.
IDLE_YIELD_SECS = 8.0                 # a reply longer than this is read in part; the chat has all of it
# The call's ears (owner, 2026-09-30: "transcription is terrible"; "That's a vague Da they talking
# to me" came back for a sentence with pauses). The full model, not the mini; EVE_CALL_STT_MODEL overrides it.
DEFAULT_STT_MODEL = "gpt-4o-transcribe"
# Push-to-talk already supplies the turn boundary. Keep each press together, including its pauses, instead of
# asking server VAD to create additional turns. Near-field reduction is for a microphone held to the mouth.
LIVE_NOISE_REDUCTION = "near_field"
# The live ears (LiveEars) are OpenAI's realtime transcription WebSocket, which OpenRouter does not carry: an opt-in
# that needs this key (EVE_CALL_LIVE_STT=1). Off by default since 2026-10-08 (docs/openrouter.md).
LIVE_EARS_KEY_ENV = "OPENAI_API_KEY"
# The language the owner speaks, ISO-639-1. Left to guess, the model heard "Spotify mode" as Chinese on a short
# Voice PE press and eve answered in Chinese (2026-09-27; owner: "it sometimes thinks i am speaking another
# language"). EVE_CALL_LANGUAGE sets it; an empty value lets the model guess again.
DEFAULT_LANGUAGE = "en"
# Even told "en", the transcriber can answer near-silence with a syllable in another script ("あ。" on a 7 s call,
# 2026-10-08; owner: "english only remember"), and eve then answered in that language. With English set, words
# whose letters are mostly not Latin are treated as not caught (``foreign``).
LATIN_LANGUAGES = frozenset({"en"})


def foreign(text: str, language: str) -> bool:
    """Whether ``text`` cannot be what the owner said in ``language``: under half its letters are Latin. Only for
    a Latin-script language (LATIN_LANGUAGES); "" (the model guesses) or any other language keeps every text."""
    if language not in LATIN_LANGUAGES:
        return False
    letters = [c for c in text if c.isalpha()]
    if not letters:
        return False
    latin = sum(1 for c in letters if c.isascii() or "\u00c0" <= c <= "\u024f")
    return latin * 2 < len(letters)


# The call's voice, on OpenRouter (owner, 2026-09-25: "switch to these ... for telegram call"). Measured from this
# Mac the same day, median of three, one two-sentence reply: deepgram/aura-2 first sound 0.31 s, its free flux-tts
# 0.42 s, OpenAI gpt-4o-mini-tts streamed 1.15 s, google gemini-3.8-flash-lite-tts 1.92 s, hexgrad/kokoro-82m 7-10 s.
# Until 2026-10-08 OpenAI's gpt-4o-mini-tts was the fallback and OpenAI's live transcription session the ears (text
# 0.39-0.50 s after release; no uploaded OpenRouter model beat it: deepgram/nova-3 0.69 s, whisper-large-v3-turbo
# 1.32 s). Since the owner moved every model call to his OpenRouter key (docs/openrouter.md), the voice is OpenRouter's
# alone and the ears upload each press to OpenRouter's /audio/transcriptions (``DEFAULT_STT_MODEL``, sent as
# "openai/gpt-4o-transcribe"). The live ears (``LiveEars``) are an opt-in kept for later: OpenRouter has no realtime
# transcription, so they run only with EVE_CALL_LIVE_STT=1 AND an OPENAI_API_KEY.
OPENROUTER_SPEECH_URL = "https://openrouter.ai/api/v1/audio/speech"
DEFAULT_OR_TTS_MODEL = "deepgram/aura-2"
DEFAULT_OR_TTS_VOICE = "aura-2-thalia-en"
OR_TTS_USD_PER_CHAR = 0.00003           # deepgram/aura-2's listed price on OpenRouter (/api/v1/models, 2026-09-25)

# Notes on the phone (PhoneNotes). A call that drops while they are on is waited for this long before the notes are
# written up with what was heard: long enough for the phone to change networks or come back from a lift, short
# enough that the owner who walked away still gets their notes the same minute.
NOTES_GRACE_SECS = 60.0
NOTES_CAN = "notes"                     # the capability an Eve Link hello lists to be able to take notes
# Why a call ended. "you hung up" is the End button (``end``): with notes on it means stop and write them up. A
# connection that broke is not the owner hanging up, and gives the phone the grace to call back.
HUNG_UP = "you hung up"
SUMMARY_WAIT_SECS = 240.0               # a stop waits this long for the write-up (notes.py's own waits, and the text)
DROPPED = "the connection dropped"
QUIET = "the phone went quiet"
# The daemon stopping while the phone takes notes: the session is written up with no summary (the summary model
# can take two minutes, and a restart will not wait that long), within this many seconds, so the words are kept.
NOTES_SHUTDOWN_SECS = 30.0

OP_CONT, OP_TEXT, OP_BIN, OP_CLOSE, OP_PING, OP_PONG = 0x0, 0x1, 0x2, 0x8, 0x9, 0xA

class Closed(Exception):
    """The other side closed the WebSocket, or it broke."""


def accept_key(key: str) -> str:
    return base64.b64encode(hashlib.sha1((key + WS_GUID).encode("ascii")).digest()).decode("ascii")


def is_upgrade(headers: dict[str, str]) -> bool:
    return ("websocket" in headers.get("upgrade", "").lower()
            and "upgrade" in headers.get("connection", "").lower()
            and bool(headers.get("sec-websocket-key")))


def frame(opcode: int, payload: bytes) -> bytes:
    """One unmasked, final frame (a server never masks)."""
    n = len(payload)
    if n < 126:
        head = struct.pack("!BB", 0x80 | opcode, n)
    elif n < 1 << 16:
        head = struct.pack("!BBH", 0x80 | opcode, 126, n)
    else:
        head = struct.pack("!BBQ", 0x80 | opcode, 127, n)
    return head + payload


class WebSocket:
    """The server side of one WebSocket, over an asyncio stream the HTTP server already read the request from."""

    def __init__(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        self.reader, self.writer = reader, writer
        self._send_lock = asyncio.Lock()
        self.closed = False

    @classmethod
    async def accept(cls, reader: asyncio.StreamReader, writer: asyncio.StreamWriter,
                     headers: dict[str, str]) -> "WebSocket":
        writer.write(("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                      f"Sec-WebSocket-Accept: {accept_key(headers['sec-websocket-key'])}\r\n\r\n").encode("ascii"))
        await writer.drain()
        return cls(reader, writer)

    async def _read_frame(self) -> tuple[bool, int, bytes]:
        try:
            b1, b2 = await self.reader.readexactly(2)
            n = b2 & 0x7F
            if n == 126:
                (n,) = struct.unpack("!H", await self.reader.readexactly(2))
            elif n == 127:
                (n,) = struct.unpack("!Q", await self.reader.readexactly(8))
            if n > MAX_FRAME_BYTES:
                raise Closed("frame too large")
            if not b2 & 0x80:
                raise Closed("a client frame must be masked")
            mask = await self.reader.readexactly(4)
            data = await self.reader.readexactly(n)
        except (asyncio.IncompleteReadError, ConnectionError) as e:
            raise Closed(type(e).__name__) from None
        # unmask the whole payload at once: XOR with the 4-byte mask repeated to its length
        if not n:
            return bool(b1 & 0x80), b1 & 0x0F, b""
        key = (mask * (n // 4 + 1))[:n]
        payload = (int.from_bytes(data, "big") ^ int.from_bytes(key, "big")).to_bytes(n, "big")
        return bool(b1 & 0x80), b1 & 0x0F, payload

    async def recv(self) -> tuple[int, bytes]:
        """The next text or binary message (fragments joined). Pings are answered; a close raises Closed."""
        parts: list[bytes] = []
        first = 0
        while True:
            fin, op, payload = await self._read_frame()
            if op == OP_PING:
                await self._send(OP_PONG, payload)
                continue
            if op == OP_PONG:
                continue
            if op == OP_CLOSE:
                with contextlib.suppress(Exception):
                    await self._send(OP_CLOSE, payload[:2])
                self.closed = True
                raise Closed("closed by the phone")
            if op in (OP_TEXT, OP_BIN):
                first, parts = op, [payload]
            elif op == OP_CONT and parts:
                parts.append(payload)
            else:
                raise Closed("bad frame")
            if sum(len(p) for p in parts) > MAX_FRAME_BYTES:
                raise Closed("message too large")
            if fin:
                return first, b"".join(parts)

    async def _send(self, opcode: int, payload: bytes) -> None:
        if self.closed and opcode != OP_CLOSE:
            raise Closed("already closed")
        async with self._send_lock:
            try:
                self.writer.write(frame(opcode, payload))
                await self.writer.drain()
            except (ConnectionError, RuntimeError) as e:
                self.closed = True
                raise Closed(type(e).__name__) from None

    async def send_bytes(self, data: bytes) -> None:
        await self._send(OP_BIN, data)

    async def send_json(self, obj: dict[str, Any]) -> None:
        await self._send(OP_TEXT, json.dumps(obj).encode("utf-8"))

    async def close(self, code: int = 1000) -> None:
        if self.closed:
            return
        with contextlib.suppress(Exception):
            await self._send(OP_CLOSE, struct.pack("!H", code))
        self.closed = True



# ---- the speech on both ends ---------------------------------------------------------------------

_MARKS = re.compile(r"[*_`#>|~]+|\[([^\]]*)\]\([^)]*\)")


def speakable(text: str) -> str:
    """A chat message as words to read out: no Markdown marks, a link read as its words, a bare URL as "a link",
    and a long message cut at a sentence with a pointer to the chat."""
    t = _MARKS.sub(lambda m: m.group(1) or " ", text or "")
    t = re.sub(r"https?://\S+", "a link", t)
    t = re.sub(r"^\s*[-•]\s+", "", t, flags=re.M)
    t = " ".join(t.split())
    if len(t) > MAX_SPOKEN_CHARS:
        cut = t.rfind(". ", 0, MAX_SPOKEN_CHARS)
        t = t[: cut + 1 if cut > 200 else MAX_SPOKEN_CHARS] + " The rest is in the chat."
    return t


def sentences(text: str, limit: int = 220) -> list[str]:
    """Read in sentence-sized pieces, so the first one plays while the next is still being made."""
    parts = [p.strip() for p in re.split(r"(?<=[.!?])\s+", text) if p.strip()]
    out: list[str] = []
    for p in parts:
        if out and len(out[-1]) + len(p) < 90:
            out[-1] += " " + p                      # short sentences ride together: fewer, fuller requests
        else:
            out.append(p[:limit * 4])
    return out


class Voice(Protocol):
    def transcribe(self, pcm: bytes) -> str: ...
    def speak_stream(self, text: str) -> Iterator[bytes]: ...
    def live_ears(self) -> Optional["LiveEars"]: ...


class LiveEars:
    """Stream the held button's audio, with one manually committed transcript at release.

    One OpenAI realtime transcription session per call, opened when the call starts and reused for every
    press. Audio is appended as it arrives, and the server receives the entire press as one manually committed
    turn. The next press cannot clear or reuse that turn's
    bookkeeping until its transcript is complete. This preserves sentence context and prevents delayed events
    from a previous press appearing at the start of the next one. Call's PCM upload remains the fallback.
    Any failure is the caller's cue to fall back to uploading the press (``Voice.transcribe``)."""

    def __init__(self, api_key: str, model: str, language: str = DEFAULT_LANGUAGE) -> None:
        self.api_key, self.model, self.language = api_key, model, language
        self.conn: Any = None
        self._manager: Any = None
        self._reader: Optional[asyncio.Task[Any]] = None
        self.order: list[str] = []
        self.done: dict[str, str] = {}
        self.changed = asyncio.Event()
        self.configured = asyncio.Event()
        self._press_lock = asyncio.Lock()
        self.commit_resolved = True
        self._awaiting_commit = False
        self.broken = False
        self.fed = 0

    async def open(self) -> None:
        from openai import AsyncOpenAI

        self._manager = AsyncOpenAI(api_key=self.api_key).realtime.connect(extra_query={"intent": "transcription"})
        self.conn = await self._manager.__aenter__()
        await self.conn.session.update(session={"type": "transcription", "audio": {"input": {
            "format": {"type": "audio/pcm", "rate": SAMPLE_RATE},
            "transcription": {"model": self.model, **({"language": self.language} if self.language else {})},
            "noise_reduction": {"type": LIVE_NOISE_REDUCTION},
            "turn_detection": None}}})
        self._reader = asyncio.ensure_future(self._read())
        # Sending session.update does not prove the server accepted it. Audio must not reach the default VAD
        # session while the update is still pending, or it can produce an unsolicited startup turn.
        async with asyncio.timeout(5.0):
            await self.configured.wait()
        if self.broken:
            raise ConnectionError("the live transcription session was not configured")

    async def _read(self) -> None:
        try:
            async for ev in self.conn:
                kind = getattr(ev, "type", "")
                if kind == "session.updated":
                    self.configured.set()
                elif kind == "input_audio_buffer.committed":
                    if not self._awaiting_commit:
                        self.broken = True    # an unexpected turn must never become the owner's words
                        self.changed.set()
                        continue
                    self.order.append(ev.item_id)
                    self.commit_resolved = True
                    self._awaiting_commit = False
                elif kind == "conversation.item.input_audio_transcription.completed":
                    if ev.item_id in self.order:
                        self.done[ev.item_id] = (ev.transcript or "").strip()
                elif kind == "conversation.item.input_audio_transcription.failed":
                    if getattr(ev, "item_id", "") in self.order:
                        self.broken = True    # recover with the complete PCM instead of losing this press
                elif kind == "error":
                    code = str(getattr(getattr(ev, "error", None), "code", "") or "")
                    log.warning("call: live transcription error %s", code or "?")
                    self.broken = True
                    self.configured.set()
                self.changed.set()
        except Exception as e:  # noqa: BLE001
            log.info("call: the live transcription session closed (%s)", type(e).__name__)
        self.broken = True
        self.configured.set()
        self.changed.set()

    async def begin(self) -> None:
        """A new press: nothing left over from the last one."""
        await self._press_lock.acquire()
        try:
            if self.broken:
                raise ConnectionError("the live transcription session closed")
            self.order, self.done, self.fed = [], {}, 0
            self.commit_resolved = True
            self._awaiting_commit = False
            await self.conn.input_audio_buffer.clear()
        except BaseException:
            self._press_lock.release()
            raise

    async def discard(self) -> None:
        """A short tap has no transcript; clear its audio and let the next press begin."""
        try:
            await self.conn.input_audio_buffer.clear()
        except BaseException:
            self.broken = True
            raise
        finally:
            if self._press_lock.locked():
                self._press_lock.release()

    async def feed(self, pcm: bytes) -> None:
        self.fed += len(pcm)
        await self.conn.input_audio_buffer.append(audio=base64.b64encode(pcm).decode("ascii"))

    async def finish(self, timeout: float) -> str:
        """One release commits one complete turn, then waits for that item's final transcript."""
        try:
            if self.broken:
                raise ConnectionError("the live transcription session closed")
            self.commit_resolved = False
            self._awaiting_commit = True
            async with asyncio.timeout(timeout):
                await self.conn.input_audio_buffer.commit()
                while True:
                    if self.broken:
                        raise ConnectionError("the live transcription session closed")
                    if self.commit_resolved and self.order and all(i in self.done for i in self.order):
                        break
                    self.changed.clear()
                    await self.changed.wait()
            spend.record("openai", self.model, spend.CALLS, None,
                         note=f"live transcription, {self.fed / BYTES_PER_SEC:.1f} s")
            return " ".join(self.done[i] for i in self.order if self.done.get(i)).strip()
        except BaseException:
            # Timed-out or cancelled requests may still deliver events. Never reuse their session state.
            self.broken = True
            raise
        finally:
            if self._press_lock.locked():
                self._press_lock.release()

    async def close(self) -> None:
        if self._reader is not None:
            self._reader.cancel()
            await asyncio.gather(self._reader, return_exceptions=True)
        if self._manager is not None:
            with contextlib.suppress(Exception):
                await self._manager.__aexit__(None, None, None)


class OpenAIVoice:
    """Speech to text and text to speech on OpenRouter, with the owner's OPENROUTER_API_KEY (blocking: run on a
    thread). The name is kept from when both were OpenAI's; the transcriber is still an OpenAI model."""

    def __init__(self, api_key: str, stt_model: str = DEFAULT_STT_MODEL) -> None:
        from . import openrouter

        self._client = openrouter.client(api_key)
        self.stt_model = stt_model
        self.language = DEFAULT_LANGUAGE                # EVE_CALL_LANGUAGE; "" = the model guesses
        self.live = False                               # LiveEars: EVE_CALL_LIVE_STT=1 and an OpenAI key
        self.live_key = ""                              # the OpenAI key the live ears need (opt-in only)
        self.or_key, self.or_model, self.or_voice = api_key, DEFAULT_OR_TTS_MODEL, DEFAULT_OR_TTS_VOICE

    def transcribe(self, pcm: bytes) -> str:
        from .notes import to_wav

        extra = {"language": self.language} if self.language else {}
        resp = self._client.audio.transcriptions.create(file=("call.wav", to_wav(pcm, SAMPLE_RATE), "audio/wav"),
                                                        model=self.stt_model, **extra)
        spend.record_transcription(spend.CALLS, self.stt_model, resp, seconds=len(pcm) / BYTES_PER_SEC)
        return (getattr(resp, "text", "") or "").strip()

    def speak_stream(self, text: str) -> Iterator[bytes]:
        """The spoken words as they are made (blocking iterator): the first bytes play while the rest arrive.
        OpenRouter's voice (deepgram/aura-2). A failure raises: that sentence is not heard (the chat has it in
        writing). There is no second voice since OpenAI's was removed with the OpenAI key (2026-10-08)."""
        import httpx

        with httpx.stream("POST", OPENROUTER_SPEECH_URL, timeout=httpx.Timeout(20.0, connect=5.0),
                          headers={"Authorization": f"Bearer {self.or_key}"},
                          json={"model": self.or_model, "voice": self.or_voice, "input": text,
                                "response_format": "pcm"}) as resp:
            if resp.status_code != 200:
                raise RuntimeError(f"OpenRouter speech {resp.status_code}")
            yield from resp.iter_bytes(4800)
        spend.record("openrouter", self.or_model, spend.CALLS, len(text) * OR_TTS_USD_PER_CHAR,
                     note=f"speech, {len(text)} chars")

    def live_ears(self) -> Optional[LiveEars]:
        return LiveEars(self.live_key, self.stt_model, self.language) if self.live and self.live_key else None


def _on(raw: Any) -> bool:
    return str(raw or "").strip().lower() in ("1", "true", "yes", "on")


def make_voice(environ: Any = None) -> Optional[Voice]:
    """The call's ears and voice on OPENROUTER_API_KEY, or None (calls off) without it. EVE_CALL_STT_MODEL,
    EVE_CALL_LANGUAGE, EVE_CALL_OR_TTS_MODEL and EVE_CALL_OR_VOICE tune it. The live ears are opt-in:
    EVE_CALL_LIVE_STT=1 with an OPENAI_API_KEY (OpenAI's realtime transcription; OpenRouter has none)."""
    from . import openrouter

    env = os.environ if environ is None else environ
    key = openrouter.key(env)
    if not key:
        log.warning("call: OPENROUTER_API_KEY is not set — calls are off")
        return None
    try:
        voice = OpenAIVoice(key, (env.get("EVE_CALL_STT_MODEL") or DEFAULT_STT_MODEL).strip())
    except ImportError as e:
        log.warning("call: the openai SDK is not importable (%s); calls are off", e)
        return None
    voice.language = (env.get("EVE_CALL_LANGUAGE", DEFAULT_LANGUAGE) or "").strip().lower()
    voice.or_model = (env.get("EVE_CALL_OR_TTS_MODEL") or DEFAULT_OR_TTS_MODEL).strip()
    voice.or_voice = (env.get("EVE_CALL_OR_VOICE") or DEFAULT_OR_TTS_VOICE).strip()
    if _on(env.get("EVE_CALL_LIVE_STT")):
        voice.live_key = (env.get(LIVE_EARS_KEY_ENV) or "").strip()
        voice.live = bool(voice.live_key)
        if not voice.live:
            log.warning("call: EVE_CALL_LIVE_STT=1 but OPENAI_API_KEY is not set; each press is uploaded instead")
    return voice


# ---- the call ------------------------------------------------------------------------------------

class Brain(Protocol):
    """The chat, lent by the daemon (telegram.TelegramInlet): ``hear`` hands it the owner's words as a message;
    ``listen(say)`` has everything it then says to the owner also passed to ``say`` until ``listen(None)``.
    ``listen(None, owner=say)`` clears it only while ``say`` is still the listener."""

    def hear(self, text: str) -> None: ...
    def listen(self, say: Optional[Callable[[str], None]],
               owner: Optional[Callable[[str], None]] = None) -> bool: ...


class CaptureBrain(Brain, Protocol):
    """Optional approval-aware chat interface. A recording carries the question it started under."""

    def answer_context(self) -> object | None: ...
    def hear(self, text: str, *, answer_context: object = ...) -> None: ...


# A brain may also take ``hear(text, call=say)``: the reader of the call that heard the words (``_takes_call``).


_UNCAPTURED_ANSWER = object()


def _takes_call(brain: object) -> bool:
    """Whether ``brain.hear`` takes ``call=``, the reader of the call that heard the words (telegram.TelegramInlet)."""
    try:
        params = inspect.signature(brain.hear).parameters  # type: ignore[attr-defined]
    except (AttributeError, TypeError, ValueError):
        return False
    return "call" in params or any(p.kind is inspect.Parameter.VAR_KEYWORD for p in params.values())


CheckOwner = Callable[[str], Optional[int]]


class Call:
    """One call: presses in, speech out. Each press is timed from release to eve's first sound, per stage, in
    the log (the owner asked for less latency: measure it, do not guess)."""

    def __init__(self, ws: WebSocket, brain: Brain, voice: Voice, clock: Callable[[], float] = time.monotonic) -> None:
        self.ws, self.brain, self.voice, self.clock = ws, brain, voice, clock
        self.talking = False
        self.press = bytearray()
        self._answer_context: object = _UNCAPTURED_ANSWER
        self.lines: asyncio.Queue[str] = asyncio.Queue()
        self.reading: Optional[asyncio.Task[Any]] = None
        self.tasks: set[asyncio.Task[Any]] = set()
        self.presses = 0
        self.ears: Optional[LiveEars] = None
        self.ears_live = False                  # this press is going to the live session
        self.opening: Optional[asyncio.Task[Any]] = None
        self.released_at: Optional[float] = None   # the press being answered: when it was let go
        self.timing: dict[str, float] = {}
        self.listening = False                  # this call is the chat's listener (from its first press)
        self.speaking = False
        self.last_activity = clock()
        # Eve Link's call (``token`` hello), and whether its hello said it can take notes: only such a call
        # records notes on the phone. Set by PhoneCalls._serve.
        self.from_app = False
        self.can_notes = False
        self.notes: Optional[PhoneNotes] = None   # notes on this phone are on: every audio frame is theirs

    def _touch(self) -> None:
        self.last_activity = self.clock()

    def _take_chat(self) -> bool:
        if not self.listening:
            self.listening = bool(self.brain.listen(self.say))
        return self.listening

    @property
    def idle(self) -> bool:
        return (not self.talking and not self.speaking and not self.tasks and self.lines.empty()
                and self.clock() - self.last_activity >= IDLE_YIELD_SECS)

    def release_if_idle(self) -> bool:
        """The desk's button wants the chat: an idle call lets go of it (and stays open, for its next press)."""
        if self.listening and self.idle:
            self.brain.listen(None, owner=self.say)
            self.listening = False
            log.info("call: idle, so the desk's button takes the chat")
            return True
        return not self.listening

    def release_for_notes(self) -> None:
        """The turn that started the notes is over (its reply read out): now let the chat's listener go."""
        if self.notes is not None:
            self.release_chat()

    def drop_press(self) -> None:
        """A press the notes cut off (a press made while eve was still answering, then notes started): it is
        nobody's question now. The phone drops its turn too (Relay.notesOn), so both sides forget it, and the next
        press after the notes is a fresh one, not stuck behind ``talking``."""
        self.talking, self.press = False, bytearray()
        self._answer_context = _UNCAPTURED_ANSWER
        if self.ears_live and self.ears is not None:
            ears = self.ears
            discard = getattr(ears, "discard", None)
            if callable(discard):
                self._spawn(discard())
        self.ears_live = False

    def release_chat(self) -> None:
        """Give the chat's listener up, so nothing texted is read out on the call (notes recording the phone would
        write eve's own voice down). The call's next press takes it back."""
        if self.listening:
            self.brain.listen(None, owner=self.say)
            self.listening = False

    async def rest(self) -> None:
        """The state the phone waits in: taking notes, while they are on; listening for the next press otherwise."""
        if self.notes is not None:
            await self._state("notes", **self.notes.progress())
        else:
            await self._state("listening")

    def _spawn(self, coro: Awaitable[Any]) -> None:
        task = asyncio.ensure_future(coro)
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)

    async def _state(self, state: str, **extra: Any) -> None:
        if not self.ws.closed:
            with contextlib.suppress(Closed):
                await self.ws.send_json({"type": "state", "state": state, **extra})

    def say(self, text: str) -> None:
        """Something eve said in the chat (or a sentence of it, streamed): read it out after what is playing."""
        words = speakable(text)
        if words:
            if self.released_at is not None and "brain" not in self.timing:
                self.timing["brain"] = self.clock() - self.released_at
            self.lines.put_nowait(words)

    async def _speech(self, text: str) -> Any:
        """The words' audio as it is made: the blocking stream runs on a thread and hands chunks over."""
        loop = asyncio.get_running_loop()
        chunks: asyncio.Queue[Optional[bytes]] = asyncio.Queue()

        def pump() -> None:
            try:
                for chunk in self.voice.speak_stream(text):
                    loop.call_soon_threadsafe(chunks.put_nowait, chunk)
            except Exception as e:  # noqa: BLE001 — one sentence unheard, the call goes on
                log.warning("call: speech failed (%s); a sentence was not read out", e)
                raise
            finally:
                loop.call_soon_threadsafe(chunks.put_nowait, None)

        worker = loop.run_in_executor(None, pump)
        try:
            while (chunk := await chunks.get()) is not None:
                yield chunk
        finally:
            await asyncio.gather(worker, return_exceptions=True)

    async def read_out(self) -> None:
        """The reader: one message at a time, a sentence at a time, streamed to the phone as it is made."""
        carry = b""
        while True:
            text = await self.lines.get()
            self.speaking = True
            self._touch()
            await self._state("speaking")
            for piece in sentences(text):
                # The sentence's words go just before its sound, so the phone's voice mode captions what it plays
                # (owner, 2026-10-09: "the stackchan face with captions"); the phone shows it when it is heard.
                if not self.ws.closed:
                    with contextlib.suppress(Closed):
                        await self.ws.send_json({"type": "said", "text": piece})
                async for chunk in self._speech(piece):
                    data = carry + chunk
                    carry = data[len(data) - len(data) % 2:]
                    data = data[: len(data) - len(data) % 2]
                    if data and not self.ws.closed:
                        if self.released_at is not None:
                            self._first_sound()
                        await self.ws.send_bytes(data)
            if self.lines.empty():
                self.speaking = False
                self._touch()
                await self.rest()

    def _first_sound(self) -> None:
        t = self.timing
        t["first_sound"] = self.clock() - (self.released_at or self.clock())
        self.released_at = None
        log.info("call: press %.1f s → heard in %.2f s (%s) → first reply %.2f s → first sound %.2f s after release",
                 t.get("press", 0), t.get("stt", 0), "live" if t.get("live") else "upload",
                 t.get("brain", 0), t["first_sound"])

    async def interrupt(self) -> None:
        """A press while eve talks: stop reading, drop what is queued here and on the phone."""
        while not self.lines.empty():
            self.lines.get_nowait()
        if self.reading is not None and not self.reading.done():
            self.reading.cancel()
            await asyncio.gather(self.reading, return_exceptions=True)
        self.reading = asyncio.ensure_future(self.read_out())
        with contextlib.suppress(Closed):
            await self.ws.send_json({"type": "flush"})

    async def _open_ears(self) -> None:
        ears = self.voice.live_ears()
        if ears is None:
            return
        try:
            await ears.open()
            self.ears = ears
        except asyncio.CancelledError:
            await ears.close()
            raise
        except Exception as e:  # noqa: BLE001 — no live session: each press is uploaded after release
            log.warning("call: live transcription unavailable (%s); uploading presses instead", type(e).__name__)
            await ears.close()

    async def _begin_press(self) -> None:
        if self.talking:
            return                              # duplicate talk frames are not a new press
        capture = getattr(self.brain, "answer_context", None)
        # Capture before yielding or taking the listener: a question can change during either operation.
        self._answer_context = capture() if callable(capture) else _UNCAPTURED_ANSWER
        self.talking, self.press, self.ears_live = True, bytearray(), False
        self._touch()
        self._take_chat()                       # the first press, or the first after the desk took it
        await self.interrupt()
        ears = self.ears
        if ears is not None and not ears.broken:
            try:
                await ears.begin()
                self.ears_live = True
            except Exception as e:  # noqa: BLE001
                log.info("call: live transcription dropped (%s)", type(e).__name__)
                if self.ears is ears:
                    self.ears = None
                    await ears.close()

    async def _feed(self, data: bytes) -> None:
        if not self.talking or len(self.press) >= MAX_PRESS_SECS * BYTES_PER_SEC:
            return
        remaining = int(MAX_PRESS_SECS * BYTES_PER_SEC) - len(self.press)
        data = data[:min(len(data) - len(data) % 2, remaining)]
        self.press.extend(data)
        if self.ears_live and self.ears is not None:
            try:
                await self.ears.feed(data)
            except Exception:  # noqa: BLE001 — the press is still kept whole, for the upload
                self.ears.broken = True

    async def _transcribe(self, pcm: bytes, live: bool) -> tuple[str, bool]:
        ears = self.ears
        if live and ears is not None:
            try:
                return await ears.finish(timeout=6.0), True
            except Exception as e:  # noqa: BLE001
                log.info("call: live transcription failed (%s); uploading the press", type(e).__name__)
                if self.ears is ears:
                    self.ears = None
                    await ears.close()
        return await asyncio.to_thread(self.voice.transcribe, pcm), False

    async def heard(self, pcm: bytes, live: bool, released: float,
                    *, answer_context: object = _UNCAPTURED_ANSWER) -> None:
        """One press, done: its words, handed to the chat as the owner's."""
        if len(pcm) < MIN_PRESS_SECS * BYTES_PER_SEC:
            if live and self.ears is not None:
                with contextlib.suppress(Exception):
                    discard = getattr(self.ears, "discard", self.ears.begin)
                    await discard()                   # nothing of a tap is left in the live session
            await self._state("listening", note="Hold the button while you talk.")
            return
        await self._state("thinking")
        try:
            text, was_live = await self._transcribe(pcm, live)
        except Exception as e:  # noqa: BLE001 — the type only: the error can quote what was said
            log.warning("call: transcription failed: %s", type(e).__name__)
            await self._state("listening", note="I couldn't make that out. Try again?")
            return
        if text and foreign(text, getattr(self.voice, "language", DEFAULT_LANGUAGE)):
            log.info("call: heard %d character(s) not in the call's language; dropped", len(text))
            text = ""
        if not text:
            await self._state("listening", note="I didn't catch anything. Try again?")
            return
        self.presses += 1
        self.timing = {"press": len(pcm) / BYTES_PER_SEC, "stt": self.clock() - released, "live": float(was_live)}
        self.released_at = released
        with contextlib.suppress(Closed):
            await self.ws.send_json({"type": "heard", "text": text})
        # The words are this call's, whichever call holds the chat's listener now: a desk call's `listening` stays
        # True after a phone call took the listener, so the brain is told which call heard them, marks the work
        # with this call's reader and gives this call the listener back (verification/Eve/CallScope.lean, the
        # hearing call). A brain whose hear takes no `call` is asked the old way.
        extra: dict[str, Any] = {"call": self.say} if _takes_call(self.brain) else {}
        if answer_context is _UNCAPTURED_ANSWER:
            self.brain.hear(text, **extra)      # legacy brains have no capture hook or keyword argument
        else:
            cast(CaptureBrain, self.brain).hear(text, answer_context=answer_context, **extra)

    async def run(self) -> str:
        """Until the phone hangs up or the hour is up. Returns why it ended."""
        self.reading = asyncio.ensure_future(self.read_out())
        self.opening = asyncio.ensure_future(self._open_ears())       # while the owner reaches for the button
        # Is there a chat to be in? Asked without taking the listener, which a desk call may hold: the call takes it
        # at its first press. (A brain without chat_ready is asked the old way, by taking it.)
        ready = getattr(self.brain, "chat_ready", None)
        if callable(ready):
            if not ready():
                return "eve's chat isn't running"
        elif not self._take_chat():
            return "eve's chat isn't running"
        started = self.clock()
        try:
            while True:
                if self.notes is not None:
                    started = self.clock()      # notes run to their own cap; the hour counts from when they end
                left = MAX_CALL_SECS - (self.clock() - started)
                if left <= 0:
                    return "the call reached its hour"
                wait = QUIET_SECS if self.notes is not None else min(left, QUIET_SECS)
                try:
                    op, data = await asyncio.wait_for(self.ws.recv(), wait)
                except asyncio.TimeoutError:
                    # the phone slept or the page closed without a goodbye (live 2026-09-25: a call left open
                    # that way refused the next one with "already on a call")
                    return QUIET if wait >= QUIET_SECS else "the call reached its hour"
                except Closed:
                    return DROPPED
                if op == OP_BIN:
                    if self.notes is not None:
                        self.notes.feed(data)       # notes: every frame is theirs, never a press
                    else:
                        await self._feed(data)
                    continue
                try:
                    msg = json.loads(data.decode("utf-8"))
                except (ValueError, UnicodeDecodeError):
                    continue
                kind = msg.get("type") if isinstance(msg, dict) else None
                if kind == "ping":
                    # While notes record, eve sends nothing else for an hour, so the phone cannot tell a live
                    # connection from a half-open one (review, 2026-10-04): each ping gets a pong, and a phone that
                    # hears none for a while takes it as a drop, holds its audio and calls back (CallSocket).
                    if self.notes is not None and not self.ws.closed:
                        with contextlib.suppress(Closed):
                            await self.ws.send_json({"type": "pong"})
                    continue
                if self.notes is not None and kind in ("notes_stop", "talk"):
                    # The Action Button while notes are on stops them (a ``talk`` from a build that does not know
                    # ``notes_stop`` too). The write-up then says "Notes saved." here and texts the notes.
                    if self.notes.request_stop("you pressed stop on the phone"):
                        await self._state("thinking", note="Writing up your notes.")
                elif kind == "talk":
                    await self._begin_press()
                elif kind == "done" and self.talking:
                    self.talking = False
                    # Freeze this press's context in its coroutine, before another press overwrites it.
                    answer_context, self._answer_context = self._answer_context, _UNCAPTURED_ANSWER
                    self._spawn(self.heard(bytes(self.press), self.ears_live, self.clock(),
                                           answer_context=answer_context))
                    self.press = bytearray()
                elif kind == "end":
                    return HUNG_UP
        finally:
            # Only this call's own reader: another call may have taken the chat since (a phone call started
            # during a desk call), and its reader must outlive this call (verification/Eve/DeskCall.lean).
            self.brain.listen(None, owner=self.say)
            pending = [t for t in (*self.tasks, self.reading, self.opening) if t is not None and not t.done()]
            for task in pending:
                task.cancel()
            await asyncio.gather(*pending, return_exceptions=True)
            if self.ears is not None:
                await self.ears.close()


class PhoneCalls:
    """The ``/api/call`` endpoint: authenticates the phone, then runs one Call at a time."""

    def __init__(self, check_owner: CheckOwner, brain: Optional[Brain], voice: Optional[Voice],
                 clock: Callable[[], float] = time.monotonic) -> None:
        self.check_owner, self.brain, self.voice, self.clock = check_owner, brain, voice, clock
        self.active: Optional[Call] = None
        # Notes on the phone, when the daemon lends a note-taker (daemon._start_miniapp). They belong to the
        # endpoint, not to one call, so that a call which drops and comes back carries on with the same notes.
        self.notes: Optional[PhoneNotes] = None

    async def notes_action(self, action: str, call_say: Optional[Callable[[str], None]] = None,
                           reason: str = "asked from Telegram",
                           later: Optional[Callable[[Callable[[], None]], None]] = None) -> Optional[dict[str, Any]]:
        """take_notes, when it is the phone's to answer (telegram.py and the ``notes`` command); None when it is
        the desk's note-taker's (daemon._room_notes_taker).

        ``start`` is the phone's when it was said on the Eve Link call that holds the chat (``call_say`` is that
        call's reader): a Mini App call, the desk or a text start the room's notes as before. ``stop`` and
        ``status`` are the phone's while its notes are on, from anywhere.

        ``later`` runs a callable once the turn that asked is over (telegram.py's end of turn). The call lets the
        chat's listener go only then: let go inside the turn, the turn's own reply ("Taking notes.") found no
        call to be read on and was texted instead (review, 2026-10-04; owner, 2026-09-25: "don't send a text
        unless it's necessary"). Without ``later`` the listener goes at once."""
        notes = self.notes
        if notes is None:
            return None
        if action == "start":
            call = self.active
            if call_say is None or call is None or not call.from_app or call.say != call_say:
                return None
            return await notes.start(call, later=later)
        if not notes.active:
            return None
        if action == "stop":
            return await notes.stop(reason)
        return {"ok": True, **notes.status()}

    def holding_chat(self) -> bool:
        """Whether a phone call has the chat and is using it; an idle one gives it back first (Call.release_if_idle).
        The desk's button waits only on this (daemon._desk_calls_get's phone_busy)."""
        call = self.active
        return call is not None and not call.release_if_idle()

    async def serve(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter, headers: dict[str, str],
                    token_check: Optional[Callable[[str], bool]] = None) -> None:
        """One call. With ``token_check`` it is the Eve Link app's (stick_link.py): its first message carries
        ``token`` instead of ``initData``, and only that is accepted."""
        ws = await WebSocket.accept(reader, writer, headers)
        try:
            await self._serve(ws, token_check)
        except Closed:
            pass
        finally:
            await ws.close()

    async def _serve(self, ws: WebSocket, token_check: Optional[Callable[[str], bool]] = None) -> None:
        try:
            op, data = await asyncio.wait_for(ws.recv(), AUTH_SECS)
            hello = json.loads(data.decode("utf-8")) if op == OP_TEXT else {}
        except (asyncio.TimeoutError, ValueError, UnicodeDecodeError):
            hello = {}
        user: Any = None
        can: Any = hello.get("can") if isinstance(hello, dict) else None
        if isinstance(hello, dict):
            if token_check is not None:
                user = "eve link" if token_check(str(hello.get("token") or "")) else None
            else:
                user = self.check_owner(str(hello.get("initData") or ""))
        if user is None:
            await ws.send_json({"type": "ended", "reason": "Only eve's owner can call."})
            return
        if self.brain is None or self.voice is None:
            await ws.send_json({"type": "ended", "reason": "Calls aren't set up on this computer."})
            return
        if self.active is not None:
            # the owner calling again means the old call is over (a closed page, a lost connection): end it
            old = self.active
            log.info("call: a new call replaces the one still open")
            await old.ws.close()
            async with asyncio.timeout(5):
                while self.active is old:
                    await asyncio.sleep(0.05)
        call = self.active = Call(ws, self.brain, self.voice, self.clock)
        call.from_app = token_check is not None
        call.can_notes = call.from_app and isinstance(can, list) and NOTES_CAN in can
        log.info("call: the owner is on a call from the phone")
        started = self.clock()
        reason = DROPPED
        taking_notes = False
        try:
            await ws.send_json({"type": "state", "state": "connected"})
            notes = self.notes
            if notes is not None and notes.recording and call.can_notes:
                await notes.attach(call)            # the phone called back: carry on with the same notes
            reason = await call.run()
        finally:
            taking_notes = call.notes is not None and call.notes.recording
            if self.notes is not None:
                self.notes.detach(call, reason)
            self.active = None
            log.info("call: ended after %.0f s, %d press(es)", self.clock() - started, call.presses)
        if taking_notes and reason != HUNG_UP:
            # A call that went quiet while notes record is a drop, not an end eve chose: the phone treats any
            # ``ended`` as final and would give the notes up (review, 2026-10-04). So the socket just closes, the
            # phone sees a lost connection, holds its audio and calls back within the grace (detach above).
            await ws.close(1001)
            return
        if not ws.closed:
            with contextlib.suppress(Closed):
                await ws.send_json({"type": "ended", "reason": reason})


# ---- notes on the phone (owner, 2026-10-04) ------------------------------------------------------------

NotesNotify = Callable[[str, str], Awaitable[Any]]    # (text, about): the owner's chat (telegram.tell_owner)


NOTES_PIECE_CHARS = 3500       # one Telegram message, with room under the Bot API's 4096 for the title and markup
NOTES_MAX_PIECES = 8            # past this the notes point to the file, which has everything


def _paragraphs(summary: dict[str, Any], speakers: Optional[list[str]]) -> list[str]:
    from .notes import note_sections

    out: list[str] = []
    for heading, rows in note_sections(summary, speakers):
        if heading == "Topics":
            for topic, points in rows:
                out.append(topic + ":\n" + "\n".join(f"• {p}" for p in points) if points else topic)
            continue
        out.append(heading + ":\n" + "\n".join(f"• {r}" for r in rows))
    return out


def _pack(paragraphs: list[str], limit: int) -> list[str]:
    """Paragraphs into messages of at most ``limit`` characters, cut between paragraphs, and between lines only
    when one paragraph is longer than a message."""
    pieces: list[str] = []
    current = ""
    for para in paragraphs:
        chunks = [para]
        if len(para) > limit:
            chunks, line_run = [], ""
            for line in para.split("\n"):
                while len(line) > limit:
                    if line_run:
                        chunks.append(line_run)
                        line_run = ""
                    chunks.append(line[:limit])
                    line = line[limit:]
                if line_run and len(line_run) + 1 + len(line) > limit:
                    chunks.append(line_run)
                    line_run = ""
                line_run = f"{line_run}\n{line}" if line_run else line
            if line_run:
                chunks.append(line_run)
        for chunk in chunks:
            if current and len(current) + 2 + len(chunk) > limit:
                pieces.append(current)
                current = ""
            current = f"{current}\n\n{chunk}" if current else chunk
    if current:
        pieces.append(current)
    return pieces


def notes_messages(summary: dict[str, Any], lines: int, minutes: float, path: Any, transcript: str = "",
                   speakers: Optional[list[str]] = None, limit: int = NOTES_PIECE_CHARS) -> list[str]:
    """The notes as texts to the owner: the title with the length and the number of voices, the summary, and
    every section of the notes (notes.note_sections), in as many messages as it takes, each under Telegram's
    limit. The last one says where the full transcript is, since the point of a recording is being able to go
    back to what was actually said. More than ``NOTES_MAX_PIECES`` messages of notes end in the file.

    When the file was not written (a full disk, a folder it cannot write), the texts are the only copy: they say
    so and carry the transcript itself, rather than claiming a saved file that does not exist (review,
    2026-10-04)."""
    if not lines:
        return ["I took notes on your phone, but nothing was said, so there are no notes."]
    summary = summary if isinstance(summary, dict) else {}
    title = str(summary.get("title") or "").strip().rstrip(".")
    length = f"{max(1, round(minutes))} min"
    if speakers is not None:
        length += f", {len(speakers)} {'speaker' if len(speakers) == 1 else 'speakers'}"
    paragraphs = ["Notes from your phone" + (f": {title}" if title else "") + f" ({length})."]
    gist = str(summary.get("summary") or summary.get("gist") or "").strip()
    if gist:
        paragraphs.append(gist)
    paragraphs.extend(_paragraphs(summary, speakers))
    if not summary:
        paragraphs.append(f"The write-up failed; the transcript ({lines} lines) is "
                          + ("saved." if path is not None else "below."))
    if path is None:
        paragraphs.append("I could not save them to a file, so these messages are the only copy. What was said:")
        return _pack(paragraphs + [p for p in transcript.split("\n") if p.strip()], limit)
    where = f"The full transcript is in {path.name}, in the meeting notes."
    pieces = _pack(paragraphs, limit)
    if len(pieces) > NOTES_MAX_PIECES:
        pieces = pieces[:NOTES_MAX_PIECES]
        pieces[-1] += "\n\nThere is more than fits here: the rest of the notes are in the file."
    if len(pieces[-1]) + 2 + len(where) > limit:
        pieces.append(where)
    else:
        pieces[-1] += "\n\n" + where
    return pieces


def notes_message(summary: dict[str, Any], lines: int, minutes: float, path: Any, transcript: str = "",
                  speakers: Optional[list[str]] = None) -> str:
    """notes_messages as one text (the pieces, a blank line apart)."""
    return "\n\n".join(notes_messages(summary, lines, minutes, path, transcript, speakers))


class PhoneNotes:
    """Notes on the phone: one note-taking session (notes.RoomNotes) fed from an Eve Link call, not the desk.

    The owner asked on 2026-10-04: "when i say start taking notes the phone should hold the port until i click
    action button to turn it off, this allows me to take notes on the go". So:

    * **Start.** take_notes said on an Eve Link call (PhoneCalls.notes_action) builds a session whose
      microphone is a notes.PushedAudio, and attaches it to the call: the phone is told
      ``{"type":"state","state":"notes","started":<unix seconds>,"words":<so far>}``, every audio frame it then
      sends is the notes' (not a press), and the call lets the chat's listener go, so a texted reply is not read
      out into the recording. A call whose hello did not list ``"notes"`` (an Eve Link build from before) is
      refused with the reason: it would record silence.
    * **Stop.** ``notes_stop`` (the Action Button), a ``talk``, "stop taking notes" said into the phone, the End
      button, take_notes stop from the chat or ``eve-bridge notes stop``. The phone is told
      ``{"type":"state","state":"thinking","note":"Writing up your notes."}``; once the file is written it is
      told ``{"type":"state","state":"listening","note":"Notes saved: <title>"}`` and eve says "Notes saved."
      on the call, and the notes are texted to the owner.
    * **A dropped call.** The session is the endpoint's, not the call's, so it outlives the call: what was heard
      stays heard, and the session waits on an empty microphone. The same phone calling back within
      ``NOTES_GRACE_SECS`` is attached again and carries on; otherwise the notes are written up with what there
      is, ``stopped_by`` "the phone dropped", and texted.

    The session's tasks run in a fresh context. take_notes runs inside the call's turn, where telegram.py marks
    work as the call's (its ``_FROM_CALL``) so that it is read out: the notes texted at the end would otherwise
    be read out whole on the call, an hour after the turn was over.
    """

    def __init__(self, make_taker: Callable[[Any], Any], notify: Optional[NotesNotify] = None,
                 wall: Callable[[], float] = time.time) -> None:
        self.make_taker = make_taker        # (notes.PushedAudio) -> notes.RoomNotes, recording that microphone
        self.notify = notify
        self.wall = wall
        self.taker: Any = None
        self.mic: Any = None
        self.call: Optional[Call] = None
        self.started_wall = 0.0
        self.grace: Optional[asyncio.Task[Any]] = None
        self.watch: Optional[asyncio.Task[Any]] = None

    @property
    def active(self) -> bool:
        """A session is on: recording, or being written up."""
        return self.watch is not None and not self.watch.done()

    @property
    def recording(self) -> bool:
        return self.active and self.taker is not None and self.taker.active and not self.taker.state.stopped_by

    def progress(self) -> dict[str, Any]:
        words = self.taker.state.words if self.taker is not None else 0
        return {"started": round(self.started_wall, 3), "words": words}

    def status(self) -> dict[str, Any]:
        """Counts only: the session's ``last`` (the words last heard) is left out, since this goes to the chat model
        as a tool result and the words are anyone's in a public place (the reason ``about`` never carries them)."""
        out = self.taker.status() if self.taker is not None else {"active": False}
        out = {k: v for k, v in out.items() if k != "last"}
        return {**out, "on": "your phone", "attached": self.call is not None}

    async def start(self, call: Call, later: Optional[Callable[[Callable[[], None]], None]] = None) -> dict[str, Any]:
        if self.active:
            return {"ok": True, "already": True, "on": "your phone", **self.status()}
        if not call.can_notes:
            return {"ok": False, "reason": "this Eve Link app cannot take notes yet; update it, or text me to "
                                           "take notes on the Mac's microphone instead"}
        from .notes import PushedAudio

        mic = PushedAudio()
        taker = self.make_taker(mic)
        fresh = contextvars.Context()
        result = fresh.run(taker.start)
        if not result.get("ok"):
            return result
        self.taker, self.mic, self.started_wall = taker, mic, self.wall()
        self.watch = fresh.run(asyncio.ensure_future, self._watch(taker))
        await self.attach(call, release=later is None)
        if later is not None:
            later(call.release_for_notes)       # once this turn's reply has been read out (notes_action)
        log.info("call: taking notes on the phone")
        return {**result, "on": "your phone",
                "stop": "press the Action Button, or say stop taking notes; the notes are texted when they stop"}

    async def attach(self, call: Call, release: bool = True) -> None:
        """This call is the notes' microphone: from the start, or the phone calling back after a drop. A press the
        notes cut off is dropped here as the phone drops it (Call.drop_press)."""
        if self.grace is not None:
            self.grace.cancel()
            self.grace = None
        self.call, call.notes = call, self
        call.drop_press()
        if release:
            call.release_chat()
        await call.rest()

    def detach(self, call: Call, reason: str) -> None:
        """The call ended. The End button stops the notes; anything else waits for the phone to call back."""
        if self.call is not call:
            return
        self.call, call.notes = None, None
        if not self.recording:
            return
        if reason == HUNG_UP:
            self.request_stop("you ended the call")
            return
        log.info("call: dropped while taking notes (%s); waiting %.0f s for the phone", reason, NOTES_GRACE_SECS)
        self.grace = asyncio.ensure_future(self._grace())

    async def _grace(self) -> None:
        await asyncio.sleep(NOTES_GRACE_SECS)
        if self.call is None:
            self.request_stop("the phone dropped")

    def feed(self, pcm: bytes) -> None:
        if self.mic is not None:
            self.mic.put(pcm)

    def request_stop(self, reason: str) -> bool:
        """Stop recording now; the write-up follows on its own (``_watch``). Returns whether it was recording."""
        if not self.recording:
            return False
        self.taker.request_stop(reason)
        return True

    async def stop(self, reason: str, quick: bool = False) -> dict[str, Any]:
        """Stop and wait for the write-up and the text (take_notes stop, the ``notes`` command). ``quick`` (the
        daemon stopping) writes the transcript with no summary and waits only ``NOTES_SHUTDOWN_SECS``."""
        if not self.active:
            return {"ok": False, "error": "eve is not taking notes on your phone"}
        taker, watch, call = self.taker, self.watch, self.call
        if quick and taker is not None:
            taker.quick = True
        if self.request_stop(reason) and call is not None:
            await call._state("thinking", note="Writing up your notes.")
        if watch is not None:
            with contextlib.suppress(asyncio.TimeoutError, asyncio.CancelledError):
                await asyncio.wait_for(asyncio.shield(watch),
                                       timeout=NOTES_SHUTDOWN_SECS if quick else SUMMARY_WAIT_SECS)
        path = taker.state.path
        return {"ok": True, "on": "your phone", "path": str(path) if path else None,
                "minutes": round(taker.minutes(), 1), "words": taker.state.words,
                "texted": self.notify is not None and bool(watch is not None and watch.done())}

    async def _watch(self, taker: Any) -> None:
        """Wait for the session to write its file, whatever stopped it, then tell the phone and the owner."""
        try:
            if taker.task is not None:
                await asyncio.shield(taker.task)
        except Exception as e:  # noqa: BLE001 — a session that broke still lets go of the call
            log.warning("call: the phone's notes broke (%s)", type(e).__name__)
        lines, path = len(taker.lines), taker.state.path
        minutes, words = taker.minutes(), taker.state.words
        title = str(taker.summary.get("title") or "your notes").strip().rstrip(".")
        if self.grace is not None:
            self.grace.cancel()
            self.grace = None
        call, self.call = self.call, None
        self.taker, self.mic = None, None
        if call is not None:
            call.notes = None
            if lines and path is None:
                # The file was not written: never "saved". The text below carries the transcript instead.
                await call._state("listening", note="Notes not saved to a file; I texted them to you.")
                call.say("I couldn't save the notes to a file, so I texted them to you.")
            elif lines:
                await call._state("listening", note=f"Notes saved: {title}")
                call.say("Notes saved.")
            else:
                await call._state("listening", note="Nothing was said, so there are no notes.")
                call.say("Nothing was said, so there are no notes.")
        if self.notify is not None:
            about = (f"I texted the owner the notes they took on their phone ({max(1, round(minutes))} min, "
                     f"{words} words)" + (f"; they are saved as {path.name} in the meeting notes, where "
                                          "memory_search finds them." if path is not None else "."))
            try:
                transcript = ""
                if path is None:
                    transcript = getattr(taker, "transcript", "") or "\n".join(text for _, text in taker.lines)
                speakers = getattr(taker, "speakers", None) if getattr(taker, "diarized", "no") != "no" else None
                pieces = notes_messages(taker.summary, lines, minutes, path, transcript, speakers)
                for i, piece in enumerate(pieces):
                    await self.notify(piece, about if len(pieces) == 1 else f"{about} (message {i + 1} of "
                                                                            f"{len(pieces)})")
            except Exception as e:  # noqa: BLE001 — the file is written either way
                log.warning("call: the phone's notes were not texted (%s)", type(e).__name__)
        taker.close()
