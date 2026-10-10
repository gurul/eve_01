#!/usr/bin/env python3
"""The voice gate on recordings: how much of the owner is lost, how much of everyone else gets through.

    .venv/bin/python tools/voice_gate_eval.py --wake wake.wav --owner me1.wav me2.wav --other tv.wav guest.wav
    .venv/bin/python tools/voice_gate_eval.py --wake wake.wav --wake-secs 0.8 --owner … --other …   # a short wake

Replays wav files through the REAL gate (voice_gate.SpeakerGate in `on` mode) with the REAL speaker model, in
100 ms blocks on a fake clock, exactly as voice_agent._pump_mic feeds it: the wake clip enrols, then the
owner's clips and the others' clips are streamed in the order given, a second of quiet between them.

It prints, per clip, the seconds of SPEECH forwarded and silenced, and the two numbers that matter:

    owner speech silenced      the robot going deaf to its owner   (bar, fixed 2026-09-21: ≤ 5 %)
    other speech forwarded     what the gate exists to stop        (bar: ≤ 20 %; with the gate off it is 100 %)

The cut-offs in voice_gate.GateConfig are NOT fitted. Fit them on recordings from the room eve lives in —
the owner at 0.5, 1.5 and 3 m on different days, one-word answers, other people, a TV, eve's own voice from
the Mac speaker — split by recording day into a set to fit on and a set scored once. `shadow` mode
(EVE_VOICE_GATE=shadow) collects real score distributions with no recording at all: it logs every
decision's score and changes no audio.

Nothing here is written anywhere: the wavs are read, the numbers are printed.
"""

from __future__ import annotations

import argparse
import sys
import wave
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from eve_bridge import voice_gate as vg  # noqa: E402

BAR_OWNER_SILENCED = 0.05
BAR_OTHER_FORWARDED = 0.20
BLOCK_BYTES = int(vg.SAMPLE_RATE * vg.BLOCK_SECS) * 2


def read_wav(path: str, secs: float = 0.0) -> bytes:
    """int16 mono PCM at the gate's 24 kHz (linear resampling: the extractor resamples again to 16 kHz)."""
    import numpy as np

    with wave.open(path, "rb") as w:
        if w.getsampwidth() != 2:
            raise SystemExit(f"{path}: 16-bit PCM wav only")
        data = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(np.float32)
        if w.getnchannels() > 1:
            data = data.reshape(-1, w.getnchannels()).mean(axis=1)
        rate = w.getframerate()
    if rate != vg.SAMPLE_RATE:
        n = int(len(data) * vg.SAMPLE_RATE / rate)
        data = np.interp(np.linspace(0, len(data) - 1, n), np.arange(len(data)), data)
    if secs > 0:
        data = data[:int(secs * vg.SAMPLE_RATE)]
    return data.astype(np.int16).tobytes()


def blocks_of(pcm: bytes) -> list[bytes]:
    out = [pcm[i:i + BLOCK_BYTES] for i in range(0, len(pcm), BLOCK_BYTES)]
    if out and len(out[-1]) < BLOCK_BYTES:
        out[-1] = out[-1] + bytes(BLOCK_BYTES - len(out[-1]))
    return out


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--wake", required=True, help="the wake utterance (what enrols the conversation's speaker)")
    p.add_argument("--wake-secs", type=float, default=0.0, help="use only this much of --wake (a real 'hey eve' is ~0.7 s)")
    p.add_argument("--owner", nargs="+", default=[], help="clips of the same person as --wake")
    p.add_argument("--other", nargs="+", default=[], help="clips of anyone or anything else")
    p.add_argument("--model", default=vg.DEFAULT_MODEL)
    p.add_argument("--check", action="store_true", help="exit 1 unless both bars are met")
    args = p.parse_args(argv)

    now = [0.0]
    cfg = vg.GateConfig(mode="on", model=args.model)
    gate = vg.build(cfg, read_wav(args.wake, args.wake_secs), clock=lambda: now[0])
    if gate is None or gate.state == "open":
        print(f"the gate is open ({gate.stats.opened if gate else 'not built'}): nothing to measure")
        return 1
    print(f"enrolled {gate.stats.enrolled_secs:.1f} s from {Path(args.wake).name}; cut-offs: loose {cfg.t_loose} "
          f"accept {cfg.t_accept} drop {cfg.t_drop} (NOT fitted)")
    totals = {"owner": [0.0, 0.0], "other": [0.0, 0.0]}           # [speech secs forwarded, speech secs silenced]
    for kind, paths in (("owner", args.owner), ("other", args.other)):
        for path in paths:
            forwarded = silenced = 0.0
            seen = len(gate.stats.decisions)
            pending: list[bytes] = []                                # inputs not yet answered (the hold)
            for block in [*blocks_of(read_wav(path)), *[bytes(BLOCK_BYTES)] * 10]:
                now[0] += vg.BLOCK_SECS
                pending.append(block)
                for piece in gate.process(block):
                    src = pending.pop(0)
                    if vg.rms(src) < cfg.speech_rms:
                        continue                                     # only SPEECH seconds are scored
                    if any(piece):
                        forwarded += vg.BLOCK_SECS
                    else:
                        silenced += vg.BLOCK_SECS
            totals[kind][0] += forwarded
            totals[kind][1] += silenced
            scores = [f"{d.score:.2f}" if d.score is not None else d.why for d in gate.stats.decisions[seen:]]
            print(f"  {kind:<5} {Path(path).name:<28} speech forwarded {forwarded:5.1f} s  silenced {silenced:5.1f} s"
                  f"   [{gate.state}] scores {' '.join(scores[:8])}")
    own_f, own_s = totals["owner"]
    oth_f, oth_s = totals["other"]
    owner_lost = own_s / (own_f + own_s) if own_f + own_s else 0.0
    other_heard = oth_f / (oth_f + oth_s) if oth_f + oth_s else 0.0
    print(f"\nowner speech silenced   {owner_lost:6.1%}   (bar ≤ {BAR_OWNER_SILENCED:.0%})")
    print(f"other speech forwarded  {other_heard:6.1%}   (bar ≤ {BAR_OTHER_FORWARDED:.0%}; gate off = 100%)")
    print(f"gate: {gate.summary()}")
    ok = owner_lost <= BAR_OWNER_SILENCED and other_heard <= BAR_OTHER_FORWARDED
    print("VOICE_GATE_BARS_MET" if ok else "VOICE_GATE_BARS_NOT_MET")
    return 0 if ok or not args.check else 1


if __name__ == "__main__":
    raise SystemExit(main())
