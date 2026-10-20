#!/usr/bin/env python3
"""Write the CJK codec tables eve-protocol embeds, read from CPython's own codecs.

The Python daemon encodes heartbeat strings with ``str.encode(codec, "replace")``
for gbk, big5 and shift_jis. Those are CPython's tables, which differ from the
WHATWG tables in encoding_rs (WHATWG GBK adds the euro sign and gb18030 extras,
WHATWG Big5 is Big5-HKSCS, WHATWG Shift_JIS is windows-31j). So the Rust port
ships CPython's tables, generated here:

    <codec>.bin            sorted records of 5 bytes: code point (3 bytes, big
                           endian), then its encoding (2 bytes, big endian; a
                           one-byte encoding is stored as 0x00XX). ASCII code
                           points that encode to themselves are left out.
    <codec>.roundtrip.bin  sorted records of 6 bytes: a code point whose
                           encoding decodes to a different code point (3
                           bytes), then that code point (3 bytes).

Run it with the bridge's interpreter so the tables are the daemon's:

    python3 -I tools/gen_codec_tables.py [out_dir]     # default: ../data
"""
from __future__ import annotations

import sys
from pathlib import Path

CODECS = ("gbk", "big5", "shift_jis")


def tables(codec: str) -> tuple[bytes, bytes]:
    enc = bytearray()
    rt = bytearray()
    for cp in range(0x110000):
        if 0xD800 <= cp <= 0xDFFF:
            continue
        ch = chr(cp)
        try:
            b = ch.encode(codec)
        except UnicodeEncodeError:
            continue
        if len(b) not in (1, 2) or (len(b) == 2 and b[0] == 0):
            raise SystemExit(f"{codec}: U+{cp:04X} encodes to {b!r}; the table format cannot hold it")
        back = b.decode(codec)
        if len(back) != 1:
            raise SystemExit(f"{codec}: U+{cp:04X} decodes back to {back!r}")
        if cp < 0x80 and b == bytes([cp]) and back == ch:
            continue
        if cp < 0x80:
            raise SystemExit(f"{codec}: ASCII U+{cp:04X} is not identity ({b!r})")
        code = int.from_bytes(b, "big")
        enc += cp.to_bytes(3, "big") + code.to_bytes(2, "big")
        if back != ch:
            rt += cp.to_bytes(3, "big") + ord(back).to_bytes(3, "big")
    return bytes(enc), bytes(rt)


def main() -> None:
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parent.parent / "data"
    out.mkdir(parents=True, exist_ok=True)
    for codec in CODECS:
        enc, rt = tables(codec)
        (out / f"{codec}.bin").write_bytes(enc)
        (out / f"{codec}.roundtrip.bin").write_bytes(rt)
        print(f"{codec}: {len(enc) // 5} mappings, {len(rt) // 6} round-trip overrides")


if __name__ == "__main__":
    main()
