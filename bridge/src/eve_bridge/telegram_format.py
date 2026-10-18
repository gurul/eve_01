"""One shape for everything eve sends to Telegram.

Before this module a message was whatever string reached ``BotApi.send_message``: a model's
answer with its markdown intact (``**bold**`` shown as stars, ``## Heading`` as hashes), a relayed
Claude Code message flattened onto one line, a task result as one long paragraph. Read on a phone,
that is a wall of text. Now every message is composed here, the same way:

* a short **bold title** when the message is not eve's own voice (a task's result or question,
  Claude Code speaking through the relay, a permission to grant), one blank line, then the body;
* the body in short paragraphs: a blank line between paragraphs, ``•`` bullets, headings in bold,
  code in ``<code>`` / ``<pre>`` — markdown from a model turned into Telegram HTML, never shown raw;
* a markdown table as one block per row (Telegram has no tables, and a pipe grid in a proportional
  font is a wall of bars on a phone): the row's first cell in bold, then ``header: value`` lines;
* Telegram's HTML parse mode, which needs only ``&``, ``<`` and ``>`` escaped in content (MarkdownV2
  needs eighteen characters escaped, and a missed one is a 400 and a lost message);
* split at 4096 characters on a paragraph boundary, then a line, then a space, with the open tags
  closed at the cut and reopened after it, so no piece is rejected by Telegram;
* no emoji, no em or en dashes (owner, 2026-09-21), stripped in code from every piece of prose.

Pure: nothing here touches the network. ``compose`` builds one message, ``split`` cuts it,
``visible`` is the plain text a piece shows (the fallback when Telegram will not parse it).
"""

from __future__ import annotations

import html as html_mod
import re
from typing import Optional

MAX_MESSAGE_CHARS = 4096            # Bot API limit on sendMessage text, HTML tags included
PARSE_MODE = "HTML"
MAX_TITLE_CHARS = 80
MAX_SUBTITLE_CHARS = 100
_TAG_HEADROOM = 64                  # room kept at a cut for the closing tags a balanced piece needs

# No emoji, ever (owner, 2026-09-21). Stripped in code from every outgoing message, prompt or not: the
# pictographic blocks, the variation selectors and joiners that build them, and the keycap combiner.
_EMOJI = re.compile("[\U0001F000-\U0001FAFF\U00002600-\U000027BF\U00002B00-\U00002BFF\U0001F900-\U0001F9FF"
                    "\U0000FE0F\U0000200D\U000020E3\U0001F1E6-\U0001F1FF\U0000231A-\U0000231B\U000023E9-\U000023FA"
                    "\U000025AA-\U000025FE\U00002934-\U00002935\U00003030\U0000303D\U00003297\U00003299]")

_DASH = re.compile(r"\s*[—–]\s*")      # em dash, en dash (owner, 2026-09-21): a comma or a full stop instead


def plain(text: str) -> str:
    """The text without emoji or dashes, and without the doubled spaces they leave behind. A dash between
    words becomes a comma; one that ends a sentence-like run becomes a full stop."""
    out = _EMOJI.sub("", text)
    out = _DASH.sub(", ", out)
    out = re.sub(r", (?=[,.!?]|$)", "", out)            # a dash right before punctuation just goes
    return re.sub(r"[ \t]{2,}", " ", out).strip() if out != text else text


def escape(text: str) -> str:
    """The three characters Telegram's HTML mode reserves. Everything else travels as it is."""
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


# ---- markdown-ish text → Telegram HTML ---------------------------------------------------------

_FENCE = re.compile(r"^\s*(```|~~~)")
_HEADING = re.compile(r"^#{1,6}\s+(.*?)\s*#*$")
_NUMBERED = re.compile(r"^(\d{1,3})[.)]\s+(.*)$")
_BULLET = re.compile(r"^(?:[-*+•◦])\s+(.*)$")
_RULE = re.compile(r"^(?:[-*_]\s*){3,}$")
_QUOTE = re.compile(r"^>\s?(.*)$")
_CODE_SPAN = re.compile(r"(`[^`\n]+`)")
_LINK = re.compile(r"\[([^\]\n]+)\]\((https?://[^)\s]+)\)")
_BOLD = re.compile(r"\*\*(?=\S)(.+?)(?<=\S)\*\*|__(?=\S)(.+?)(?<=\S)__")
_ITALIC_STAR = re.compile(r"(?<![\w*])\*(?=\S)([^*\n]+?)(?<=\S)\*(?![\w*])")
_ITALIC_UNDER = re.compile(r"(?<!\w)_(?=\S)([^_\n]+?)(?<=\S)_(?!\w)")
_STRIKE = re.compile(r"~~(?=\S)(.+?)(?<=\S)~~")
_TABLE_SEP = re.compile(r"^\|?\s*:?-+:?\s*(?:\|\s*:?-+:?\s*)*\|?$")   # |---|:---:|, the line under a header


def _styles(escaped: str) -> str:
    """Inline markdown on already-escaped prose: links, bold, italic, strikethrough. A link's URL is held out
    while the styles run and put back after: run over it, "_foo_" in a path became <i> inside the href."""
    urls: list[str] = []

    def hold(m: re.Match[str]) -> str:
        urls.append(m.group(2))
        return f'<a href="\x00{len(urls) - 1}\x00">{m.group(1)}</a>'

    out = _LINK.sub(hold, escaped.replace("\x00", ""))        # a NUL of the text's own is no placeholder
    out = _BOLD.sub(lambda m: f"<b>{m.group(1) or m.group(2)}</b>", out)
    out = _STRIKE.sub(r"<s>\1</s>", out)
    out = _ITALIC_STAR.sub(r"<i>\1</i>", out)
    out = _ITALIC_UNDER.sub(r"<i>\1</i>", out)
    return re.sub(r"\x00(\d+)\x00", lambda m: urls[int(m.group(1))], out) if urls else out


def inline(text: str) -> str:
    """One line of prose as HTML: escaped, with `code` spans kept verbatim and the styles applied
    around them."""
    pieces: list[str] = []
    for part in _CODE_SPAN.split(escape(text)):
        if len(part) >= 3 and part.startswith("`") and part.endswith("`"):
            pieces.append("<code>" + part[1:-1] + "</code>")
        elif part:
            pieces.append(_styles(part))
    return "".join(pieces)


def _cells(row: str) -> list[str]:
    """The cells of one ``| a | b |`` line, outer pipes dropped, each cell stripped."""
    body = row.strip()
    body = body[1:] if body.startswith("|") else body
    body = body[:-1] if body.endswith("|") else body
    return [c.strip() for c in body.split("|")]


def _lead(cell: str) -> str:
    """A row's first cell, bold. Its own ``**stars**`` come off first: bold inside bold is two tags."""
    return "<b>" + inline(_BOLD.sub(lambda m: m.group(1) or m.group(2), cell)) + "</b>"


def table_blocks(lines: list[str]) -> list[str]:
    """A markdown table (header, separator, rows) as HTML paragraphs a phone can read. Telegram has no
    tables, and a pipe grid in a proportional font lines nothing up. Two columns or fewer: one
    paragraph, a line per row, ``<b>first cell</b>: second``. Wider: a paragraph per row, the first
    cell in bold, then a ``header: value`` line for every filled cell after it (an empty cell is no
    line). Cells are prose: emoji and dashes go, inline styles and links stay."""
    header = [plain(c) for c in _cells(lines[0])]
    rows = [[plain(c) for c in _cells(line)] for line in lines[2:]]
    rows = [r for r in rows if any(r)]
    width = max([len(header)] + [len(r) for r in rows])
    header += [""] * (width - len(header))
    if width <= 2:
        out: list[str] = []
        for r in rows:
            r += [""] * (width - len(r))
            first = _lead(r[0]) if r[0] else ""
            second = inline(r[1]) if width == 2 and r[1] else ""
            out.append(first + ": " + second if first and second else first or second)
        return ["\n".join(out)] if out else []
    blocks: list[str] = []
    for r in rows:
        r += [""] * (width - len(r))
        block = [_lead(r[0])] if r[0] else []
        block += [(inline(h) + ": " if h else "") + inline(v) for h, v in zip(header[1:], r[1:], strict=True) if v]
        if block:
            blocks.append("\n".join(block))
    return blocks


def render_body(text: str) -> str:
    """Free text (a model's answer, a task's result, what Claude Code said) as Telegram HTML paragraphs.

    Line structure is kept: a blank line separates paragraphs, and lines inside a paragraph stay on their
    own lines. Fenced code becomes ``<pre>``; ``#`` headings become bold lines; ``-``, ``*`` and numbered
    lists become ``•`` and ``1.`` lines; ``> quotes`` become italic; a horizontal rule goes; a ``|`` table
    becomes ``table_blocks``. Emoji and dashes are stripped from prose (``plain``), never from code."""
    paragraphs: list[str] = []
    current: list[str] = []
    fence: Optional[list[str]] = None

    def flush() -> None:
        if current:
            paragraphs.append("\n".join(current))
            current.clear()

    lines = str(text).replace("\r\n", "\n").replace("\r", "\n").split("\n")
    i = 0
    while i < len(lines):
        raw = lines[i]
        i += 1
        if _FENCE.match(raw):
            if fence is None:
                flush()
                fence = []
            else:
                code = "\n".join(fence).strip("\n")
                if code.strip():
                    paragraphs.append("<pre>" + escape(_EMOJI.sub("", code)) + "</pre>")
                fence = None
            continue
        if fence is not None:
            fence.append(raw.rstrip())
            continue
        if raw.strip().startswith("|") and i < len(lines) and _TABLE_SEP.match(lines[i].strip()):
            flush()                                    # a header row over its separator: a table until a line
            end = i + 1                                # that does not start with a pipe
            while end < len(lines) and lines[end].strip().startswith("|"):
                end += 1
            paragraphs.extend(table_blocks(lines[i - 1:end]))
            i = end
            continue
        line = plain(raw.strip())
        if not line:
            flush()
            continue
        if _RULE.match(line):
            flush()
            continue
        if (m := _HEADING.match(line)) is not None:
            current.append("<b>" + inline(m.group(1)) + "</b>")
        elif (m := _NUMBERED.match(line)) is not None:
            current.append(f"{m.group(1)}. " + inline(m.group(2)))
        elif (m := _BULLET.match(line)) is not None:
            current.append("• " + inline(m.group(1)))
        elif (m := _QUOTE.match(line)) is not None:
            if m.group(1).strip():
                current.append("<i>" + inline(m.group(1).strip()) + "</i>")
        else:
            current.append(inline(line))
    if fence is not None:                          # an unclosed fence: still code, still shown
        code = "\n".join(fence).strip("\n")
        if code.strip():
            paragraphs.append("<pre>" + escape(_EMOJI.sub("", code)) + "</pre>")
    flush()
    return "\n\n".join(paragraphs)


def _clip(text: str, limit: int) -> str:
    text = plain(" ".join(str(text).split()))
    return text if len(text) <= limit else text[:limit - 1].rstrip() + "…"


def compose(body: str, title: Optional[str] = None, subtitle: Optional[str] = None) -> str:
    """One message: a bold title (when the message is not eve's own voice), an italic subtitle under it
    when an identifier helps (the task's goal, the repo Claude speaks from), a blank line, the body."""
    head: list[str] = []
    if title and title.strip():
        head.append("<b>" + escape(_clip(title, MAX_TITLE_CHARS)) + "</b>")
    if subtitle and subtitle.strip():
        head.append("<i>" + escape(_clip(subtitle, MAX_SUBTITLE_CHARS)) + "</i>")
    parts: list[str] = []
    if head:
        parts.append("\n".join(head))
    rendered = render_body(body)
    if rendered:
        parts.append(rendered)
    return "\n\n".join(parts)


# ---- splitting at the Bot API limit, tags kept balanced ------------------------------------------

_TAG = re.compile(r"<(/?)(b|i|s|u|code|pre|a)(\s[^<>]*)?>")


def _open_tags(piece: str) -> list[tuple[str, str]]:
    """The tags still open at the end of ``piece``, outermost first, with their attributes."""
    stack: list[tuple[str, str]] = []
    for m in _TAG.finditer(piece):
        closing, name, attrs = m.group(1) == "/", m.group(2), m.group(3) or ""
        if closing:
            if stack and stack[-1][0] == name:
                stack.pop()
        else:
            stack.append((name, attrs))
    return stack


def _safe_cut(text: str, at: int) -> int:
    """Move a cut position left, out of the middle of a tag or an entity."""
    lt = text.rfind("<", 0, at)
    if lt != -1 and text.find(">", lt, at) == -1:
        at = lt
    amp = text.rfind("&", max(0, at - 10), at)
    if amp != -1 and text.find(";", amp, at) == -1:
        at = amp
    return at


def _slice(paragraph: str, limit: int) -> list[str]:
    """One paragraph longer than ``limit``: cut at a line, then a space, then anywhere, with the open
    tags closed at the cut and reopened after it."""
    out: list[str] = []
    rest = paragraph
    window = max(limit - _TAG_HEADROOM, limit // 2)
    while len(rest) > limit:
        cut = rest.rfind("\n", 0, window)             # a line end in the second half of the window first,
        if cut < window // 2:
            cut = rest.rfind(" ", 0, window)          # then a space there, else the window as it is
        cut = cut + 1 if cut >= window // 2 else window
        cut = _safe_cut(rest, cut)
        if cut <= 0:
            cut = window
        piece, rest = rest[:cut], rest[cut:]
        open_tags = _open_tags(piece)
        piece = piece.rstrip() + "".join(f"</{name}>" for name, _ in reversed(open_tags))
        rest = "".join(f"<{name}{attrs}>" for name, attrs in open_tags) + rest.lstrip(" ")
        out.append(piece)
    if rest:
        out.append(rest)
    return out


def split(text: str, limit: int = MAX_MESSAGE_CHARS) -> list[str]:
    """The pieces to send, each at most ``limit`` characters, cut on a paragraph boundary when one is
    there. An empty message is no pieces."""
    if not text.strip():
        return []
    if len(text) <= limit:
        return [text]
    pieces: list[str] = []
    current = ""
    for paragraph in text.split("\n\n"):
        if not paragraph.strip():
            continue
        if len(paragraph) > limit:
            # Its slices go out one per piece; the first may still follow what is waiting (a title), and
            # the last may still be joined by the paragraph after it.
            slices = _slice(paragraph, limit)
            joined = slices[0] if not current else current + "\n\n" + slices[0]
            if len(joined) <= limit:
                pieces.append(joined)
            else:
                pieces.extend(p for p in (current, slices[0]) if p)
            pieces.extend(slices[1:-1])
            current = slices[-1]
            continue
        joined = paragraph if not current else current + "\n\n" + paragraph
        if len(joined) <= limit:
            current = joined
        else:
            pieces.append(current)
            current = paragraph
    if current:
        pieces.append(current)
    return pieces


def visible(piece: str) -> str:
    """The text a piece shows once Telegram has rendered it: tags gone, entities back to characters.
    What is sent, without a parse mode, when Telegram refuses the HTML."""
    return html_mod.unescape(_TAG.sub("", piece))
