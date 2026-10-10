#!/usr/bin/env python3
"""Synthetic app states for the visual judge's evals: a JSON scene → (Snapshot, screenshot in screen points).

Why synthetic: the visual judge (docs/evals/visual-judge-2026-10/BAR.md) is scored offline on frozen pairs of an AX
snapshot and a screenshot. Real captures need the owner to drive his apps (tools/visual_judge_capture.py); a scene
file needs nobody, holds no personal data, and renders the same pixels every time, so its sha256 can be frozen.
What a scene cannot do is look like a real app: its icons are drawn here from lines and circles. So a synthetic set
measures the judge's mechanics (does a description of an icon-only control reach the decision, does risk stop it)
and never ships it on its own (BAR.md says so).

Scene (one app state):

    {"id": "s01", "app": "Ledger", "bundle": "com.example.ledger", "title": "Ledger — March",
     "screen": [1440, 900], "theme": "light" | "dark", "window": [x, y, w, h],
     "panes": [{"rect": [x, y, w, h], "fill": "sidebar" | "toolbar" | "content" | "#rrggbb"}],
     "texts": [{"id": "t1", "text": "March 2026", "at": [x, y], "size": 18}],
     "controls": [{"id": "7", "role": "button", "label": "toolbar item 3", "rect": [x, y, w, h],
                   "draw": {"glyph": "trash"} | {"text": "Week"} | {"glyph": "plus", "text": "New"},
                   "style": "plain" | "bordered" | "segment" | "row" | "checkbox" | "link",
                   "value": "" | "selected" | "checked" | "unchecked", "enabled": true}]}

``label`` is what Accessibility reports (it may be unhelpful: an SF Symbol name, "button", "item 4"); ``draw`` is
what a person sees. Static texts become ``static text`` elements and context lines, as the real walk makes them.

    .venv/bin/python tools/visual_judge_scenes.py --render FILE.json --out DIR   # PNG per scene, for a look
    .venv/bin/python tools/visual_judge_scenes.py --scope FILE.json              # which cases the judge would see
    .venv/bin/python tools/visual_judge_scenes.py --glyphs --out DIR             # a sheet of every glyph

``--scope`` runs the code that runs before the judge (the keyword lane, the request gates, the menu) on every case
and says which ones would reach the judge at all; it never calls a model.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import math
import sys
from pathlib import Path
from typing import Any, Callable

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "src"))

from PIL import Image, ImageDraw  # noqa: E402

from eve_bridge.ax_candidates import Candidate, Snapshot  # noqa: E402

THEMES = {
    "light": {"bg": "#e8e8ea", "window": "#ffffff", "sidebar": "#f2f1f4", "toolbar": "#f7f7f8", "content": "#ffffff",
              "ink": "#1d1d1f", "dim": "#6e6e73", "accent": "#0a64d8", "line": "#d2d2d7", "button": "#ececee",
              "selected": "#0a64d8", "selected_ink": "#ffffff", "row_sel": "#d6e4fb"},
    "dark": {"bg": "#2b2b2e", "window": "#1e1e20", "sidebar": "#29292c", "toolbar": "#2f2f33", "content": "#1e1e20",
             "ink": "#f2f2f7", "dim": "#a1a1a6", "accent": "#3a8dff", "line": "#3d3d42", "button": "#3a3a3e",
             "selected": "#3a8dff", "selected_ink": "#ffffff", "row_sel": "#21406e"},
}
STYLES = ("plain", "bordered", "segment", "row", "checkbox", "link")
ROLES = ("button", "radio button", "tab", "checkbox", "row", "cell", "menu button", "pop up button", "link",
         "disclosure triangle", "toggle")


# ---- glyphs: each draws into a square box with one ink colour -------------------------------------------------

def _box(b: tuple[float, float, float, float], inset: float = 0.0) -> tuple[float, float, float, float]:
    x0, y0, x1, y1 = b
    return (x0 + inset, y0 + inset, x1 - inset, y1 - inset)


def _w(b: tuple[float, float, float, float]) -> int:
    return max(1, round((b[2] - b[0]) / 11))


def _arrow_head(d: ImageDraw.ImageDraw, tip: tuple[float, float], angle: float, size: float, ink: str) -> None:
    left = (tip[0] - size * math.cos(angle - 0.5), tip[1] - size * math.sin(angle - 0.5))
    right = (tip[0] - size * math.cos(angle + 0.5), tip[1] - size * math.sin(angle + 0.5))
    d.polygon([tip, left, right], fill=ink)


def g_plus(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.2)
    cx, cy, w = (x0 + x1) / 2, (y0 + y1) / 2, _w(b) + 1
    d.line((cx, y0, cx, y1), fill=ink, width=w)
    d.line((x0, cy, x1, cy), fill=ink, width=w)


def g_minus(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.2)
    d.line((x0, (y0 + y1) / 2, x1, (y0 + y1) / 2), fill=ink, width=_w(b) + 1)


def g_close(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.25)
    d.line((x0, y0, x1, y1), fill=ink, width=_w(b) + 1)
    d.line((x0, y1, x1, y0), fill=ink, width=_w(b) + 1)


def g_trash(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.18)
    w, s = _w(b), x1 - x0
    d.line((x0, y0 + s * 0.18, x1, y0 + s * 0.18), fill=ink, width=w)
    d.rectangle((x0 + s * 0.35, y0, x1 - s * 0.35, y0 + s * 0.18), outline=ink, width=w)
    d.polygon([(x0 + s * 0.1, y0 + s * 0.25), (x1 - s * 0.1, y0 + s * 0.25), (x1 - s * 0.18, y1), (x0 + s * 0.18, y1)],
              outline=ink, width=w)
    for f in (0.38, 0.5, 0.62):
        d.line((x0 + s * f, y0 + s * 0.38, x0 + s * f, y1 - s * 0.12), fill=ink, width=max(1, w - 1))


def g_gear(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.12)
    cx, cy, r = (x0 + x1) / 2, (y0 + y1) / 2, (x1 - x0) / 2
    for i in range(8):
        a = i * math.pi / 4
        d.line((cx + r * 0.55 * math.cos(a), cy + r * 0.55 * math.sin(a), cx + r * math.cos(a), cy + r * math.sin(a)),
               fill=ink, width=_w(b) + 2)
    d.ellipse((cx - r * 0.7, cy - r * 0.7, cx + r * 0.7, cy + r * 0.7), outline=ink, width=_w(b) + 1)
    d.ellipse((cx - r * 0.25, cy - r * 0.25, cx + r * 0.25, cy + r * 0.25), outline=ink, width=_w(b))


def g_magnifier(d, b, ink, sign: str = ""):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s, w = x1 - x0, _w(b) + 1
    d.ellipse((x0, y0, x0 + s * 0.65, y0 + s * 0.65), outline=ink, width=w)
    d.line((x0 + s * 0.56, y0 + s * 0.56, x1, y1), fill=ink, width=w + 1)
    cx, cy, r = x0 + s * 0.325, y0 + s * 0.325, s * 0.16
    if sign:
        d.line((cx - r, cy, cx + r, cy), fill=ink, width=max(1, w - 1))
    if sign == "+":
        d.line((cx, cy - r, cx, cy + r), fill=ink, width=max(1, w - 1))


def g_zoom_in(d, b, ink):
    g_magnifier(d, b, ink, "+")


def g_zoom_out(d, b, ink):
    g_magnifier(d, b, ink, "-")


def _star_points(cx, cy, r_out, r_in, n=5):
    pts = []
    for i in range(n * 2):
        r = r_out if i % 2 == 0 else r_in
        a = -math.pi / 2 + i * math.pi / n
        pts.append((cx + r * math.cos(a), cy + r * math.sin(a)))
    return pts


def g_star(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.12)
    d.polygon(_star_points((x0 + x1) / 2, (y0 + y1) / 2 + 1, (x1 - x0) / 2, (x1 - x0) / 5), outline=ink, width=_w(b))


def g_heart(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s = x1 - x0
    d.ellipse((x0, y0 + s * 0.05, x0 + s * 0.55, y0 + s * 0.55), fill=ink)
    d.ellipse((x1 - s * 0.55, y0 + s * 0.05, x1, y0 + s * 0.55), fill=ink)
    d.polygon([(x0 + s * 0.03, y0 + s * 0.38), (x1 - s * 0.03, y0 + s * 0.38), ((x0 + x1) / 2, y1)], fill=ink)


def g_share(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s, w = x1 - x0, _w(b)
    d.line((x0 + s * 0.3, y0 + s * 0.38, x0 + s * 0.12, y0 + s * 0.38), fill=ink, width=w)
    d.line((x0 + s * 0.12, y0 + s * 0.38, x0 + s * 0.12, y1), fill=ink, width=w)
    d.line((x0 + s * 0.12, y1, x1 - s * 0.12, y1), fill=ink, width=w)
    d.line((x1 - s * 0.12, y1, x1 - s * 0.12, y0 + s * 0.38), fill=ink, width=w)
    d.line((x1 - s * 0.12, y0 + s * 0.38, x1 - s * 0.3, y0 + s * 0.38), fill=ink, width=w)
    cx = (x0 + x1) / 2
    d.line((cx, y0 + s * 0.12, cx, y0 + s * 0.68), fill=ink, width=w)
    _arrow_head(d, (cx, y0), -math.pi / 2, s * 0.22, ink)


def g_paperplane(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.12)
    s = x1 - x0
    d.polygon([(x0, y0 + s * 0.45), (x1, y0), (x0 + s * 0.55, y1), (x0 + s * 0.42, y0 + s * 0.58)], outline=ink,
              width=_w(b))
    d.line((x0 + s * 0.42, y0 + s * 0.58, x1, y0), fill=ink, width=_w(b))


def g_envelope(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.12)
    s = x1 - x0
    d.rectangle((x0, y0 + s * 0.18, x1, y1 - s * 0.12), outline=ink, width=_w(b))
    d.line((x0, y0 + s * 0.18, (x0 + x1) / 2, y0 + s * 0.55, x1, y0 + s * 0.18), fill=ink, width=_w(b))


def g_pencil(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s, w = x1 - x0, _w(b) + 1
    d.line((x0 + s * 0.15, y1 - s * 0.15, x1 - s * 0.1, y0 + s * 0.1), fill=ink, width=w + 2)
    d.polygon([(x0, y1), (x0 + s * 0.08, y1 - s * 0.3), (x0 + s * 0.3, y1 - s * 0.08)], fill=ink)


def g_compose(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.12)
    s = x1 - x0
    d.rectangle((x0, y0 + s * 0.2, x1 - s * 0.2, y1), outline=ink, width=_w(b))
    g_pencil(d, (x0 + s * 0.3, y0 - s * 0.05, x1 + s * 0.05, y0 + s * 0.7), ink)


def g_bell(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s = x1 - x0
    d.pieslice((x0 + s * 0.12, y0 + s * 0.08, x1 - s * 0.12, y0 + s * 0.9), 180, 360, outline=ink, width=_w(b))
    d.line((x0 + s * 0.12, y0 + s * 0.49, x0 + s * 0.05, y0 + s * 0.8, x1 - s * 0.05, y0 + s * 0.8, x1 - s * 0.12,
            y0 + s * 0.49), fill=ink, width=_w(b))
    d.ellipse(((x0 + x1) / 2 - s * 0.1, y1 - s * 0.18, (x0 + x1) / 2 + s * 0.1, y1), fill=ink)


def g_bookmark(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.18)
    s = x1 - x0
    d.polygon([(x0 + s * 0.15, y0), (x1 - s * 0.15, y0), (x1 - s * 0.15, y1), ((x0 + x1) / 2, y1 - s * 0.3),
               (x0 + s * 0.15, y1)], outline=ink, width=_w(b))


def g_download(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s, cx = x1 - x0, (x0 + x1) / 2
    d.line((cx, y0, cx, y0 + s * 0.6), fill=ink, width=_w(b) + 1)
    _arrow_head(d, (cx, y0 + s * 0.72), math.pi / 2, s * 0.28, ink)
    d.line((x0, y0 + s * 0.7, x0, y1, x1, y1, x1, y0 + s * 0.7), fill=ink, width=_w(b))


def g_upload(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s, cx = x1 - x0, (x0 + x1) / 2
    d.line((cx, y0 + s * 0.15, cx, y0 + s * 0.72), fill=ink, width=_w(b) + 1)
    _arrow_head(d, (cx, y0), -math.pi / 2, s * 0.28, ink)
    d.line((x0, y0 + s * 0.7, x0, y1, x1, y1, x1, y0 + s * 0.7), fill=ink, width=_w(b))


def g_refresh(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.18)
    s = x1 - x0
    d.arc((x0, y0, x1, y1), 40, 330, fill=ink, width=_w(b) + 1)
    _arrow_head(d, (x0 + s * 0.5 + s * 0.5 * math.cos(math.radians(330)),
                    y0 + s * 0.5 + s * 0.5 * math.sin(math.radians(330))), math.radians(60), s * 0.3, ink)


def g_undo(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.18)
    s = x1 - x0
    d.arc((x0 + s * 0.1, y0 + s * 0.2, x1, y1), 200, 90, fill=ink, width=_w(b) + 1)
    _arrow_head(d, (x0, y0 + s * 0.45), math.radians(200), s * 0.28, ink)


def g_redo(d, b, ink):
    im = Image.new("L", (int(b[2] - b[0]) + 1, int(b[3] - b[1]) + 1), 0)
    g_undo(ImageDraw.Draw(im), (0, 0, im.width - 1, im.height - 1), 255)
    d._image.paste(ink, (int(b[0]), int(b[1])), im.transpose(Image.Transpose.FLIP_LEFT_RIGHT))


def g_home(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s = x1 - x0
    d.line((x0, y0 + s * 0.48, (x0 + x1) / 2, y0, x1, y0 + s * 0.48), fill=ink, width=_w(b) + 1)
    d.rectangle((x0 + s * 0.15, y0 + s * 0.42, x1 - s * 0.15, y1), outline=ink, width=_w(b))
    d.rectangle((x0 + s * 0.42, y0 + s * 0.68, x1 - s * 0.42, y1), fill=ink)


def g_sidebar(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.12)
    s = x1 - x0
    d.rounded_rectangle((x0, y0 + s * 0.12, x1, y1 - s * 0.12), radius=s * 0.1, outline=ink, width=_w(b))
    d.line((x0 + s * 0.35, y0 + s * 0.12, x0 + s * 0.35, y1 - s * 0.12), fill=ink, width=_w(b))


def g_list(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s = x1 - x0
    for f in (0.15, 0.5, 0.85):
        y = y0 + s * f
        d.ellipse((x0, y - s * 0.07, x0 + s * 0.14, y + s * 0.07), fill=ink)
        d.line((x0 + s * 0.28, y, x1, y), fill=ink, width=_w(b))


def g_grid(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s = x1 - x0
    for fx in (0, 0.56):
        for fy in (0, 0.56):
            d.rectangle((x0 + s * fx, y0 + s * fy, x0 + s * (fx + 0.44), y0 + s * (fy + 0.44)), outline=ink,
                        width=_w(b))


def g_info(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.1)
    s, cx = x1 - x0, (x0 + x1) / 2
    d.ellipse((x0, y0, x1, y1), outline=ink, width=_w(b))
    d.ellipse((cx - s * 0.06, y0 + s * 0.2, cx + s * 0.06, y0 + s * 0.32), fill=ink)
    d.line((cx, y0 + s * 0.42, cx, y0 + s * 0.78), fill=ink, width=_w(b) + 1)


def g_lock(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s = x1 - x0
    d.arc((x0 + s * 0.2, y0, x1 - s * 0.2, y0 + s * 0.8), 180, 360, fill=ink, width=_w(b) + 1)
    d.rectangle((x0 + s * 0.1, y0 + s * 0.42, x1 - s * 0.1, y1), fill=ink)


def g_folder(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.12)
    s = x1 - x0
    d.polygon([(x0, y0 + s * 0.15), (x0 + s * 0.38, y0 + s * 0.15), (x0 + s * 0.48, y0 + s * 0.28), (x1, y0 + s * 0.28),
               (x1, y1 - s * 0.1), (x0, y1 - s * 0.1)], outline=ink, width=_w(b))


def g_calendar(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.12)
    s = x1 - x0
    d.rectangle((x0, y0 + s * 0.12, x1, y1), outline=ink, width=_w(b))
    d.rectangle((x0, y0 + s * 0.12, x1, y0 + s * 0.32), fill=ink)
    for fx in (0.2, 0.5, 0.8):
        for fy in (0.5, 0.75):
            d.rectangle((x0 + s * fx - 1, y0 + s * fy - 1, x0 + s * fx + 1, y0 + s * fy + 1), fill=ink)


def g_sun(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.1)
    cx, cy, r = (x0 + x1) / 2, (y0 + y1) / 2, (x1 - x0) / 2
    d.ellipse((cx - r * 0.42, cy - r * 0.42, cx + r * 0.42, cy + r * 0.42), outline=ink, width=_w(b))
    for i in range(8):
        a = i * math.pi / 4
        d.line((cx + r * 0.62 * math.cos(a), cy + r * 0.62 * math.sin(a), cx + r * math.cos(a), cy + r * math.sin(a)),
               fill=ink, width=_w(b))


def g_moon(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s = x1 - x0
    mask = Image.new("L", (int(s) + 2, int(s) + 2), 0)
    md = ImageDraw.Draw(mask)
    md.ellipse((0, 0, s, s), fill=255)
    md.ellipse((s * 0.35, -s * 0.1, s * 1.15, s * 0.7), fill=0)
    d._image.paste(ink, (int(x0), int(y0)), mask)


def g_flag(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s = x1 - x0
    d.line((x0 + s * 0.12, y0, x0 + s * 0.12, y1), fill=ink, width=_w(b) + 1)
    d.polygon([(x0 + s * 0.12, y0), (x1, y0 + s * 0.08), (x1 - s * 0.15, y0 + s * 0.3), (x1, y0 + s * 0.52),
               (x0 + s * 0.12, y0 + s * 0.52)], fill=ink)


def g_printer(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.12)
    s = x1 - x0
    d.rectangle((x0 + s * 0.22, y0, x1 - s * 0.22, y0 + s * 0.3), outline=ink, width=_w(b))
    d.rounded_rectangle((x0, y0 + s * 0.3, x1, y0 + s * 0.75), radius=s * 0.08, outline=ink, width=_w(b))
    d.rectangle((x0 + s * 0.22, y0 + s * 0.6, x1 - s * 0.22, y1), fill=ink)


def g_camera(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.1)
    s = x1 - x0
    d.rounded_rectangle((x0, y0 + s * 0.25, x1, y1 - s * 0.08), radius=s * 0.1, outline=ink, width=_w(b))
    d.rectangle((x0 + s * 0.32, y0 + s * 0.12, x1 - s * 0.32, y0 + s * 0.25), fill=ink)
    cx, cy = (x0 + x1) / 2, y0 + s * 0.57
    d.ellipse((cx - s * 0.18, cy - s * 0.18, cx + s * 0.18, cy + s * 0.18), outline=ink, width=_w(b))


def g_play(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.2)
    d.polygon([(x0 + 2, y0), (x1, (y0 + y1) / 2), (x0 + 2, y1)], fill=ink)


def g_pause(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.22)
    s = x1 - x0
    d.rectangle((x0, y0, x0 + s * 0.33, y1), fill=ink)
    d.rectangle((x1 - s * 0.33, y0, x1, y1), fill=ink)


def g_skip_forward(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.18)
    s, cy = x1 - x0, (y0 + y1) / 2
    d.polygon([(x0, y0 + s * 0.15), (x0 + s * 0.45, cy), (x0, y1 - s * 0.15)], fill=ink)
    d.polygon([(x0 + s * 0.42, y0 + s * 0.15), (x0 + s * 0.87, cy), (x0 + s * 0.42, y1 - s * 0.15)], fill=ink)
    d.rectangle((x1 - s * 0.1, y0 + s * 0.15, x1, y1 - s * 0.15), fill=ink)


def g_skip_back(d, b, ink):
    im = Image.new("L", (int(b[2] - b[0]) + 1, int(b[3] - b[1]) + 1), 0)
    g_skip_forward(ImageDraw.Draw(im), (0, 0, im.width - 1, im.height - 1), 255)
    d._image.paste(ink, (int(b[0]), int(b[1])), im.transpose(Image.Transpose.FLIP_LEFT_RIGHT))


def g_chevron_left(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.28)
    d.line((x1, y0, x0, (y0 + y1) / 2, x1, y1), fill=ink, width=_w(b) + 1)


def g_chevron_right(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.28)
    d.line((x0, y0, x1, (y0 + y1) / 2, x0, y1), fill=ink, width=_w(b) + 1)


def g_ellipsis(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.12)
    s, cy = x1 - x0, (y0 + y1) / 2
    for f in (0.15, 0.5, 0.85):
        cx = x0 + s * f
        d.ellipse((cx - s * 0.08, cy - s * 0.08, cx + s * 0.08, cy + s * 0.08), fill=ink)


def g_filter(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s = x1 - x0
    d.polygon([(x0, y0), (x1, y0), (x0 + s * 0.6, y0 + s * 0.5), (x0 + s * 0.6, y1), (x0 + s * 0.4, y1 - s * 0.12),
               (x0 + s * 0.4, y0 + s * 0.5)], outline=ink, width=_w(b))


def g_sort(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s = x1 - x0
    d.line((x0 + s * 0.28, y0 + s * 0.2, x0 + s * 0.28, y1), fill=ink, width=_w(b))
    _arrow_head(d, (x0 + s * 0.28, y0), -math.pi / 2, s * 0.25, ink)
    d.line((x1 - s * 0.28, y0, x1 - s * 0.28, y1 - s * 0.2), fill=ink, width=_w(b))
    _arrow_head(d, (x1 - s * 0.28, y1), math.pi / 2, s * 0.25, ink)


def g_link(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.12)
    s = x1 - x0
    d.rounded_rectangle((x0, y0 + s * 0.3, x0 + s * 0.6, y0 + s * 0.7), radius=s * 0.2, outline=ink, width=_w(b) + 1)
    d.rounded_rectangle((x1 - s * 0.6, y0 + s * 0.3, x1, y0 + s * 0.7), radius=s * 0.2, outline=ink, width=_w(b) + 1)


def g_pin(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s, cx = x1 - x0, (x0 + x1) / 2
    d.ellipse((cx - s * 0.28, y0, cx + s * 0.28, y0 + s * 0.56), fill=ink)
    d.polygon([(cx - s * 0.22, y0 + s * 0.42), (cx + s * 0.22, y0 + s * 0.42), (cx, y1)], fill=ink)


def g_person(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s, cx = x1 - x0, (x0 + x1) / 2
    d.ellipse((cx - s * 0.2, y0, cx + s * 0.2, y0 + s * 0.4), fill=ink)
    d.chord((x0 + s * 0.05, y0 + s * 0.5, x1 - s * 0.05, y1 + s * 0.45), 180, 360, fill=ink)


def g_archive(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.12)
    s = x1 - x0
    d.rectangle((x0, y0 + s * 0.1, x1, y0 + s * 0.32), outline=ink, width=_w(b))
    d.rectangle((x0 + s * 0.06, y0 + s * 0.32, x1 - s * 0.06, y1), outline=ink, width=_w(b))
    d.line((x0 + s * 0.38, y0 + s * 0.5, x1 - s * 0.38, y0 + s * 0.5), fill=ink, width=_w(b) + 1)


def g_tag(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.12)
    s = x1 - x0
    d.polygon([(x0, y0 + s * 0.45), (x0 + s * 0.45, y0), (x1, y0), (x1, y0 + s * 0.55), (x0 + s * 0.55, y1)],
              outline=ink, width=_w(b))
    d.ellipse((x1 - s * 0.3, y0 + s * 0.12, x1 - s * 0.15, y0 + s * 0.27), fill=ink)


def g_eye(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.08)
    s, cx, cy = x1 - x0, (x0 + x1) / 2, (y0 + y1) / 2
    d.ellipse((x0, cy - s * 0.28, x1, cy + s * 0.28), outline=ink, width=_w(b))
    d.ellipse((cx - s * 0.15, cy - s * 0.15, cx + s * 0.15, cy + s * 0.15), fill=ink)


def g_mic(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s, cx = x1 - x0, (x0 + x1) / 2
    d.rounded_rectangle((cx - s * 0.17, y0, cx + s * 0.17, y0 + s * 0.6), radius=s * 0.17, fill=ink)
    d.arc((cx - s * 0.32, y0 + s * 0.2, cx + s * 0.32, y0 + s * 0.8), 0, 180, fill=ink, width=_w(b))
    d.line((cx, y0 + s * 0.8, cx, y1), fill=ink, width=_w(b))


def g_speaker(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s, cy = x1 - x0, (y0 + y1) / 2
    d.polygon([(x0, cy - s * 0.15), (x0 + s * 0.2, cy - s * 0.15), (x0 + s * 0.45, y0 + s * 0.05),
               (x0 + s * 0.45, y1 - s * 0.05), (x0 + s * 0.2, cy + s * 0.15), (x0, cy + s * 0.15)], fill=ink)
    d.arc((x0 + s * 0.35, y0 + s * 0.25, x0 + s * 0.75, y1 - s * 0.25), -50, 50, fill=ink, width=_w(b))
    d.arc((x0 + s * 0.4, y0 + s * 0.05, x1, y1 - s * 0.05), -50, 50, fill=ink, width=_w(b))


def g_check(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.18)
    s = x1 - x0
    d.line((x0, y0 + s * 0.55, x0 + s * 0.38, y1 - s * 0.05, x1, y0 + s * 0.1), fill=ink, width=_w(b) + 2)


def g_doc(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s = x1 - x0
    d.polygon([(x0 + s * 0.12, y0), (x1 - s * 0.35, y0), (x1 - s * 0.12, y0 + s * 0.25), (x1 - s * 0.12, y1),
               (x0 + s * 0.12, y1)], outline=ink, width=_w(b))
    for f in (0.45, 0.62, 0.79):
        d.line((x0 + s * 0.3, y0 + s * f, x1 - s * 0.3, y0 + s * f), fill=ink, width=max(1, _w(b) - 1))


def g_rotate(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s = x1 - x0
    d.rectangle((x0, y0 + s * 0.45, x0 + s * 0.55, y1), outline=ink, width=_w(b))
    d.arc((x0 + s * 0.2, y0, x1, y0 + s * 0.8), 270, 0, fill=ink, width=_w(b))
    _arrow_head(d, (x1 - s * 0.1, y0 + s * 0.42), math.pi / 2, s * 0.22, ink)


def g_crop(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s, w = x1 - x0, _w(b) + 1
    d.line((x0 + s * 0.25, y0, x0 + s * 0.25, y1 - s * 0.25, x1, y1 - s * 0.25), fill=ink, width=w)
    d.line((x0, y0 + s * 0.25, x1 - s * 0.25, y0 + s * 0.25, x1 - s * 0.25, y1), fill=ink, width=w)


def g_bolt(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.15)
    s = x1 - x0
    d.polygon([(x0 + s * 0.55, y0), (x0 + s * 0.15, y0 + s * 0.58), (x0 + s * 0.45, y0 + s * 0.58),
               (x0 + s * 0.35, y1), (x1 - s * 0.1, y0 + s * 0.4), (x0 + s * 0.55, y0 + s * 0.4)], fill=ink)


def g_globe(d, b, ink):
    x0, y0, x1, y1 = _box(b, (b[2] - b[0]) * 0.1)
    s, cx, cy = x1 - x0, (x0 + x1) / 2, (y0 + y1) / 2
    d.ellipse((x0, y0, x1, y1), outline=ink, width=_w(b))
    d.ellipse((cx - s * 0.22, y0, cx + s * 0.22, y1), outline=ink, width=max(1, _w(b) - 1))
    d.line((x0, cy, x1, cy), fill=ink, width=max(1, _w(b) - 1))


GLYPHS: dict[str, Callable[..., None]] = {name[2:]: fn for name, fn in globals().items()
                                          if name.startswith("g_") and callable(fn)}


# ---- scenes --------------------------------------------------------------------------------------------------

def _colour(theme: dict[str, str], name: str) -> str:
    return theme.get(name, name)


def render(scene: dict[str, Any]) -> Image.Image:
    """The scene's screenshot in screen points (RGB). Deterministic: the same JSON gives the same bytes."""
    theme = THEMES[scene.get("theme", "light")]
    w, h = scene.get("screen", [1440, 900])
    image = Image.new("RGB", (int(w), int(h)), theme["bg"])
    d = ImageDraw.Draw(image)
    d.rectangle((0, 0, w, 24), fill=theme["toolbar"])                                 # the menu bar
    d.text((14, 5), scene.get("app", ""), fill=theme["ink"], font_size=13)
    wx, wy, ww, wh = scene.get("window", [80, 60, w - 160, h - 120])
    d.rounded_rectangle((wx, wy, wx + ww, wy + wh), radius=10, fill=theme["window"], outline=theme["line"])
    for pane in scene.get("panes", []):
        px, py, pw, ph = pane["rect"]
        d.rectangle((px, py, px + pw, py + ph), fill=_colour(theme, pane.get("fill", "content")))
    for i, colour in enumerate(("#ff5f57", "#febc2e", "#28c840")):                 # traffic lights: not controls
        d.ellipse((wx + 14 + i * 20, wy + 14, wx + 26 + i * 20, wy + 26), fill=colour)
    if scene.get("title"):
        d.text((wx + ww / 2, wy + 14), scene["title"], fill=theme["dim"], font_size=13, anchor="mt")
    for t in scene.get("texts", []):
        d.text(tuple(t["at"]), t["text"], fill=_colour(theme, t.get("colour", "ink")), font_size=t.get("size", 13))
    for c in scene.get("controls", []):
        _draw_control(d, theme, c)
    return image


def _draw_control(d: ImageDraw.ImageDraw, theme: dict[str, str], c: dict[str, Any]) -> None:
    x, y, w, h = c["rect"]
    style = c.get("style", "plain")
    draw = c.get("draw", {})
    value = c.get("value", "")
    on = value == "selected"
    ink = theme["ink"] if c.get("enabled", True) else theme["dim"]
    if style == "bordered":
        d.rounded_rectangle((x, y, x + w, y + h), radius=6, fill=theme["selected"] if on else theme["button"],
                            outline=theme["line"])
    elif style == "segment":
        d.rounded_rectangle((x, y, x + w, y + h), radius=5, fill=theme["selected"] if on else theme["button"])
    elif style == "row":
        if on:
            d.rectangle((x, y, x + w, y + h), fill=theme["row_sel"])
        d.line((x, y + h, x + w, y + h), fill=theme["line"])
    elif style == "checkbox":
        box = (x + 2, y + (h - 14) / 2, x + 16, y + (h + 14) / 2)
        d.rounded_rectangle(box, radius=3, outline=theme["dim"],
                            fill=theme["selected"] if value == "checked" else None)
        if value == "checked":
            g_check(d, box, "#ffffff")
        x, w = x + 22, w - 22
    if on and style in ("bordered", "segment"):
        ink = theme["selected_ink"]
    if style == "link":
        ink = theme["accent"]
    glyph, text = draw.get("glyph"), draw.get("text")
    size = draw.get("size", 13)
    cx = x + 6
    if glyph:
        side = min(h - 6, draw.get("glyph_size", 18))
        if text:
            gb = (x + 6, y + (h - side) / 2, x + 6 + side, y + (h + side) / 2)
            cx = x + 12 + side
        else:
            gb = (x + (w - side) / 2, y + (h - side) / 2, x + (w + side) / 2, y + (h + side) / 2)
        GLYPHS[glyph](d, gb, theme["accent"] if draw.get("accent") else ink)
    if text:
        if glyph or style in ("row", "checkbox", "link") or draw.get("align") == "left":
            d.text((cx, y + h / 2), text, fill=ink, font_size=size, anchor="lm")
        else:
            d.text((x + w / 2, y + h / 2), text, fill=ink, font_size=size, anchor="mm")


def snapshot(scene: dict[str, Any], seq: int = 1) -> Snapshot:
    """The AX snapshot a walk of this scene would give: controls and static texts in reading order."""
    app = scene.get("app", "")
    elements: list[Candidate] = []
    for c in scene.get("controls", []):
        elements.append(Candidate(
            id=str(c["id"]), role=c.get("role", "button"), label=c.get("label", ""), value=c.get("value", ""),
            frame=tuple(int(v) for v in c["rect"]), actions=("AXPress",), enabled=bool(c.get("enabled", True)),
            app=app, in_content=False, in_dialog=False, secure=False, editable=False))
    for t in scene.get("texts", []):
        size = t.get("size", 13)
        elements.append(Candidate(
            id=str(t["id"]), role="static text", label=t["text"], value="",
            frame=(int(t["at"][0]), int(t["at"][1]), int(len(t["text"]) * size * 0.55), int(size * 1.3)),
            actions=(), enabled=True, app=app, in_content=False, in_dialog=False, secure=False, editable=False))
    elements.sort(key=lambda e: (e.frame[1], e.frame[0]))
    ids = [e.id for e in elements]
    if len(set(ids)) != len(ids):
        raise ValueError(f"scene {scene.get('id')}: duplicate element ids")
    context = ([f"title: {scene['title']}"] if scene.get("title") else []) + [t["text"] for t in scene.get("texts", [])]
    return Snapshot(seq=seq, app=app, bundle=scene.get("bundle", "com.example.scene"), title=scene.get("title", ""),
                    pid=4242, elements=tuple(elements), context_lines=tuple(context[:8]),
                    node_count=len(elements) * 3)


def png_bytes(image: Image.Image) -> bytes:
    out = io.BytesIO()
    image.save(out, format="PNG", optimize=False)
    return out.getvalue()


def image_sha(scene: dict[str, Any]) -> str:
    return hashlib.sha256(png_bytes(render(scene))).hexdigest()


def validate(data: dict[str, Any]) -> list[str]:
    """Problems with a scene file's shape (an empty list when it is well formed)."""
    problems: list[str] = []
    scenes = {s.get("id"): s for s in data.get("scenes", [])}
    if len(scenes) != len(data.get("scenes", [])):
        problems.append("duplicate scene ids")
    for s in scenes.values():
        for c in s.get("controls", []):
            if c.get("role", "button") not in ROLES:
                problems.append(f"{s['id']}/{c.get('id')}: role {c.get('role')!r} not in {ROLES}")
            if c.get("style", "plain") not in STYLES:
                problems.append(f"{s['id']}/{c.get('id')}: style {c.get('style')!r} not in {STYLES}")
            g = c.get("draw", {}).get("glyph")
            if g and g not in GLYPHS:
                problems.append(f"{s['id']}/{c.get('id')}: unknown glyph {g!r}")
        try:
            snapshot(s)
        except ValueError as e:
            problems.append(str(e))
    for case in data.get("cases", []):
        if case.get("scene") not in scenes:
            problems.append(f"case {case.get('id')}: unknown scene")
            continue
        ids = {str(c["id"]) for c in scenes[case["scene"]].get("controls", [])}
        if case.get("kind") not in ("click", "abstain", "risk", "distractor"):
            problems.append(f"case {case.get('id')}: kind must be click|abstain|risk|distractor")
        exp = case.get("expected_id")
        if case.get("kind") in ("click", "distractor") and str(exp) not in ids:
            problems.append(f"case {case.get('id')}: expected_id {exp!r} is not a control in the scene")
        if case.get("kind") in ("abstain", "risk") and exp is not None:
            problems.append(f"case {case.get('id')}: abstain and risk cases have expected_id null")
    for pair in data.get("verify", []):
        for side in ("before", "after"):
            if pair.get(side) not in scenes:
                problems.append(f"verify {pair.get('id')}: unknown {side} scene")
        if not isinstance(pair.get("done"), bool):
            problems.append(f"verify {pair.get('id')}: done must be true or false")
        if pair.get("before") in scenes and str(pair.get("clicked")) not in {
                str(c["id"]) for c in scenes[pair["before"]].get("controls", [])}:
            problems.append(f"verify {pair.get('id')}: clicked is not a control in the before scene")
    return problems


# ---- scope: the code before the judge ------------------------------------------------------------------------

LANE_MISSES = ("no_match", "ambiguous_target", "uncovered")


class _Senses:
    def __init__(self, snap: Snapshot) -> None:
        self._snap = snap

    def snapshot(self) -> Snapshot:
        return self._snap

    def text_visible(self, _s: str) -> bool:
        return False

    def focused(self) -> Any:
        return None

    def frontmost_pid(self) -> int:
        return self._snap.pid

    def screen_changed(self) -> Any:
        return True


class _Effectors:
    def __init__(self) -> None:
        self.clicked: list[str] = []

    def click_candidate(self, c: Candidate) -> str:
        self.clicked.append(c.id)
        return "clicked"

    def focus_and_type(self, c: Candidate, text: str) -> str:
        return "refused"

    def press(self, key: str) -> str:
        return "refused"

    def settle(self, secs: float) -> float:
        return 0.0


def scope(scene: dict[str, Any], case: dict[str, Any]) -> dict[str, Any]:
    """Would this case reach the visual judge? The keyword lane must miss it (status none with a miss reason), the
    request gates must let it through, and the menu must offer the expected control (and, for a risk case, the
    risky one). ``in_scope`` is all of them."""
    from eve_bridge import decision_lane, lane_router
    from eve_bridge.task_router import CONSEQUENTIAL, TELL_ME, TYPING

    snap = snapshot(scene)
    goal = case["goal"]
    eff = _Effectors()
    lane = lane_router.route(goal, senses=_Senses(snap), effectors=eff, frontmost_app=snap.app, apps=[],
                             dry_run=True)
    text = lane_router.normalise(goal)
    refused = bool(lane_router.refuse_reason(text) or CONSEQUENTIAL.search(text) or TYPING.search(text)
                   or TELL_ME.search(text))
    clauses = lane_router.split_clauses(text)
    menu = decision_lane.menu(clauses[0] if clauses else text, snap) if not refused else {}
    lane_miss = lane.status == "none" and lane.reason in LANE_MISSES and not eff.clicked
    out = {"id": case.get("id"), "lane": f"{lane.status}:{lane.reason}", "lane_miss": lane_miss,
           "request_refused": refused, "clauses": len(clauses), "offered": len(menu)}
    exp = case.get("expected_id")
    out["expected_offered"] = exp is None or str(exp) in menu
    risky = case.get("risky_id")
    out["risky_offered"] = risky is None or str(risky) in menu
    out["in_scope"] = (lane_miss and not refused and len(clauses) == 1 and bool(menu) and out["expected_offered"]
                       and out["risky_offered"])
    return out


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--render", type=Path)
    p.add_argument("--scope", type=Path)
    p.add_argument("--glyphs", action="store_true")
    p.add_argument("--out", type=Path)
    a = p.parse_args(argv)
    if a.glyphs:
        names = sorted(GLYPHS)
        sheet = Image.new("RGB", (8 * 110, math.ceil(len(names) / 8) * 90), "#ffffff")
        d = ImageDraw.Draw(sheet)
        for i, name in enumerate(names):
            x, y = (i % 8) * 110, (i // 8) * 90
            GLYPHS[name](d, (x + 35, y + 10, x + 75, y + 50), "#1d1d1f")
            d.text((x + 55, y + 70), name, fill="#444444", font_size=12, anchor="mm")
        a.out.mkdir(parents=True, exist_ok=True)
        sheet.save(a.out / "glyphs.png")
        print(a.out / "glyphs.png")
    for path in filter(None, (a.render, a.scope)):
        data = json.loads(path.read_text(encoding="utf-8"))
        problems = validate(data)
        if problems:
            print("\n".join(problems))
            return 1
    if a.render:
        data = json.loads(a.render.read_text(encoding="utf-8"))
        a.out.mkdir(parents=True, exist_ok=True)
        for s in data["scenes"]:
            render(s).save(a.out / f"{s['id']}.png")
        print(f"rendered {len(data['scenes'])} scenes into {a.out}")
    if a.scope:
        data = json.loads(a.scope.read_text(encoding="utf-8"))
        scenes = {s["id"]: s for s in data["scenes"]}
        rows = [scope(scenes[c["scene"]], c) for c in data.get("cases", [])]
        for r in rows:
            print(json.dumps(r))
        print(f"in scope: {sum(r['in_scope'] for r in rows)} of {len(rows)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
