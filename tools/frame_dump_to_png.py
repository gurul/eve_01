#!/usr/bin/env python3
"""Turn a `[frame] W H` hex dump from the firmware's look debug output (look.cpp) into a PNG.

Usage: frame_dump_to_png.py <capture.log> <out.png> [scale]

Reads the first `[frame] W H` ... `[frame] end` block in the capture, decodes
the RRGGBB hex triplets, upscales by nearest neighbour (default 6x) and writes
a PNG with no dependencies beyond the stdlib (zlib + struct). Used on the
bench to check camera channel order and white balance by eye.
"""
import struct
import sys
import zlib


def read_frame(path: str):
    w = h = 0
    rows = []
    with open(path, encoding="utf-8", errors="replace") as f:
        lines = [ln.rstrip("\n") for ln in f]
    i = 0
    while i < len(lines):
        ln = lines[i]
        # capture lines may carry a leading timestamp column
        if "[frame]" in ln and "end" not in ln:
            parts = ln.split("[frame]")[1].split()
            w, h = int(parts[0]), int(parts[1])
            i += 1
            while i < len(lines) and len(rows) < h:
                hexline = lines[i].split()[-1] if lines[i].strip() else ""
                if len(hexline) == w * 6:
                    rows.append(bytes.fromhex(hexline))
                i += 1
            break
        i += 1
    if not rows or len(rows) != h:
        raise SystemExit(f"no complete frame found ({len(rows)}/{h} rows)")
    return w, h, rows


def write_png(path: str, w: int, h: int, rows, scale: int) -> None:
    raw = bytearray()
    for r in rows:
        line = bytearray()
        for x in range(w):
            px = r[x * 3 : x * 3 + 3]
            line += px * scale
        for _ in range(scale):
            raw += b"\x00" + line

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w * scale, h * scale, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


def main() -> None:
    if len(sys.argv) < 3:
        raise SystemExit(__doc__)
    scale = int(sys.argv[3]) if len(sys.argv) > 3 else 6
    w, h, rows = read_frame(sys.argv[1])
    write_png(sys.argv[2], w, h, rows, scale)
    # mean colour is a cheap sanity number for the log
    tot = [0, 0, 0]
    for r in rows:
        for x in range(w):
            for c in range(3):
                tot[c] += r[x * 3 + c]
    n = w * h
    print(f"wrote {sys.argv[2]} {w}x{h}x{scale} mean R={tot[0]//n} G={tot[1]//n} B={tot[2]//n}")


if __name__ == "__main__":
    main()
