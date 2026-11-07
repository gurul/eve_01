"""eve's voice on this Mac: Kokoro-82M (hexgrad, Apache-2.0) through kokoro-onnx, English only.

The owner, 2026-11-06: "can we make it so that the tts and stt only use english", then "use kokoro". Calls, Voice PE
presses and the app's push-to-talk speak through ``phone_call.OpenAIVoice.speak_stream``; with this voice on, the
words are made here instead of on OpenRouter (deepgram/aura-2), offline and free.

English is locked in two places. The phonemizer is always American English (``LANG``), so any text is read as English
words. The voice must be one of Kokoro's English speakers (``ENGLISH_PREFIXES``: American and British, female and
male); any other name falls back to ``DEFAULT_VOICE``.

Measured on the owner's M-series Mac, 2026-11-06, CPU (onnxruntime 1.31): the model loads in 0.4 s, a short reply
("Hi.") takes 0.2 s and a two-clause sentence of 4.4 s audio takes 0.75 s. deepgram/aura-2 on OpenRouter starts
sounding in 0.31 s. To start sooner, a sentence is cut at commas and the like (``chunks``), and each piece is
streamed as soon as it is made. OpenRouter's own hexgrad/kokoro-82m took 7-10 s (2026-09-25), so the voice runs
locally and never through OpenRouter.

The model files (``MODEL_FILE``, ``VOICES_FILE``, from the kokoro-onnx release "model-files-v1.0") live in
``~/.config/eve-bridge/kokoro`` (``EVE_KOKORO_DIR``). ``eve-bridge kokoro-setup`` downloads them and checks their
SHA-256 values. If the package or the files are missing, ``load`` returns None, and the call uses OpenRouter's voice.
"""

from __future__ import annotations

import hashlib
import logging
import os
import re
import threading
from pathlib import Path
from typing import Any, Iterator, Optional

log = logging.getLogger(__name__)

SAMPLE_RATE = 24000                    # Kokoro's own rate, the same as a call's (phone_call.SAMPLE_RATE)
LANG = "en-us"
DEFAULT_VOICE = "af_heart"
ENGLISH_PREFIXES = ("af_", "am_", "bf_", "bm_")
DEFAULT_SPEED = 1.0
CHUNK_BYTES = 4800                     # 100 ms of 16-bit mono at 24 kHz, the size OpenRouter's stream was read in
MIN_PIECE_CHARS = 24                   # a piece shorter than this joins the next: tiny pieces sound choppy
RELEASE = "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/"
MODEL_FILE = "kokoro-v1.0.onnx"
VOICES_FILE = "voices-v1.0.bin"
SHA256 = {MODEL_FILE: "7d5df8ecf7d4b1878015a32686053fd0eebe2bc377234608764cc0ef3636a6c5",
          VOICES_FILE: "bca610b8308e8d99f32e6fe4197e7ec01679264efed0cac9140fe9c29f1fbf7d"}

_CUT = re.compile(r"(?<=[.!?;:,])\s+")


def model_dir(environ: Any = None) -> Path:
    env = os.environ if environ is None else environ
    raw = (env.get("EVE_KOKORO_DIR") or "").strip()
    return Path(raw).expanduser() if raw else Path.home() / ".config" / "eve-bridge" / "kokoro"


def english_voice(name: str) -> str:
    """``name`` when it is one of Kokoro's English speakers, else ``DEFAULT_VOICE``."""
    name = (name or "").strip().lower()
    return name if name.startswith(ENGLISH_PREFIXES) and re.fullmatch(r"[a-z]{2}_[a-z]+", name) else DEFAULT_VOICE


def chunks(text: str) -> list[str]:
    """``text`` cut after sentence and clause punctuation, so the first piece can play while the rest are made.
    A piece shorter than MIN_PIECE_CHARS joins the next one. Joining the pieces with spaces gives back the text
    with its whitespace collapsed."""
    out: list[str] = []
    buf = ""
    for part in _CUT.split(" ".join(text.split())):
        buf = f"{buf} {part}" if buf else part
        if len(buf) >= MIN_PIECE_CHARS:
            out.append(buf)
            buf = ""
    if buf:
        if out and len(buf) < MIN_PIECE_CHARS:
            out[-1] = f"{out[-1]} {buf}"
        else:
            out.append(buf)
    return out


def to_pcm16(samples: Any) -> bytes:
    """Kokoro's float samples (-1..1) as 16-bit little-endian PCM, clipped."""
    import numpy as np

    return (np.clip(np.asarray(samples, dtype=np.float32), -1.0, 1.0) * 32767.0).astype("<i2").tobytes()


class KokoroVoice:
    """One loaded Kokoro model. ``speak_stream`` is blocking (run it on a thread), and a lock lets one sentence be
    made at a time."""

    def __init__(self, engine: Any, voice: str = DEFAULT_VOICE, speed: float = DEFAULT_SPEED) -> None:
        self.engine = engine
        self.voice = english_voice(voice)
        self.speed = speed
        self._lock = threading.Lock()

    def speak_stream(self, text: str) -> Iterator[bytes]:
        for piece in chunks(text):
            with self._lock:
                samples, rate = self.engine.create(piece, voice=self.voice, speed=self.speed, lang=LANG)
            if rate != SAMPLE_RATE:
                raise RuntimeError(f"Kokoro returned {rate} Hz, not {SAMPLE_RATE}")
            pcm = to_pcm16(samples)
            for i in range(0, len(pcm), CHUNK_BYTES):
                yield pcm[i:i + CHUNK_BYTES]


def load(environ: Any = None) -> Optional[KokoroVoice]:
    """The local voice, or None (logged) when kokoro-onnx or the model files are missing."""
    env = os.environ if environ is None else environ
    folder = model_dir(env)
    model, voices = folder / MODEL_FILE, folder / VOICES_FILE
    if not (model.is_file() and voices.is_file()):
        log.warning("voice: Kokoro's model files are not in %s (run `eve-bridge kokoro-setup`); using OpenRouter's "
                    "voice", folder)
        return None
    try:
        from kokoro_onnx import Kokoro
    except ImportError as e:
        log.warning("voice: kokoro-onnx is not importable (%s); using OpenRouter's voice", e)
        return None
    try:
        engine = Kokoro(str(model), str(voices))
    except Exception as e:  # noqa: BLE001 - a broken model file must not stop calls; the other voice takes over
        log.warning("voice: Kokoro did not load (%s); using OpenRouter's voice", type(e).__name__)
        return None
    try:
        speed = float(env.get("EVE_KOKORO_SPEED") or DEFAULT_SPEED)
    except ValueError:
        speed = DEFAULT_SPEED
    speed = min(max(speed, 0.5), 2.0)
    voice = KokoroVoice(engine, env.get("EVE_KOKORO_VOICE") or DEFAULT_VOICE, speed)
    try:
        # the first sentence after a load took 1.4 s, a warm one 0.2-0.8 s (2026-11-06): warm it up here
        engine.create("Hi.", voice=voice.voice, speed=speed, lang=LANG)
    except Exception as e:  # noqa: BLE001
        log.warning("voice: Kokoro could not speak (%s); using OpenRouter's voice", type(e).__name__)
        return None
    log.info("voice: Kokoro (%s, speed %.2f) speaks", voice.voice, speed)
    return voice


def setup(environ: Any = None) -> int:
    """Download the model files into ``model_dir`` and check them. 0 when both are in place and match."""
    import httpx

    folder = model_dir(environ)
    folder.mkdir(parents=True, exist_ok=True)
    for name, want in SHA256.items():
        path = folder / name
        if path.is_file() and _sha256(path) == want:
            print(f"{name}: already in place")
            continue
        part = path.with_suffix(path.suffix + ".part")
        print(f"{name}: downloading")
        with httpx.stream("GET", RELEASE + name, follow_redirects=True, timeout=httpx.Timeout(60.0, connect=10.0)) as r:
            r.raise_for_status()
            with part.open("wb") as f:
                for block in r.iter_bytes(1 << 20):
                    f.write(block)
        got = _sha256(part)
        if got != want:
            part.unlink(missing_ok=True)
            print(f"{name}: checksum mismatch ({got}); not installed")
            return 1
        part.replace(path)
        print(f"{name}: installed")
    print(f"Kokoro is ready in {folder}")
    return 0


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()
