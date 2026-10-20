"""Writes differential.json: random markdown-ish inputs and what Python's telegram_format returns for
each, so the Rust port is checked against the Python module itself. Run from the repository root:

    bridge/.venv/bin/python rust/crates/eve-telegram-format/tests/fixtures/gen_differential.py
"""
from __future__ import annotations

import json
import random
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[4] / "bridge" / "src"))
from eve_bridge import telegram_format as fmt  # noqa: E402

TOKENS = [
    "a", "word", "Word", "x_y", "_it_", "*it*", "**b**", "__u__", "~~s~~", "`code`", "`", "``", "*", "**", "_",
    "~", "~~", "#", "## ", "### Head ##", "- ", "* ", "+ ", "• ", "◦ ", "1. ", "12) ", "> ", ">",
    "---", "***", "___", "|", " | ", "|---|", "|:--|--:|", "```", "```py", "~~~", "\n", "\n\n", "\n\n\n", " ",
    "  ", "\t", "\r\n", "\r", " ", "\x1c", " ", "　", "—", "–", " — ", "—.",
    "\U0001F680", "\U0001F49B", "❤️", "1️⃣", "⌚", "&", "&amp;", "&lt;", "&#39;",
    "&copy", "&notit;", "&#x1F600;", "&#128;", "<", ">", "<b>", "</b>", "<i>", "</i>", '<a href="https://e.com/x">',
    "</a>", "<pre>", "</pre>", "<code>", "</code>", "<span>", "[docs](https://x.com/_a_/b)", "[t](http://y.org)",
    "[bad](ftp://z)", "café", "日本語", "½", "²", "2*3*4", "snake_case", "\x00",
    ",", ".", "!", "?", ";", ":", "(", ")", "[", "]", "…", "́", "é",
]


def text(rng: random.Random, n: int) -> str:
    return "".join(rng.choice(TOKENS) + (" " if rng.random() < 0.4 else "") for _ in range(n))


def table(rng: random.Random) -> str:
    cols = rng.randint(1, 4)
    rows = ["| " + " | ".join(text(rng, rng.randint(0, 3)).replace("\n", " ").replace("\r", " ").replace("|", "")
                              for _ in range(rng.randint(1, cols + 1))) + " |" for _ in range(rng.randint(1, 5))]
    sep = "|" + "|".join(rng.choice(["---", ":--", "--:", ":-:", "-"]) for _ in range(cols)) + "|"
    return "\n".join([rows[0], sep] + rows[1:])


class Hang(Exception):
    """Python's _slice never ends when the reopened tags fill the whole cut window (a tiny limit and
    nested tags): such a case is left out, and the Rust side stops instead (PARITY.md, Differences)."""


_open_tags = fmt._open_tags
_calls = [0, 0]


def counted_open_tags(piece: str) -> list[tuple[str, str]]:
    _calls[0] += 1
    if _calls[0] > _calls[1]:
        raise Hang
    return _open_tags(piece)


fmt._open_tags = counted_open_tags


# Where engines and tables part: letters and digits Unicode 16 added (Python 3.13 knows Unicode 15.1),
# Python's other whitespace, the html.unescape corners, and tags and entities packed around a cut.
EDGE = [
    "\U00010D50", "\U00010D40", "\U00010D41", "\u1C89", "\u0B55", "\x85", "\u2028", "\u2029", "\x0b", "\x0c",
    "\x1f", "\u3000", "\u200b", "\u00a0", "&amp", "&ampx", "&notin", "&notit", "&#0;", "&#13;", "&#x80;", "&#x9F",
    "&#65534;", "&#xD800;", "&#99999999999;", "&#x110000;", "&#1;", "&#x7f;", "&AElig", "&lt", "&gt;x", "&#;", "&#x;",
    "&" + "a" * 40 + ";", '<a href="https://e.com/?a=1&amp;b=2">', "<b>", "</b>", "<i>", "</i>", "<s>", "</s>",
    "<u>", "</u>", "<pre>", "</pre>", "<code>", "</code>", "</a>", "<blockquote>", "<b\tx>", "<bx>", "< b>",
    "_x_", "*y*", "**z**", "~~w~~", "`c`", "1) ", "\U00010D40) ", "\n", "\n\n", " ", "word", "longerword",
]


def edge_text(rng: random.Random, n: int) -> str:
    pool = EDGE + TOKENS
    return "".join(rng.choice(pool) + (" " if rng.random() < 0.3 else "") for _ in range(n))


def adversarial(cases: list[dict]) -> None:
    rng = random.Random(20261020)
    for k in range(600):
        body = edge_text(rng, rng.randint(0, 60))
        cases.append({"fn": "plain", "in": body, "out": fmt.plain(body)})
        cases.append({"fn": "inline", "in": body, "out": fmt.inline(body)})
        cases.append({"fn": "render_body", "in": body, "out": fmt.render_body(body)})
        cases.append({"fn": "visible", "in": body, "out": fmt.visible(body)})
        title = rng.choice([None, edge_text(rng, rng.randint(1, 40))])
        subtitle = rng.choice([None, edge_text(rng, rng.randint(1, 60))])
        cases.append({"fn": "compose", "in": body, "title": title, "subtitle": subtitle,
                      "out": fmt.compose(body, title, subtitle)})
        raw = edge_text(rng, rng.randint(50, 400))         # raw HTML-ish text straight into split
        limit = rng.choice([20, 33, 50, 64, 65, 66, 100, 128, 129, 200, 500])
        _calls[:] = [0, 4 * len(raw) + 16]
        try:
            pieces = fmt.split(raw, limit)
        except Hang:
            continue
        cases.append({"fn": "split", "in": raw, "limit": limit, "out": pieces})


def main() -> None:
    rng = random.Random(20261019)
    cases: list[dict] = []
    for k in range(500):
        body = text(rng, rng.randint(0, 40))
        if k % 5 == 0:
            body = body + "\n" + table(rng) + "\n" + text(rng, rng.randint(0, 10))
        cases.append({"fn": "plain", "in": body, "out": fmt.plain(body)})
        cases.append({"fn": "inline", "in": body, "out": fmt.inline(body)})
        cases.append({"fn": "render_body", "in": body, "out": fmt.render_body(body)})
        cases.append({"fn": "visible", "in": body, "out": fmt.visible(body)})
        title = rng.choice([None, "", "  ", text(rng, rng.randint(1, 30))])
        subtitle = rng.choice([None, "", text(rng, rng.randint(1, 50))])
        composed = fmt.compose(body, title, subtitle)
        cases.append({"fn": "compose", "in": body, "title": title, "subtitle": subtitle, "out": composed})
        if k % 5 == 0:
            lines = table(rng).split("\n")
            cases.append({"fn": "table_blocks", "lines": lines, "out": fmt.table_blocks(lines)})
        long = composed if k % 3 else fmt.compose(text(rng, rng.randint(100, 450)), title, subtitle)
        limit = rng.choice([2, 3, 10, 40, 100, 150, 300, 4096])
        _calls[:] = [0, 4 * len(long) + 16]
        try:
            pieces = fmt.split(long, limit)
        except Hang:
            continue
        cases.append({"fn": "split", "in": long, "limit": limit, "out": pieces})
    adversarial(cases)
    (HERE / "differential.json").write_text(json.dumps(cases, ensure_ascii=True, indent=0) + "\n")
    print(len(cases), "cases")


if __name__ == "__main__":
    main()
