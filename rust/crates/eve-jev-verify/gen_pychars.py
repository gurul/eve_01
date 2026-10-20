# Writes src/pychars.rs from the running Python (run with the daemon's Python 3.13):
#   python -I gen_pychars.py > src/pychars.rs
import sys
import unicodedata


def ranges(pred):
    out = []
    start = None
    for cp in range(0x110000):
        ok = not (0xD800 <= cp <= 0xDFFF) and pred(chr(cp))
        if ok and start is None:
            start = cp
        if not ok and start is not None:
            out.append((start, cp - 1))
            start = None
    if start is not None:
        out.append((start, 0x10FFFF))
    return out


def emit(name, rs, doc):
    print(f"/// {doc}")
    print(f"pub(crate) static {name}: [(u32, u32); {len(rs)}] = [")
    line = "   "
    for a, b in rs:
        item = f" (0x{a:x}, 0x{b:x}),"
        if len(line) + len(item) > 110:
            print(line)
            line = "   "
        line += item
    print(line)
    print("];\n")


def emit_map(name, pairs, doc):
    print(f"/// {doc}")
    print(f"pub(crate) static {name}: [(char, &str); {len(pairs)}] = [")
    for cp, f in pairs:
        s = "".join(f"\\u{{{ord(x):x}}}" for x in f)
        print(f"    ('\\u{{{cp:x}}}', \"{s}\"),")
    print("];")


chars = [cp for cp in range(0x110000) if not 0xD800 <= cp <= 0xDFFF]
print("//! Python's character classes and `str.casefold()`, generated from CPython", sys.version.split()[0], end="")
print(f" (Unicode {unicodedata.unidata_version}).")
print("//! Data copied from CPython, not hand-picked: regenerate it the same way rather than editing rows.\n")
emit("ALPHA", ranges(str.isalpha), "`str.isalpha()`.")
emit("DECIMAL", ranges(str.isdecimal), "`str.isdecimal()`: what `\\d` matches in a `str` pattern.")
emit("DIGIT", ranges(str.isdigit), "`str.isdigit()`.")
emit("NUMERIC", ranges(str.isnumeric), "`str.isnumeric()`.")
emit_map("FOLDS", [(cp, chr(cp).casefold()) for cp in chars if chr(cp).casefold() != chr(cp).lower()],
         "Every character whose `casefold()` is not its `lower()`, with its fold, sorted.")
print()
emit_map("LOWER", [(cp, chr(cp).lower()) for cp in chars if chr(cp).lower() != chr(cp)],
         "Every character `lower()` changes, with its lowercase, sorted (so the fold does not depend on Rust's Unicode).")
