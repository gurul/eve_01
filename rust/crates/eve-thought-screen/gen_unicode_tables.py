"""Regenerate src/unicode_tables.rs from the Python the daemon runs: python3 gen_unicode_tables.py"""

import pathlib
import sys
import unicodedata

ranges, start, prev = [], None, None
for c in range(0x110000):
    if not chr(c).isprintable():
        if start is None:
            start = prev = c
        elif c == prev + 1:
            prev = c
        else:
            ranges.append((start, prev))
            start = prev = c
ranges.append((start, prev))

v = sys.version_info
lines = [
    f"// Generated from CPython {v.major}.{v.minor} (unicodedata {unicodedata.unidata_version}): the code points for which",
    "// str.isprintable() is False, as inclusive ranges. Regenerate with gen_unicode_tables.py when the daemon",
    "// moves to a new Python.",
    "pub(crate) const NOT_PRINTABLE: &[(u32, u32)] = &[",
]
row = [f"(0x{a:x}, 0x{b:x})" for a, b in ranges]
for i in range(0, len(row), 6):
    lines.append("    " + ", ".join(row[i:i + 6]) + ",")
lines.append("];")
lines += [
    "",
    "// The code points that str.lower() changes, with what they become (the context rule for U+03A3 aside).",
    "pub(crate) const LOWER: &[(u32, &str)] = &[",
]
row = []
for c in range(0x110000):
    if 0xD800 <= c < 0xE000:
        continue
    low = chr(c).lower()
    if low != chr(c):
        row.append(f'(0x{c:x}, "' + "".join(f"\\u{{{ord(x):x}}}" for x in low) + '")')
for i in range(0, len(row), 6):
    lines.append("    " + ", ".join(row[i:i + 6]) + ",")
lines.append("];")
out = pathlib.Path(__file__).parent / "src" / "unicode_tables.rs"
out.write_text("\n".join(lines) + "\n")
