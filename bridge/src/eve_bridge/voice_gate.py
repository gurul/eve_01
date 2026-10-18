"""The voice gate: in a conversation, only the person who said "hey eve" reaches the model.

eve's live voice is OpenAI's Live API (gpt-live-1), which has no turn detection, noise reduction or
speaker setting a client can touch: it hears whatever the client appends. So a TV, a call or someone else in
the room were answered, kept the conversation open (any transcribed speech resets the idle timer) and wiped
eve's caption mid-reply (any transcribed speech is a barge-in). Whatever isolates a speaker has to happen
here, on the Mac, before `input_audio.append`.

HOW. The last seconds of microphone audio before the wake word are kept in RAM (ears.py), so the wake
utterance is an enrolment sample. Speech is cut into segments by energy; each segment's first HOLD of audio is
held back, embedded with a speaker-embedding model (sherpa-onnx, already a dependency — TitaNet-small, 192
numbers, ~10 ms on this Mac) and compared with the enrolled voice by cosine. A match is forwarded — the held
audio in one burst, then live; anything else is forwarded as SILENCE of the same length, because the Live
model wants a continuous stream and needs trailing quiet to close a turn. Non-speech blocks pass untouched.

    SEED         the wake utterance, trimmed to its speech. Never overwritten.
    PROVISIONAL  until ENROL_MIN seconds are enrolled NOTHING IS REJECTED. "hey eve" is ~0.7 s, and a voice
                 embedding of that is not evidence: replayed on sherpa-onnx's sample speakers, one speaker's own
                 later speech scored 0.06–0.26 against his 0.6 s seed and most of it was silenced. So the
                 speech that starts within FIRST_TURN of the wake — the owner's query, by construction — is
                 forwarded and enrolled whatever it scores; later speech is forwarded too, and enrolled only
                 when it already resembles the voice so far (T_LOOSE).
    LOCKED       ≥ ENROL_MIN seconds enrolled: a segment is forwarded when its cosine clears T_ACCEPT,
                 re-scored as it goes on. This is the only state that silences anything.
    ADAPT        only a segment close to the centroid AND to the seed moves the centroid: a TV cannot walk it.
    RESET        at the end of the conversation everything is dropped. Nothing about a voice touches the disk.

IT FAILS OPEN, except where it is sure. No model file, an embedder that raised, no usable seed, a segment too
short to judge, nothing accepted at all since the wake (the enrolment itself is suspect): all of those forward
the audio as today. It closes only on a confident mismatch — which is the TV and the other person, the long,
plainly different segments. A robot that stops hearing its owner cannot be debugged by talking to it.

ONLY FOR QUERIES. In a think-aloud lesson eve is a listener taking notes on whoever is speaking, so the
session sets `bypass` and every block passes unjudged. A conversation opened without a wake word (the lesson
toggle) has no seed, so it is ungated too.

THE NUMBERS BELOW ARE NOT FITTED. They come from a three-speaker probe on sherpa-onnx's sample recordings
(2026-09-21: TitaNet-small separated 3 s enrolments from 2 s tests cleanly, lowest same-speaker cosine 0.58,
highest different-speaker 0.39; two other popular models did not separate at all). That is why the switch
ships `off`, why `shadow` exists — every decision computed and logged, no audio changed — and why
tools/voice_gate_eval.py scores a labelled set of the owner's own room before anything is believed.
"""

from __future__ import annotations

import logging
import math
import os
import time
from collections import deque
from dataclasses import dataclass, field
from typing import Any, Callable, Mapping, Optional, Protocol, Sequence

log = logging.getLogger(__name__)

SAMPLE_RATE = 24000                       # ears.py's capture rate; the extractor resamples to 16 kHz itself
BLOCK_SECS = 0.1                          # ears.py's block: 2400 samples
MODES = ("off", "shadow", "on")
DEFAULT_MODE = "off"
DEFAULT_MODEL = "~/.config/eve-bridge/models/nemo_en_titanet_small.onnx"
MODEL_URL = ("https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/"
             "nemo_en_titanet_small.onnx")


@dataclass(frozen=True)
class GateConfig:
    mode: str = DEFAULT_MODE
    model: str = DEFAULT_MODEL
    speech_rms: float = 0.012             # a block at or above this is speech (ears.SILENCE_RMS is the dead-mic floor)
    hang_blocks: int = 4                  # quiet blocks that end a segment (0.4 s)
    hold_secs: float = 0.6                # audio held back before the first decision on a segment
    hold_more_secs: float = 1.5           # …and when that decision is "not them": hold on, and judge again on this much.
                                          # 0.6 s is nearly undecidable (one speaker's own speech: 0.20–0.41), so
                                          # nothing is REJECTED on it. Only unconvincing openings pay the extra wait.
    continue_secs: float = 1.0            # a fragment too short to judge, this soon after a judged one, gets its verdict
    min_decide_secs: float = 0.5          # a shorter segment cannot be judged: it passes
    rescore_secs: float = 0.5
    window_secs: float = 2.0              # what a re-score embeds: the last this-many seconds of the segment
    enrol_min_secs: float = 3.0
    first_turn_secs: float = 10.0         # speech starting this soon after the wake is the owner's first sentence
    enrol_cap_secs: float = 20.0
    # SET, NOT FITTED — from tools/voice_gate_eval.py replaying sherpa-onnx's three sample speakers (clean, read,
    # Mandarin, 2026-09-21) with each in turn as the owner: every OTHER voice scored ≤ 0.27, two owners scored
    # 0.45–0.59, and the third — a halting speaker — only 0.26–0.39 against his own enrolment. One number cannot
    # be right for every voice, and going deaf to the owner is the worse mistake, so the cut sits just above the
    # highest stranger rather than in the middle. Fit these on the owner's own room before trusting them.
    t_loose: float = 0.30
    t_accept: float = 0.33
    t_drop: float = 0.27
    t_adapt: float = 0.50
    t_seed_floor: float = 0.20
    open_after_rejected_secs: float = 12.0   # this much speech rejected with NOTHING accepted yet: open up


def configured(environ: Optional[Mapping[str, str]] = None) -> GateConfig:
    env = os.environ if environ is None else environ
    mode = (env.get("EVE_VOICE_GATE") or "").strip().lower() or DEFAULT_MODE
    if mode not in MODES:
        log.warning("voice gate: EVE_VOICE_GATE=%r is not one of %s; off", mode, "|".join(MODES))
        mode = "off"
    return GateConfig(mode=mode, model=(env.get("EVE_VOICE_GATE_MODEL") or DEFAULT_MODEL).strip() or DEFAULT_MODEL)


def model_present(config: GateConfig) -> bool:
    return os.path.isfile(os.path.expanduser(config.model))


class Embedder(Protocol):
    def embed(self, pcm: bytes) -> Optional[Sequence[float]]:
        """A voice embedding of int16 mono PCM at SAMPLE_RATE, or None when it cannot make one."""


class SherpaEmbedder:
    """sherpa-onnx's speaker-embedding extractor. One thread, CPU: ~10 ms for two seconds on this Mac."""

    def __init__(self, model: str) -> None:
        import sherpa_onnx

        path = os.path.expanduser(model)
        if not os.path.isfile(path):
            raise FileNotFoundError(f"no speaker model at {path} (download {MODEL_URL})")
        self._extractor = sherpa_onnx.SpeakerEmbeddingExtractor(
            sherpa_onnx.SpeakerEmbeddingExtractorConfig(model=path, num_threads=1, provider="cpu"))

    def embed(self, pcm: bytes) -> Optional[Sequence[float]]:
        import numpy as np

        samples = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0
        if samples.size < SAMPLE_RATE // 5:
            return None
        stream = self._extractor.create_stream()
        stream.accept_waveform(SAMPLE_RATE, samples)
        stream.input_finished()
        if not self._extractor.is_ready(stream):
            return None
        return list(self._extractor.compute(stream))


def rms(pcm: bytes) -> float:
    """RMS of int16 PCM as a fraction of full scale."""
    n = len(pcm) // 2
    if n == 0:
        return 0.0
    import array

    a = array.array("h")
    a.frombytes(pcm[:n * 2])
    return math.sqrt(sum(v * v for v in a) / n) / 32768.0


def cosine(a: Sequence[float], b: Sequence[float]) -> float:
    dot = sum(x * y for x, y in zip(a, b, strict=True))
    na, nb = math.sqrt(sum(x * x for x in a)), math.sqrt(sum(y * y for y in b))
    return dot / (na * nb) if na > 0 and nb > 0 else 0.0


def _unit(v: Sequence[float]) -> list[float]:
    n = math.sqrt(sum(x * x for x in v))
    return [x / n for x in v] if n > 0 else list(v)


def trim_to_speech(pcm: bytes, speech_rms: float, block_bytes: int) -> bytes:
    """The span from the first loud block to the last: the wake snapshot is mostly the room before the word."""
    blocks = [pcm[i:i + block_bytes] for i in range(0, len(pcm), block_bytes)]
    loud = [i for i, b in enumerate(blocks) if rms(b) >= speech_rms]
    return b"".join(blocks[loud[0]:loud[-1] + 1]) if loud else b""


@dataclass
class Decision:
    """One judgement of one segment, for the log and the eval. Never words, never audio."""
    at: float
    secs: float
    state: str                     # provisional | locked
    score: Optional[float]
    threshold: float
    accepted: bool
    why: str                       # match | mismatch | too_short | no_seed | embed_failed | bypass | lenient | first_turn


@dataclass
class Stats:
    segments: int = 0
    accepted: int = 0
    rejected: int = 0
    secs_forwarded: float = 0.0
    secs_silenced: float = 0.0
    enrolled_secs: float = 0.0
    opened: str = ""               # why the gate stopped judging for this conversation, "" if it did not
    decisions: list[Decision] = field(default_factory=list)


class SpeakerGate:
    """`process(block)` returns the blocks to append now — the block itself, silence of its length, nothing
    while a decision is pending, or a held burst. In `shadow` mode the decisions are made and the audio that
    comes out is exactly the audio that went in."""

    def __init__(self, config: GateConfig, embedder: Optional[Embedder],
                 clock: Callable[[], float] = time.monotonic, block_bytes: int = int(SAMPLE_RATE * BLOCK_SECS) * 2) -> None:
        self.config = config
        self.embedder = embedder
        self._clock = clock
        self._block_bytes = block_bytes
        self.bypass = False            # a think-aloud lesson: eve takes notes on whoever speaks
        self.lenient = False           # a yes/no is awaited or a task runs: a short "stop" must never be lost
        self.stats = Stats()
        self._seed: Optional[list[float]] = None
        self._centroid: Optional[list[float]] = None
        self._woke_at = clock()
        self._segment: list[bytes] = []            # the current segment's audio so far
        self._held: list[bytes] = []               # blocks not yet released (the first HOLD of a segment)
        self._quiet = 0
        self._verdict: Optional[bool] = None       # the current segment: None undecided, True forward, False silence
        self._scored_at = 0.0                      # segment seconds at the last score
        self._segment_started = 0.0
        self._rejected_secs = 0.0
        self._low_streak = 0
        self._voiced = 0                           # loud blocks in the current segment
        self._last_verdict: Optional[bool] = None  # the previous judged segment, for a fragment that follows it
        self._last_ended = -1e9
        if embedder is None:
            self.stats.opened = "no embedder"

    # -- enrolment --
    def enrol_seed(self, pcm: bytes) -> bool:
        """The audio before the wake word fired. False, and an open gate, when it holds no usable voice."""
        speech = trim_to_speech(pcm, self.config.speech_rms, self._block_bytes)
        vector = self._embed(speech) if speech else None
        if vector is None:
            self.stats.opened = self.stats.opened or "no seed"
            return False
        self._seed = _unit(vector)
        self._centroid = list(self._seed)
        self.stats.enrolled_secs = len(speech) / self._block_bytes * BLOCK_SECS
        return True

    @property
    def state(self) -> str:
        if self.stats.opened or self._seed is None:
            return "open"
        return "locked" if self.stats.enrolled_secs >= self.config.enrol_min_secs else "provisional"

    def reset(self) -> None:
        self._seed = self._centroid = None
        self._segment, self._held = [], []
        self.stats = Stats(opened="reset")

    # -- the stream --
    def process(self, block: bytes) -> list[bytes]:
        if self.bypass or self.state == "open" or self.config.mode == "off":
            # Blocks held for a decision that will not come now go out first: _end_segment drops them, and
            # that was up to the first 1.5 s of a sentence. In shadow they were heard as they came.
            pending = [] if self.config.mode == "shadow" else self._held
            self._held = []
            self._end_segment()
            return [*pending, block]
        try:
            out = self._process(block)
        except Exception as e:  # noqa: BLE001 — a gate that breaks must never deafen eve
            log.warning("voice gate: %s: %s — open for the rest of this conversation", type(e).__name__, e)
            self.stats.opened = f"error: {type(e).__name__}"
            out = self._held if self._held and self._held[-1] is block else [*self._held, block]
            self._held = []
            self._segment = []
        return out if self.config.mode == "on" else self._shadow(block)

    def _shadow(self, block: bytes) -> list[bytes]:
        return [block]                 # shadow: judged and counted as `on` would, heard exactly as the mic heard it

    def _process(self, block: bytes) -> list[bytes]:
        cfg = self.config
        loud = rms(block) >= cfg.speech_rms
        if not self._segment:
            if not loud:
                return [block]
            self._segment_started = self._clock()
            self._verdict, self._scored_at, self._quiet, self._low_streak, self._voiced = None, 0.0, 0, 0, 0
            self.stats.segments += 1
        self._quiet = 0 if loud else self._quiet + 1
        self._voiced += 1 if loud else 0
        self._segment.append(block)
        secs = len(self._segment) * BLOCK_SECS
        ended = self._quiet >= cfg.hang_blocks

        if self._verdict is None:
            self._held.append(block)
            # Judged on VOICED time: the quiet after a one-word answer must not count toward the hold, or
            # "yes" plus its own trailing silence gets embedded and judged like a sentence.
            voiced = self._voiced * BLOCK_SECS
            if voiced >= cfg.hold_secs or ended:
                last_word = ended or voiced >= cfg.hold_more_secs
                verdict = self._judge(secs, final=ended, commit=last_word)
                if verdict is None:
                    return []                                # not them on 0.6 s: keep holding, judge on more
                self._verdict = verdict
                self._scored_at = secs
                burst, self._held = self._held, []
                out = burst if self._verdict else [bytes(len(b)) for b in burst]
                self._count(out, self._verdict)
                if ended:
                    self._end_segment()
                return out
            return []

        if not ended and secs - self._scored_at >= cfg.rescore_secs:
            self._verdict = self._rejudge(secs)
        out = [block] if self._verdict else [bytes(len(block))]
        self._count(out, bool(self._verdict))
        if ended:
            self._end_segment()
        return out

    def _count(self, blocks: list[bytes], forwarded: bool) -> None:
        secs = sum(len(b) for b in blocks) / self._block_bytes * BLOCK_SECS
        if forwarded:
            self.stats.secs_forwarded += secs
        else:
            self.stats.secs_silenced += secs

    def _end_segment(self) -> None:
        if not self._segment:
            return
        secs = len(self._segment) * BLOCK_SECS
        if self._verdict is False:
            self.stats.rejected += 1
            self._rejected_secs += secs
            if self.stats.accepted == 0 and self._rejected_secs >= self.config.open_after_rejected_secs:
                # Nothing has EVER matched since the wake: the enrolment is what is wrong (the word was said
                # over the TV, a bad seed), not the room. Once something has matched this never fires — a TV
                # that talks for a minute is rejected for a minute, the conversation idles out, and the next
                # "hey eve" enrols afresh.
                self.stats.opened = "nothing matched the wake voice"
                log.info("voice gate: %.0f s of speech rejected and none accepted — open for this conversation",
                         self._rejected_secs)
        elif self._verdict is True:
            self.stats.accepted += 1
        if self._verdict is not None:
            self._last_verdict, self._last_ended = self._verdict, self._clock()
        self._segment, self._held, self._verdict = [], [], None

    # -- judging --
    def _embed(self, pcm: bytes) -> Optional[Sequence[float]]:
        if self.embedder is None or not pcm:
            return None
        return self.embedder.embed(pcm)

    def _judge(self, secs: float, final: bool, commit: bool = True) -> Optional[bool]:
        """True forward, False silence, None "not convinced yet" (only when `commit` is False)."""
        cfg = self.config
        state = self.state
        if final and self._voiced * BLOCK_SECS < cfg.min_decide_secs:
            recent = self._last_verdict is not None and self._segment_started - self._last_ended <= cfg.continue_secs
            if recent and not self.lenient and state == "locked":
                # the same speaker going on after a breath: a halting voice, the owner's or the TV's
                return self._decide(secs, state, None, 0.0, bool(self._last_verdict), "continues")
            return self._decide(secs, state, None, 0.0, True, "lenient" if self.lenient else "too_short")
        vector = self._embed(b"".join(self._segment))
        if vector is None or self._centroid is None:
            return self._decide(secs, state, None, 0.0, True, "embed_failed")
        unit = _unit(vector)
        score = cosine(unit, self._centroid)
        first_turn = state == "provisional" and self._segment_started - self._woke_at <= cfg.first_turn_secs
        if state == "provisional":
            if first_turn or score >= cfg.t_loose:
                self._adapt(unit, score, self._voiced * BLOCK_SECS, True)
            return self._decide(secs, state, score, cfg.t_loose, True, "first_turn" if first_turn else "enrolling")
        threshold = cfg.t_accept
        accepted = score >= threshold
        if not accepted and not commit and not self.lenient:
            return None
        if not accepted and self.lenient and secs < 1.5:
            return self._decide(secs, state, score, threshold, True, "lenient")
        if accepted:
            self._adapt(unit, score, self._voiced * BLOCK_SECS, False)
        return self._decide(secs, state, score, threshold, accepted, "match" if accepted else "mismatch")

    def _rejudge(self, secs: float) -> bool:
        """As a segment goes on: the last `window_secs` against the centroid. Two low scores in a row stop a
        forwarded segment (someone else took over); one good score starts a silenced one (the owner spoke up)."""
        cfg = self.config
        self._scored_at = secs
        if self.state == "provisional":
            # Still enrolling: keep forwarding, and keep folding the owner's first sentence in as it goes on.
            if self._segment_started - self._woke_at <= cfg.first_turn_secs:
                vector = self._embed(b"".join(self._segment[-int(cfg.rescore_secs / BLOCK_SECS):]))
                if vector is not None and self._centroid is not None:
                    self._adapt(_unit(vector), 1.0, cfg.rescore_secs, True)
            return True
        window = self._segment[-int(cfg.window_secs / BLOCK_SECS):]
        vector = self._embed(b"".join(window))
        if vector is None or self._centroid is None:
            return bool(self._verdict)
        unit = _unit(vector)
        score = cosine(unit, self._centroid)
        if self._verdict:
            self._low_streak = self._low_streak + 1 if score < cfg.t_drop else 0
            keep = self._low_streak < 2 or self.lenient      # an awaited answer is never cut off mid-word
            if keep and score >= cfg.t_accept:
                self._adapt(unit, score, cfg.rescore_secs, False)
            if not keep:
                self._decide(secs, self.state, score, cfg.t_drop, False, "mismatch")
            return keep
        if score >= cfg.t_accept:
            self._decide(secs, self.state, score, cfg.t_accept, True, "match")
            return True
        return False

    def _adapt(self, unit: list[float], score: float, secs: float, first_turn: bool) -> None:
        cfg = self.config
        assert self._seed is not None and self._centroid is not None
        if self.stats.enrolled_secs >= cfg.enrol_cap_secs:
            return
        if not first_turn and (score < cfg.t_adapt or cosine(unit, self._seed) < cfg.t_seed_floor):
            return
        have = self.stats.enrolled_secs
        self._centroid = _unit([(c * have + u * secs) for c, u in zip(self._centroid, unit, strict=True)])
        self.stats.enrolled_secs = min(cfg.enrol_cap_secs, have + secs)

    def _decide(self, secs: float, state: str, score: Optional[float], threshold: float, accepted: bool,
                why: str) -> bool:
        d = Decision(at=self._clock() - self._woke_at, secs=round(secs, 2), state=state,
                     score=None if score is None else round(score, 3), threshold=threshold, accepted=accepted,
                     why=why)
        self.stats.decisions.append(d)
        log.info("voice gate[%s]: %s %.1f s %s score=%s cut=%.2f (%s, enrolled %.1f s)", self.config.mode, state,
                 secs, "forward" if accepted else "silence", "-" if score is None else f"{score:.2f}", threshold,
                 why, self.stats.enrolled_secs)
        return accepted

    def summary(self) -> dict[str, Any]:
        s = self.stats
        return {"mode": self.config.mode, "segments": s.segments, "accepted": s.accepted, "rejected": s.rejected,
                "secs_forwarded": round(s.secs_forwarded, 1), "secs_silenced": round(s.secs_silenced, 1),
                "enrolled_secs": round(s.enrolled_secs, 1), "opened": s.opened}


def build(config: GateConfig, wake_audio: bytes, clock: Callable[[], float] = time.monotonic,
          embedder: Optional[Embedder] = None) -> Optional[SpeakerGate]:
    """The gate for one conversation, enrolled from the wake audio — or None when it is off or there is no wake
    audio (a conversation the lesson toggle opened). Never raises: a gate that cannot be built is no gate."""
    if config.mode == "off" or not wake_audio:
        return None
    try:
        gate = SpeakerGate(config, embedder if embedder is not None else SherpaEmbedder(config.model), clock)
        enrolled = gate.enrol_seed(wake_audio)
        log.info("voice gate[%s]: %s", config.mode,
                 f"enrolled {gate.stats.enrolled_secs:.1f} s from the wake" if enrolled else "no usable wake voice — open")
        return gate
    except Exception as e:  # noqa: BLE001
        log.warning("voice gate: off for this conversation (%s: %s)", type(e).__name__, e)
        return None


_RING_SECS = 3.0


class WakeRing:
    """The last few seconds of microphone audio, in RAM only, so the wake utterance can enrol a voice."""

    def __init__(self, secs: float = _RING_SECS) -> None:
        self._blocks: deque[bytes] = deque(maxlen=max(1, round(secs / BLOCK_SECS)))

    def add(self, raw: bytes) -> None:
        self._blocks.append(raw)

    def snapshot(self) -> bytes:
        return b"".join(self._blocks)

    def clear(self) -> None:
        self._blocks.clear()
