"""Opt-in hardware check for the desktop helpers on THIS Mac's real screen.

    EVE_LIVE_DESKTOP=1 bridge/.venv/bin/python -m pytest -q bridge/tests/test_desktop_live.py

Skipped unless that variable is set and both Accessibility and Screen
Recording are granted to the venv python (a locked screen freezes captures:
see the headless-verify note in the docs). Pins the bottom-left-origin Vision
box conversion and the OCR budget on hardware.
"""

from __future__ import annotations

import os
import subprocess
import sys
import time

import pytest


def _granted() -> bool:
    if sys.platform != "darwin":
        return False
    try:
        from eve_bridge.computer_agent import desktop_grants

        g = desktop_grants(prompt=False)
        return bool(g.get("accessibility")) and bool(g.get("screen"))
    except Exception:  # noqa: BLE001
        return False


live = pytest.mark.skipif(os.environ.get("EVE_LIVE_DESKTOP") != "1" or not _granted(),
                          reason="set EVE_LIVE_DESKTOP=1 with Accessibility + Screen Recording granted")


@live
def test_worker_check_prints_ready() -> None:
    r = subprocess.run([sys.executable, "-m", "eve_bridge.desktop_worker", "--check"],
                       capture_output=True, text=True, timeout=60)
    assert r.returncode == 0 and '"ready": true' in r.stdout


@live
def test_screen_text_finds_the_frontmost_app_in_the_menu_bar() -> None:
    import pyautogui

    from eve_bridge.desktop_helpers import Helpers, ax_frontmost

    h = Helpers(pyautogui)
    app = ax_frontmost()["app"]
    assert app
    t0 = time.monotonic()
    lines = h.screen_text()
    assert time.monotonic() - t0 < 1.5
    menu = [ln["text"] for ln in lines if ln["y"] < 30]
    assert any(app.casefold() in m.casefold() for m in menu), (app, menu)
