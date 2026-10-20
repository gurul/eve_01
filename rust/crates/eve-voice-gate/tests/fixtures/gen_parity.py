"""Writes python_parity.json: random scenarios run through the Python voice gate, for the Rust port to replay.

Run from the repo root:  python3 rust/crates/eve-voice-gate/tests/fixtures/gen_parity.py
Floats are written as repr strings so the Rust side can compare them bit for bit.
"""

from __future__ import annotations

import json
import math
import os
import random
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "../../../../../bridge/src"))

from eve_bridge import voice_gate as vg  # noqa: E402

BLOCK = 8
VEC = {1000: [1.0, 0.0, 0.0], 2000: [0.0, 1.0, 0.0], 3000: [0.0, 0.0, 1.0], -1500: [0.6, 0.8, 0.0],
       2500: [0.3, -0.2, 0.9]}

# The block vocabulary: every block a scenario feeds, and every block the gate can return (silence included).
VOCAB: list[bytes] = []


def blk(samples: list[int]) -> int:
    b = struct.pack(f"<{len(samples)}h", *samples)
    if b not in VOCAB:
        VOCAB.append(b)
    return VOCAB.index(b)


SPEAKERS = [1000, 2000, 3000, -1500, 2500]
BLOCKS = {
    "quiet": blk([0] * BLOCK),
    "hum": blk([40] * BLOCK),
    "unknown": blk([500] * BLOCK),            # loud, but no voice the embedder knows: embed -> None
    "half_quiet": blk([0] * (BLOCK // 2)),
}
for s in SPEAKERS:
    BLOCKS[f"s{s}"] = blk([s] * BLOCK)
    BLOCKS[f"h{s}"] = blk([s] * (BLOCK // 2))
for a in SPEAKERS:
    for b in SPEAKERS:
        if a != b:
            BLOCKS[f"m{a}_{b}"] = blk([a] * 5 + [b] * 3)
BLOCKS["edge"] = blk([-32768, 32767] * (BLOCK // 2))


class FakeEmbedder:
    def __init__(self, fail: bool = False) -> None:
        self.calls, self.fail = 0, fail

    def embed(self, pcm: bytes):
        self.calls += 1
        if self.fail:
            raise RuntimeError("onnx went away")
        total = [0.0, 0.0, 0.0]
        for (v,) in struct.iter_unpack("<h", pcm):
            if v in VEC:
                total = [a + b for a, b in zip(total, VEC[v], strict=True)]
        return total if any(total) else None


class Clock:
    def __init__(self) -> None:
        self.t = 0.0

    def __call__(self) -> float:
        return self.t


def f(x):
    return None if x is None else repr(float(x))


def summary(g):
    s = g.summary()
    return {k: (f(v) if isinstance(v, float) else v) for k, v in s.items()}


def random_segment(rng: random.Random) -> list[int]:
    kind = rng.random()
    main = rng.choice(SPEAKERS)
    n = rng.choice([1, 2, 3, 4, 5, 6, 7, 9, 12, 15, 20, 30, 45, 60])
    out = []
    for _ in range(n):
        r = rng.random()
        if kind < 0.6:
            out.append(BLOCKS[f"s{main}"] if r < 0.85 else BLOCKS[rng.choice(["quiet", "hum", f"h{main}"])])
        elif kind < 0.8:
            other = rng.choice([s for s in SPEAKERS if s != main])
            out.append(BLOCKS[rng.choice([f"s{main}", f"s{other}", f"m{main}_{other}", "quiet"])])
        elif kind < 0.9:
            out.append(BLOCKS[rng.choice(["unknown", f"s{main}", "edge"])])
        else:
            main = rng.choice(SPEAKERS) if r < 0.2 else main
            out.append(BLOCKS[f"s{main}"])
    out += [BLOCKS["quiet"]] * rng.choice([0, 1, 2, 3, 4, 5, 6])
    return out


def scenario(seed: int) -> dict:
    rng = random.Random(seed)
    mode = rng.choice(["on", "on", "on", "shadow", "off"])
    tweaks = {}
    if rng.random() < 0.4:
        tweaks["open_after_rejected_secs"] = rng.choice([3.0, 6.0, 12.0])
    if rng.random() < 0.5:
        tweaks["enrol_min_secs"] = rng.choice([0.5, 1.0, 3.0, 5.0])
    if rng.random() < 0.2:
        tweaks["first_turn_secs"] = rng.choice([0.0, 2.0, 30.0])
    if rng.random() < 0.2:
        tweaks["enrol_cap_secs"] = rng.choice([4.0, 8.0])
    if rng.random() < 0.15:
        tweaks["hang_blocks"] = rng.choice([1, 2, 6])
    config = vg.GateConfig(mode=mode, speech_rms=0.01, **tweaks)
    clock = Clock()
    no_embedder = rng.random() < 0.05
    g = vg.SpeakerGate(config, None if no_embedder else FakeEmbedder(), clock, block_bytes=BLOCK * 2)
    seed_kind = rng.random()
    if seed_kind < 0.75:
        wake = [BLOCKS["quiet"]] * rng.randint(0, 6) + [BLOCKS["s1000"]] * rng.randint(1, 12) + [BLOCKS["quiet"]] * 2
    elif seed_kind < 0.85:
        wake = [BLOCKS["quiet"]] * 5
    elif seed_kind < 0.95:
        wake = [BLOCKS[rng.choice(["s1000", "m1000_2000", "unknown", "hum"])] for _ in range(rng.randint(1, 10))]
    else:
        wake = [BLOCKS["s2000"]] * rng.randint(3, 40)
    enrolled = g.enrol_seed(b"".join(VOCAB[i] for i in wake))
    ops = []
    steps = []
    if rng.random() < 0.3:
        clock.t += rng.choice([5.0, 20.0, 100.0])
        ops.append({"op": "advance", "secs": f(clock.t)})
        steps.append(None)
    for _ in range(rng.randint(3, 14)):
        r = rng.random()
        if r < 0.62:
            blocks = random_segment(rng)
            outs = []
            for b in blocks:
                clock.t += vg.BLOCK_SECS
                outs.append([VOCAB.index(o) if o in VOCAB else blk(list(struct.unpack(f"<{len(o)//2}h", o)))
                             for o in g.process(VOCAB[b])])
            ops.append({"op": "feed", "blocks": blocks})
            steps.append(outs)
            continue
        if r < 0.72:
            v = rng.random() < 0.5
            g.lenient = v
            ops.append({"op": "lenient", "value": v})
        elif r < 0.78:
            v = rng.random() < 0.5
            g.bypass = v
            ops.append({"op": "bypass", "value": v})
        elif r < 0.86:
            secs = rng.choice([0.3, 0.8, 1.0, 1.2, 5.0, 15.0])
            clock.t += secs
            ops.append({"op": "advance", "secs": f(secs)})
        elif r < 0.9:
            g.embedder = FakeEmbedder(fail=True)
            ops.append({"op": "fail"})
        elif r < 0.905:
            g.reset()
            ops.append({"op": "reset"})
        elif r < 0.96:
            m = rng.choice(["on", "shadow", "off"])
            g.config = vg.GateConfig(mode=m, speech_rms=0.01, **tweaks)
            ops.append({"op": "mode", "value": m})
        else:
            # a block that is not a whole number of samples per ears' block
            clock.t += vg.BLOCK_SECS
            b = rng.choice([BLOCKS["half_quiet"], BLOCKS["h1000"], BLOCKS["h2000"]])
            outs = [[VOCAB.index(o) if o in VOCAB else blk(list(struct.unpack(f"<{len(o)//2}h", o)))
                     for o in g.process(VOCAB[b])]]
            ops.append({"op": "feed", "blocks": [b]})
            steps.append(outs)
            continue
        steps.append(None)
    s = g.stats
    return {
        "seed": seed, "mode": mode, "tweaks": {k: (f(v) if isinstance(v, float) else v) for k, v in tweaks.items()},
        "no_embedder": no_embedder, "wake": wake, "enrolled": enrolled, "ops": ops, "outputs": steps,
        "state": g.state, "summary": summary(g),
        "stats": {"segments": s.segments, "accepted": s.accepted, "rejected": s.rejected,
                  "secs_forwarded": f(s.secs_forwarded), "secs_silenced": f(s.secs_silenced),
                  "enrolled_secs": f(s.enrolled_secs), "opened": s.opened},
        "decisions": [[f(d.at), f(d.secs), d.state, f(d.score), f(d.threshold), d.accepted, d.why]
                      for d in s.decisions],
        "centroid": None if g._centroid is None else [f(x) for x in g._centroid],
    }


def arithmetic(rng: random.Random) -> dict:
    rms_cases = []
    for _ in range(200):
        n = rng.randint(0, 50)
        samples = [rng.randint(-32768, 32767) for _ in range(n)]
        b = struct.pack(f"<{n}h", *samples)
        if rng.random() < 0.2:
            b += b"\x07"
        rms_cases.append([b.hex(), f(vg.rms(b))])
    cos_cases = []
    for _ in range(200):
        n = rng.randint(0, 12)
        a = [rng.uniform(-1, 1) * 10 ** rng.randint(-8, 8) for _ in range(n)]
        b = [rng.uniform(-1, 1) * 10 ** rng.randint(-8, 8) for _ in range(n)]
        if rng.random() < 0.1:
            b = [0.0] * n
        cos_cases.append([[f(x) for x in a], [f(x) for x in b], f(vg.cosine(a, b)),
                          [f(x) for x in vg._unit(a)]])
    trim_cases = []
    for _ in range(100):
        bb = rng.choice([2, 4, 6, 16, 4800])
        n = rng.randint(0, 60)
        samples = [rng.choice([0, 10, 300, 400, 2000, -3000]) for _ in range(n)]
        b = struct.pack(f"<{n}h", *samples)
        if rng.random() < 0.2:
            b += b"\x01"
        thr = rng.choice([0.005, 0.01, 0.012, 0.05])
        trim_cases.append([b.hex(), f(thr), bb, vg.trim_to_speech(b, thr, bb).hex()])
    ring_cases = []
    for secs in [0.0, 0.04, 0.05, 0.06, 0.1, 0.15, 0.25, 0.3, 0.35, 1.0, 2.45, 3.0, -1.0]:
        ring = vg.WakeRing(secs)
        for i in range(60):
            ring.add(bytes([i]))
        ring_cases.append([f(secs), ring.snapshot().hex()])
    env_cases = []
    for raw in [None, "", "on", "ON", " shadow ", "\tOff\n", "loud", "\x1con\x1f", " on ", "o n",
                "SHADOW", "İ", "on\x00"]:
        for model in [None, "", "   ", " /m/x.onnx ", "~/m.onnx", "　/y　"]:
            env = {}
            if raw is not None:
                env["EVE_VOICE_GATE"] = raw
            if model is not None:
                env["EVE_VOICE_GATE_MODEL"] = model
            c = vg.configured(env)
            env_cases.append([raw, model, c.mode, c.model])
    return {"rms": rms_cases, "cosine": cos_cases, "trim": trim_cases, "ring": ring_cases, "env": env_cases}


def sherpa_tone() -> None:
    """The live embedder on a fixed two-second tone, when the model and the wheel are here (sherpa_tone.json)."""
    try:
        import sherpa_onnx
    except ImportError:
        return
    if not vg.model_present(vg.GateConfig()):
        return
    tone = b"".join(struct.pack("=h", int(math.sin(i * 0.07) * 6000.0)) for i in range(48000))
    e = vg.SherpaEmbedder(vg.DEFAULT_MODEL)
    out = {"sherpa_onnx": getattr(sherpa_onnx, "__version__", ""), "tone": [repr(x) for x in e.embed(tone)],
           "short": e.embed(tone[:4000])}
    with open(os.path.join(HERE, "sherpa_tone.json"), "w") as fh:
        json.dump(out, fh)


def main() -> None:
    rng = random.Random(20261019)
    scenarios = [scenario(s) for s in range(800)]
    data = {"block": BLOCK, "vec": {str(k): v for k, v in VEC.items()}, "arithmetic": arithmetic(rng),
            "scenarios": scenarios, "vocab": [b.hex() for b in VOCAB]}
    with open(os.path.join(HERE, "python_parity.json"), "w") as fh:
        json.dump(data, fh, separators=(",", ":"))
    sherpa_tone()
    print(f"{len(scenarios)} scenarios, {len(VOCAB)} blocks, "
          f"{sum(len(s['decisions']) for s in scenarios)} decisions")


if __name__ == "__main__":
    main()
