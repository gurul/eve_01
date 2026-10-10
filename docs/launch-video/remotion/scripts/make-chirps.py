"""Synthesise the film's robot sounds into public/audio/sfx-*.wav.

Modelled on the firmware's chirps (firmware/claude_pet_stackchan/src/chirp.h):
R2D2-style square-wave sweeps and beeps with a 2 ms attack/release per segment.
The square is softened with a sine blend so it sits under the music bed.
Deterministic (fixed seeds), stdlib only: python3 scripts/make-chirps.py
"""
import math
import random
import struct
import wave
from pathlib import Path

RATE = 44100
OUT = Path(__file__).resolve().parent.parent / "public" / "audio"


def seg(f0, f1, ms, amp=0.5):
    n = int(RATE * ms / 1000)
    edge = int(RATE * 0.002)
    out, phase = [], 0.0
    for i in range(n):
        f = f0 + (f1 - f0) * i / max(1, n - 1)
        phase += 2 * math.pi * f / RATE
        sq = 1.0 if math.sin(phase) >= 0 else -1.0
        s = 0.35 * sq + 0.65 * math.sin(phase)
        env = min(1.0, i / edge if edge else 1, (n - i) / edge if edge else 1)
        out.append(s * amp * env)
    return out


def gap(ms):
    return [0.0] * int(RATE * ms / 1000)


def write(name, samples):
    OUT.mkdir(parents=True, exist_ok=True)
    with wave.open(str(OUT / name), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(b"".join(struct.pack("<h", int(max(-1, min(1, s)) * 32000)) for s in samples))


# wake: short rising whistle, two notes
write("sfx-wake.wav", seg(900, 1500, 90) + gap(20) + seg(1400, 2300, 120))
# happy: a trill of 6 rising beeps
rng = random.Random(7)
trill = []
for k in range(6):
    f = 1500 + k * 180 + rng.randint(-60, 60)
    trill += seg(f, f * 1.08, 55, 0.42) + gap(18)
write("sfx-happy.wav", trill)
# pop: one high blip, for things landing
write("sfx-pop.wav", seg(2200, 1700, 45, 0.38))
# ok: beep-boop, for an approval
write("sfx-ok.wav", seg(1900, 1900, 70, 0.42) + gap(25) + seg(1300, 1200, 90, 0.42))
print("wrote", sorted(p.name for p in OUT.glob("sfx-*.wav")))
