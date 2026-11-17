#!/usr/bin/env python3
"""A stand-in for eve's link door, for looking at the chat in the simulator (docs/app-chat.md, "The design hook").

    python3 ios/EveLink/tools/fake_link.py [port] [lines]     # 127.0.0.1:8765, the whole conversation
    SIMCTL_CHILD_EVE_PAIR_URL='evelink://pair?u=http://127.0.0.1:8765&t=design-design-design-design-design' \\
      xcrun simctl launch booted com.github.eve-bridge.EveLink -eveScreen chat

It answers the chat's requests with a made-up conversation: prose, every example card from the daemon's own
reference (bridge/src/eve_bridge/json_render_prompt.txt), a picture, a file, sources, a reaction, a card in the older
format, an older page for infinite scroll, topics, and a turn that streams a reply with a card a few characters at a
time. Nothing here is real, nothing is stored, and it listens on this Mac only. Any token is accepted.

A sent message picks the reply: "card" streams a card, "photo" sends a picture mid-turn, "slow" waits before the
first word, "fail" refuses the turn, and anything else gets a short plain answer.
"""

from __future__ import annotations

import base64
import json
import re
import struct
import sys
import threading
import time
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, urlparse

REFERENCE = Path(__file__).resolve().parents[3] / "bridge" / "src" / "eve_bridge" / "json_render_prompt.txt"
DAY, OLD_DAY = "2026-01-05", "2026-01-04"
ID1, ID2 = "0123456789abcdef0123456789abcdef", "fedcba9876543210fedcba9876543210"


def png(width: int, height: int, top: tuple[int, int, int], bottom: tuple[int, int, int]) -> bytes:
    """A small gradient picture, made here so no image file is needed."""
    rows = b""
    for y in range(height):
        f = y / max(1, height - 1)
        rows += b"\x00" + bytes(int(a + (b - a) * f) for a, b in zip(top, bottom)) * width

    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    head = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", head) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")


PICTURE = png(320, 220, (255, 170, 90), (70, 60, 140))
THUMB = "data:image/png;base64," + base64.b64encode(png(96, 66, (255, 170, 90), (70, 60, 140))).decode()


def examples() -> list[str]:
    """Every example block in the daemon's reference, as the fenced text a reply holds."""
    text = REFERENCE.read_text(encoding="utf-8") if REFERENCE.exists() else ""
    return re.findall(r"```json-render\n.*?\n```", text, re.DOTALL)


CARDS = examples()
LEGACY = '```openui-lang\nroot = Card([t])\nt = TextContent("An older card")\n```'
MARKDOWN = """Here is **everything markdown** in one reply, with a [link](https://example.com/a) and `inline code`.

## A heading

- A bullet with *emphasis*
- Another, with a nested list:
  1. First
  2. Second

> A quote, for good measure.

| Train | Time | Cost |
|:--|--:|--:|
| Morning | 4 h 10 min | $89 |
| Evening | 4 h 25 min | $64 |

```swift
let answer = 6 * 7
print(answer)
```

That is all of it."""


class Chat:
    """The made-up conversation, and the lines added while this runs."""

    def __init__(self) -> None:
        self.lock = threading.Condition()
        self.old: list[dict[str, Any]] = []
        self.rows: list[dict[str, Any]] = []
        self.reactions: dict[str, str] = {"turn-0001": "🏆"}
        self.bookmarked: set[str] = set()
        self.busy = False
        now = time.time()
        for i in range(60):
            who = "owner" if i % 2 == 0 else "eve"
            words = f"An older question, number {i // 2 + 1}?" if who == "owner" else f"An older answer, number {i // 2 + 1}."
            self.old.append(self.row(OLD_DAY, i + 1, who, words, now - 90000 + i * 30))
        say = lambda who, text, **extra: self.rows.append(  # noqa: E731
            self.row(DAY, len(self.rows) + 1, who, text, now - 4000 + len(self.rows) * 150, **extra))
        say("owner", "Show me what a reply can hold.", rid="turn-0001")
        say("eve", MARKDOWN)
        for n, card in enumerate(CARDS):
            say("owner", f"And card number {n + 1}?")
            say("eve", f"Here it is.\n\n{card}\n\nAnything else?")
        say("owner", "Take a photo of the desk.", attachments=[
            {"id": ID2, "name": "notes.pdf", "kind": "pdf"}])
        say("eve", "", kind="media", attachments=[{"id": ID1, "name": "desk.png", "kind": "image", "thumb": THUMB}])
        say("eve", "That is the desk. You like the lamp on the left.", sources=[
            {"kind": "fact", "text": "Likes the lamp on the left side of the desk.", "when": "Jan 2"},
            {"kind": "said", "text": "You: move the lamp back to the left.", "when": "Mon 9:00"}])
        say("eve", f"An older card:\n\n{LEGACY}")

    @staticmethod
    def row(day: str, index: int, role: str, text: str, t: float, **extra: Any) -> dict[str, Any]:
        return {"id": f"{day}:{index}", "role": role, "text": text, "t": t, "via": "app", "kind": "say", **extra}

    def add(self, role: str, text: str, **extra: Any) -> None:
        with self.lock:
            self.rows.append(self.row(DAY, len(self.rows) + 1, role, text, time.time(), **extra))
            self.lock.notify_all()

    def cursor(self) -> str:
        return f"c{len(self.rows)}"

    def after(self, cursor: str, wait: float) -> dict[str, Any]:
        seen = int(cursor[1:]) if re.fullmatch(r"c\d+", cursor) else len(self.rows)
        with self.lock:
            if len(self.rows) <= seen and wait > 0:
                self.lock.wait(timeout=wait)
            return {"ok": True, "messages": self.rows[seen:], "cursor": self.cursor(), "busy": self.busy,
                    "reactions": dict(self.reactions)}

    def topics(self) -> dict[str, Any]:
        def topic(first: str, last: str, title: str, preview: str, count: int, ago: float) -> dict[str, Any]:
            return {"id": first, "first_id": first, "last_id": last, "started_at": time.time() - ago - 600,
                    "ended_at": time.time() - ago, "count": count, "title": title, "preview": preview,
                    "bookmarked": first in self.bookmarked}

        topics = [topic(f"{DAY}:1", f"{DAY}:{len(self.rows)}", "What a reply can hold", "Cards, a photo, sources.",
                        len(self.rows), 60),
                  topic(f"{OLD_DAY}:1", f"{OLD_DAY}:60", "Older questions", "An older answer, number 30.", 60, 90000),
                  topic("2025-12-20:1", "2025-12-20:9", "Planning the spring trip", "Two routes compared.", 9, 1.4e6)]
        return {"ok": True, "topics": topics, "bookmarked": [t for t in topics if t["bookmarked"]]}


CHAT = Chat()


class Door(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt: str, *args: Any) -> None:
        sys.stderr.write("fake link: " + fmt % args + "\n")

    def answer(self, status: int, body: Any, kind: str = "application/json") -> None:
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:  # noqa: N802
        url = urlparse(self.path)
        query = {k: v[0] for k, v in parse_qs(url.query).items()}
        if url.path == "/api/link/messages":
            if "before" in query:
                first_new = query["before"].startswith(DAY)
                self.answer(200, {"ok": True, "messages": CHAT.old if first_new else [], "more": False,
                                  "cursor": "", "busy": False})
            elif "after" in query:
                self.answer(200, CHAT.after(query["after"], min(25.0, float(query.get("wait", "0") or 0))))
            else:
                self.answer(200, {"ok": True, "messages": CHAT.rows, "cursor": CHAT.cursor(), "busy": CHAT.busy,
                                  "reactions": dict(CHAT.reactions)})
        elif url.path == "/api/link/topics":
            self.answer(200, CHAT.topics())
        elif url.path == f"/api/link/file/{ID1}":
            self.answer(200, PICTURE, "image/png")
        elif url.path == "/api/link/muse":
            self.answer(200, {"ok": True, "available": False, "on": False})
        else:
            self.answer(404, {"ok": False, "error": "not in the stand-in"})

    def do_POST(self) -> None:  # noqa: N802
        url = urlparse(self.path)
        raw = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        try:
            body = json.loads(raw or b"{}")
        except ValueError:
            body = {}
        if url.path == "/api/link/turn":
            self.turn(body if isinstance(body, dict) else {})
        elif url.path == "/api/link/topics/bookmark":
            topic, on = str(body.get("topic")), body.get("on") is True
            (CHAT.bookmarked.add if on else CHAT.bookmarked.discard)(topic)
            self.answer(200, {"ok": True})
        else:
            self.answer(404, {"ok": False, "error": "not in the stand-in"})

    def turn(self, body: dict[str, Any]) -> None:
        text, rid = str(body.get("text") or ""), str(body.get("client_id") or "")
        low = text.lower()
        if "fail" in low:
            self.answer(503, {"ok": False, "error": "eve's chat isn't running"})
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        CHAT.busy = True
        CHAT.add("owner", text, rid=rid)
        if rid:
            CHAT.reactions[rid] = "👀"
        if "card" in low and CARDS:
            reply = f"Here is one.\n\n{CARDS[len(CHAT.rows) % len(CARDS)]}\n\nTap a choice, or ask for another."
        elif "photo" in low:
            reply = "Here it is."
            CHAT.add("eve", "", kind="media", attachments=[{"id": ID1, "name": "desk.png", "kind": "image", "thumb": THUMB}])
        else:
            reply = f"You said: {text.splitlines()[0][:200]}. This is the stand-in, so that is all I know."
        try:
            self.wfile.write(b": ping\n\n")
            self.wfile.flush()
            time.sleep(4 if "slow" in low else 0.6)
            for i in range(0, len(reply), 12):
                event = {"choices": [{"index": 0, "delta": {"content": reply[i:i + 12]}}]}
                self.wfile.write(f"data: {json.dumps(event)}\n\n".encode())
                self.wfile.flush()
                time.sleep(0.05)
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except OSError:
            pass                                    # the app stopped reading (Stop, or the background)
        finally:
            CHAT.busy = False
            CHAT.add("eve", reply)


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
    if len(sys.argv) > 2:                           # show only the first N lines, to look at a card further up
        del CHAT.rows[int(sys.argv[2]):]
    print(f"fake link: http://127.0.0.1:{port} with {len(CARDS)} example cards", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), Door).serve_forever()
