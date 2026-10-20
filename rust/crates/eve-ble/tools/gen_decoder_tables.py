#!/usr/bin/env python3
"""Write the incremental-decoder tables eve-ble embeds, read from CPython's own codecs.

``ble._codec_safe_chunks`` feeds a gbk / big5 / shift_jis payload to
``codecs.getincrementaldecoder(codec)(errors="replace")`` one byte at a time and
ends a chunk only where ``decoder.getstate()[0]`` (the pending bytes) is empty.
These CPython decoders hold at most one pending byte, so the whole automaton is
two facts per codec, written as one file ``<codec>.dec.bin`` of 4128 bytes:

    bytes 0..32      bitset over b in 0..256: feeding b to an empty decoder
                     leaves b pending (a lead byte).
    bytes 32..4128   bitset over (lead - 0x80) * 256 + trail: with ``lead``
                     pending, feeding ``trail`` completes a character. When it
                     does not, CPython replaces the lead and reads ``trail``
                     afresh, so the next state is "trail pending" exactly when
                     ``trail`` is itself a lead byte.

The script proves that model against the real decoder (every byte, every
lead/trail pair, and random byte strings) before writing anything.

Run it with the bridge's interpreter so the tables are the daemon's:

    python3 -I tools/gen_decoder_tables.py [out_dir]     # default: ../data
"""
from __future__ import annotations

import codecs
import random
import sys
from pathlib import Path

CODECS = ("gbk", "big5", "shift_jis")


def pending(dec) -> bytes:
    return dec.getstate()[0]


def fresh(codec: str):
    return codecs.getincrementaldecoder(codec)(errors="replace")


def tables(codec: str) -> tuple[list[bool], dict[int, list[bool]]]:
    starts = []
    for b in range(256):
        d = fresh(codec)
        d.decode(bytes([b]))
        p = pending(d)
        assert p in (b"", bytes([b])), (codec, b, p)
        starts.append(p == bytes([b]))
    assert not any(starts[:0x80]), codec
    pairs: dict[int, list[bool]] = {}
    for lead in range(0x80, 0x100):
        row = []
        for trail in range(256):
            if not starts[lead]:
                row.append(False)
                continue
            d = fresh(codec)
            d.decode(bytes([lead]))
            d.decode(bytes([trail]))
            p = pending(d)
            done = p == b""
            if not done:
                # Not a character: the lead was replaced and the trail is pending as a new lead.
                assert p == bytes([trail]) and starts[trail], (codec, lead, trail, p)
            row.append(done)
        pairs[lead] = row
    return starts, pairs


def model_pending(starts, pairs, data: bytes) -> list[bool]:
    out = []
    pend = None
    for b in data:
        if pend is None:
            pend = b if starts[b] else None
        elif pairs[pend][b]:
            pend = None
        else:
            pend = b if starts[b] else None
        out.append(pend is not None)
    return out


def real_pending(codec: str, data: bytes) -> list[bool]:
    d = fresh(codec)
    out = []
    for i in range(len(data)):
        d.decode(data[i:i + 1])
        out.append(bool(pending(d)))
    return out


def prove(codec: str, starts, pairs) -> None:
    rng = random.Random(20261019)
    for _ in range(3000):
        n = rng.randrange(1, 40)
        # Mix of high bytes (leads), ASCII, and valid text so every branch is hit.
        data = bytes(rng.choice((rng.randrange(0x80, 0x100), rng.randrange(0, 0x80), rng.randrange(0x40, 0xFF)))
                     for _ in range(n))
        assert model_pending(starts, pairs, data) == real_pending(codec, data), (codec, data)
    text = "".join(chr(rng.randrange(0x20, 0x9FFF)) for _ in range(4000))
    data = text.encode(codec, "replace")
    assert model_pending(starts, pairs, data) == real_pending(codec, data), codec


def bitset(bits: list[bool]) -> bytes:
    out = bytearray((len(bits) + 7) // 8)
    for i, v in enumerate(bits):
        if v:
            out[i // 8] |= 1 << (i % 8)
    return bytes(out)


def main() -> None:
    out_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parent.parent / "data"
    out_dir.mkdir(parents=True, exist_ok=True)
    for codec in CODECS:
        starts, pairs = tables(codec)
        prove(codec, starts, pairs)
        flat = [v for lead in range(0x80, 0x100) for v in pairs[lead]]
        blob = bitset(starts) + bitset(flat)
        assert len(blob) == 32 + 4096
        (out_dir / f"{codec}.dec.bin").write_bytes(blob)
        print(f"{codec}: {sum(starts)} lead bytes, {sum(flat)} pairs")
    print(sys.version)


if __name__ == "__main__":
    main()
