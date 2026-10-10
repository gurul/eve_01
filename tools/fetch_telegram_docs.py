#!/usr/bin/env python3
"""Save Telegram's official bot documentation as local Markdown, so it can be read and grepped offline.

    python3 tools/fetch_telegram_docs.py            # writes docs/reference/telegram/*.md

Standard library only. Each page's main content (``#dev_page_content``) becomes Markdown: headings,
paragraphs, lists, tables as pipe rows, code as fences, links kept absolute, sample bot tokens redacted. Rerun it to refresh;
every file starts with its source URL and the date it was fetched.
"""

from __future__ import annotations

import html
import re
import sys
import urllib.request
from datetime import date
from html.parser import HTMLParser
from pathlib import Path

ROOT = "https://core.telegram.org"
PAGES = {
    "bot-api": "/bots/api",
    "bot-api-changelog": "/bots/api-changelog",
    "features": "/bots/features",
    "webapps": "/bots/webapps",
    "inline": "/bots/inline",
    "payments": "/bots/payments",
    "payments-stars": "/bots/payments-stars",
    "games": "/bots/games",
    "faq": "/bots/faq",
    "tutorial": "/bots/tutorial",
}
OUT = Path(__file__).resolve().parent.parent / "docs" / "reference" / "telegram"
# Telegram's pages print sample bot tokens ("123456789:AA…"). They are fake, but secret scanners cannot
# tell, so every token-shaped string is replaced before a page is saved.
BOT_TOKEN = re.compile(r"(?<!\d)\d{6,12}:[A-Za-z0-9_-]{30,}")      # also inside a URL: /bot123456:…
REDACTED = "<bot-token>"


class ToMarkdown(HTMLParser):
    """Only what sits inside the page-content div; everything else (menus, footers) is dropped."""

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.depth = 0            # div nesting inside the content div; 0 = outside
        self.out: list[str] = []
        self.href: list[str] = []
        self.pre = False
        self.lists: list[str] = []
        self.cell = False
        self.in_anchor = False    # the ¶ link beside a heading: its icon is not text

    def _emit(self, text: str) -> None:
        if self.depth and not self.in_anchor:
            self.out.append(text)

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        a = dict(attrs)
        if tag == "div":
            if self.depth:
                self.depth += 1
            elif a.get("id") == "dev_page_content":
                self.depth = 1
            return
        if not self.depth:
            return
        if tag in ("h1", "h2", "h3", "h4", "h5", "h6"):
            self._emit("\n\n" + "#" * int(tag[1]) + " ")
        elif tag == "p":
            self._emit("\n\n")
        elif tag == "br":
            self._emit("\n")
        elif tag in ("ul", "ol"):
            self.lists.append(tag)
            self._emit("\n")
        elif tag == "li":
            self._emit("\n" + "  " * (len(self.lists) - 1) + ("1. " if self.lists[-1:] == ["ol"] else "- "))
        elif tag == "pre":
            self.pre = True
            self._emit("\n\n```\n")
        elif tag == "code" and not self.pre:
            self._emit("`")
        elif tag in ("strong", "b"):
            self._emit("**")
        elif tag in ("em", "i"):
            self._emit("*")
        elif tag == "a" and "anchor" in (a.get("class") or ""):
            self.in_anchor = True
            self.href.append("")
        elif tag == "a":
            href = a.get("href") or ""
            if href.startswith("/"):
                href = ROOT + href
            self.href.append(href)
            if href and not href.startswith("#") and not (a.get("class") or "").startswith("anchor"):
                self._emit("[")
            else:
                self.href[-1] = ""
        elif tag == "tr":
            self._emit("\n|")
        elif tag in ("td", "th"):
            self.cell = True
            self._emit(" ")
        elif tag == "blockquote":
            self._emit("\n\n> ")
        elif tag == "img":
            alt = a.get("alt") or ""
            if alt:
                self._emit(alt)

    def handle_endtag(self, tag: str) -> None:
        if tag == "div":
            if self.depth:
                self.depth -= 1
            return
        if not self.depth:
            return
        if tag in ("ul", "ol") and self.lists:
            self.lists.pop()
            self._emit("\n")
        elif tag == "pre":
            self.pre = False
            self._emit("\n```\n")
        elif tag == "code" and not self.pre:
            self._emit("`")
        elif tag in ("strong", "b"):
            self._emit("**")
        elif tag in ("em", "i"):
            self._emit("*")
        elif tag == "a" and self.href:
            self.in_anchor = False
            href = self.href.pop()
            if href:
                self._emit(f"]({href})")
        elif tag in ("td", "th"):
            self.cell = False
            self._emit(" |")
        elif tag == "thead":
            self._emit("\n|---|---|---|---|")

    def handle_data(self, data: str) -> None:
        if not self.depth:
            return
        if self.pre:
            self._emit(data)
        else:
            self._emit(re.sub(r"\s+", " ", data.replace("|", "\\|") if self.cell else data))

    def markdown(self) -> str:
        text = "".join(self.out)
        text = re.sub(r"[ \t]+\n", "\n", text)
        text = re.sub(r"\n{3,}", "\n\n", text)
        return html.unescape(text).strip() + "\n"


def fetch(path: str) -> str:
    req = urllib.request.Request(ROOT + path, headers={"User-Agent": "eve-docs-fetch/1"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read().decode("utf-8", "replace")


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    today = date.today().isoformat()
    failed = []
    for name, path in PAGES.items():
        try:
            parser = ToMarkdown()
            parser.feed(fetch(path))
            body = BOT_TOKEN.sub(REDACTED, parser.markdown())
        except Exception as e:  # noqa: BLE001 — one page failing leaves the others
            failed.append(f"{name}: {type(e).__name__}: {e}")
            continue
        if len(body) < 500:
            failed.append(f"{name}: page content not found ({len(body)} chars)")
            continue
        (OUT / f"{name}.md").write_text(f"<!-- source: {ROOT}{path} · fetched {today} -->\n\n{body}")
        print(f"{name}.md  {len(body):>8} chars")
    for line in failed:
        print("FAILED", line, file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
