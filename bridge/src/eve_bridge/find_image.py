"""A real, high-quality picture of a place, venue, artist or thing, for eve's cards in the app (OpenUI ImageBlock).

Owner, 2026-10-13: "for openui/generativeui use images when u can, especially for like concerts, places and things
like that and a highquality one is available". A card may only show an image URL a tool returned (gen-prompt.mjs
RULES), and until now nothing returned one. Concerts get theirs from Ticketmaster (events.py, ``image``); this tool
covers the rest with Wikipedia's lead picture: one keyless call to the MediaWiki API (search, then the page's original
image), kept only when it is a photo-sized JPEG, PNG or WebP of at least MIN_WIDTH x MIN_HEIGHT.

``find_image(subject)`` → ``{"ok": true, "image", "width", "height", "page", "page_url"}``, or ``ok: false`` with a
plain reason (nothing found, only a small or drawn picture, a timeout). Never raises. ``EVE_FIND_IMAGE=0`` turns it off.
The page title is other people's words, so a result taints the turn as web_search's does (tool_gate).
"""

from __future__ import annotations

import logging
import os
import re
from typing import Any, Mapping, Optional

log = logging.getLogger(__name__)

SWITCH = "EVE_FIND_IMAGE"
TOOL_NAME = "find_image"
TOOL_NAMES = (TOOL_NAME,)
API = "https://en.wikipedia.org/w/api.php"
USER_AGENT = "eve-bridge find_image (personal assistant; https://github.com/gurul/eve_01)"
TIMEOUT_S = 6.0
MIN_WIDTH = 800
MIN_HEIGHT = 400
MAX_SUBJECT_CHARS = 120
OFF_REASON = "Pictures are off on this Mac (EVE_FIND_IMAGE=0)."
_PHOTO = re.compile(r"\.(jpe?g|png|webp)$", re.I)

TOOLS = [{
    "type": "function",
    "name": TOOL_NAME,
    "description": ("A real, high-quality picture of one place, venue, landmark, city, artist, band, team, dish, "
                    "product or thing, from Wikipedia, to show in a card in the app (ImageBlock, or a ListBlock "
                    "item's image). Returns its URL and size, or ok: false when no good picture exists; then the "
                    "card goes without one. Concerts and shows from events_search already carry their own image."),
    "parameters": {
        "type": "object",
        "properties": {"subject": {"type": "string",
                                   "description": "What to picture, by its proper name (\"Climate Pledge Arena\", "
                                                  "\"Pike Place Market\", \"Phoebe Bridgers\")."}},
        "required": ["subject"],
        "additionalProperties": False,
    },
}]


def enabled(environ: Optional[Mapping[str, str]] = None) -> bool:
    env = os.environ if environ is None else environ
    return (env.get(SWITCH) or "").strip().lower() not in ("0", "false", "no", "off")


def tools(environ: Optional[Mapping[str, str]] = None) -> list[dict[str, Any]]:
    return list(TOOLS) if enabled(environ) else []


def pick(payload: Any) -> Optional[dict[str, Any]]:
    """The MediaWiki answer → the first page's original picture when it is good enough, else None."""
    pages = ((payload or {}).get("query") or {}).get("pages") or []
    for page in sorted(pages, key=lambda p: p.get("index", 0)):
        orig = page.get("original") or {}
        src, w, h = str(orig.get("source") or ""), int(orig.get("width") or 0), int(orig.get("height") or 0)
        if not src.startswith("https://") or not _PHOTO.search(src.split("?")[0]):
            continue
        if w < MIN_WIDTH or h < MIN_HEIGHT:
            continue
        title = str(page.get("title") or "")
        return {"ok": True, "image": src, "width": w, "height": h, "page": title,
                "page_url": "https://en.wikipedia.org/wiki/" + title.replace(" ", "_")}
    return None


async def handle(name: str, args: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None,
                 http: Any = None) -> dict[str, Any]:
    if name != TOOL_NAME:
        return {"ok": False, "reason": f"{name} is not the picture tool"}
    if not enabled(environ):
        return {"ok": False, "reason": OFF_REASON}
    subject = " ".join(str(args.get("subject") or "").split())[:MAX_SUBJECT_CHARS]
    if len(subject) < 2:
        return {"ok": False, "reason": "subject must name what to picture"}
    params = {"action": "query", "format": "json", "formatversion": "2", "generator": "search",
              "gsrsearch": subject, "gsrlimit": "3", "prop": "pageimages", "piprop": "original"}
    try:
        if http is not None:
            payload = await http(API, params)
        else:
            import httpx

            async with httpx.AsyncClient(timeout=TIMEOUT_S, headers={"User-Agent": USER_AGENT}) as c:
                r = await c.get(API, params=params)
                r.raise_for_status()
                payload = r.json()
    except Exception as e:  # noqa: BLE001 — a picture is optional: say so and the card goes without
        log.info("find_image: no answer (%s)", type(e).__name__)
        return {"ok": False, "reason": "Wikipedia did not answer; leave the picture out"}
    found = pick(payload)
    log.info("find_image: %s", "found one" if found else "none good enough")
    return found or {"ok": False, "reason": "no high-quality picture of that; leave the picture out"}
