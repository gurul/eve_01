#!/usr/bin/env python3
"""Capture one real app state for the visual judge's evals: AX walk + screenshot, the PNG kept outside the repo.

The owner drives an app to a state by hand, then (within a few seconds, with the app frontmost):

    .venv/bin/python tools/visual_judge_capture.py --set real-1 --name calendar-week [--app Calendar] [--delay 3]

It writes:

* ``~/.config/eve-bridge/evals/visual-judge/<set>/<name>.png``: the main display in screen points, exactly the
  image the desk lane passes to the judge (``_LaneAdapter.observe``). It never enters the repo: it shows the
  owner's screen, and the repo is public.
* ``~/.config/eve-bridge/evals/visual-judge/<set>/<name>.json``: a fixture in the shape of tests/fixtures/ax
  (``captured``, ``screen``, ``raw``, ``snapshot``, ``cases: []``) plus ``image: {"file", "sha256", "size"}``.
  Redact labels with ``tools/fastlane_eval.py --redact`` before the JSON is copied into a case file, and write
  the cases (BAR.md has the case schema). The PNG's sha256 goes into the frozen case file, so a changed or
  swapped screenshot is refused at scoring time.

A truncated walk is refused, as for the fast-lane fixtures. Nothing is sent anywhere.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import re
import sys
import time
from pathlib import Path
from typing import Any, Optional

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "src"))
sys.path.insert(0, str(ROOT / "tools"))

EVALS = Path(os.path.expanduser("~/.config/eve-bridge/evals/visual-judge"))
_NAME = re.compile(r"^[a-z0-9][a-z0-9_-]{0,63}$")


def png_sha(image: Any) -> tuple[bytes, str]:
    out = io.BytesIO()
    image.save(out, format="PNG")
    data = out.getvalue()
    return data, hashlib.sha256(data).hexdigest()


def points_screenshot() -> Any:
    """The main display, resized to screen points (the same scale as the AX frames)."""
    from PIL import ImageGrab

    from eve_bridge.ax_candidates import main_screen_points

    shot = ImageGrab.grab()
    size = main_screen_points()
    if size and shot.size != tuple(size):
        shot = shot.resize(size)
    return shot.convert("RGB")


def capture(set_name: str, name: str, *, app: str = "", out_dir: Path = EVALS) -> Path:
    from fastlane_eval import make_fixture

    from eve_bridge.ax_candidates import ax_walk, main_screen_points, pid_for_app
    from eve_bridge.desktop_helpers import _focused_pid

    if not _NAME.match(set_name) or not _NAME.match(name):
        raise SystemExit("set and name are lower-case words, digits, - and _")
    pid = pid_for_app(app) if app else _focused_pid()
    if not pid:
        raise SystemExit(f"no running app {app!r}" if app else "no frontmost app")
    screen = main_screen_points()
    raw = ax_walk(pid)
    image = points_screenshot()
    fixture = make_fixture({"raw": raw, "screen": list(screen) if screen else None}, set_name="select")
    fixture["captured"]["set"] = set_name
    data, sha = png_sha(image)
    folder = out_dir / set_name
    folder.mkdir(parents=True, exist_ok=True)
    (folder / f"{name}.png").write_bytes(data)
    fixture["image"] = {"file": f"{name}.png", "sha256": sha, "size": list(image.size)}
    path = folder / f"{name}.json"
    path.write_text(json.dumps(fixture, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    return path


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--set", required=True, dest="set_name")
    p.add_argument("--name", required=True)
    p.add_argument("--app", default="")
    p.add_argument("--delay", type=float, default=0.0, help="seconds to wait first, to bring the app forward")
    a = p.parse_args(argv)
    if a.delay > 0:
        time.sleep(a.delay)
    path = capture(a.set_name, a.name, app=a.app)
    print(path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
