"""The eval tools under tools/: what they build their calibration cases from, and where they keep pictures."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))

import app_eval  # noqa: E402
import journey_eval  # noqa: E402


def test_meaning_flips_keep_the_words_and_numbers_and_lose_the_text() -> None:
    got = dict(journey_eval.flips("1-day streak", ("Drink water", "🔥 1-day streak")))
    assert got == {"reword": ["Drink water", "streak ended · best 1 day"]}          # the live build's miss
    got = dict(journey_eval.flips("Alex owes Sam $15.00", ("Balances", "Alex owes Sam $15.00")))
    assert got["swap"] == ["Balances", "Sam owes Alex $15.00"]
    assert dict(journey_eval.flips("6 glasses to go of 8", ("6 glasses to go of 8",)))["swap"] == ["8 glasses to go of 6"]
    # a title's words or a label and its value read the same either way round: no swap
    assert "swap" not in dict(journey_eval.flips("Weekly Summary", ("Weekly Summary",)))
    assert "swap" not in dict(journey_eval.flips("Today · Good", ("Today · Good",)))
    assert journey_eval.flips("Groceries", ("Groceries",)) == []                      # nothing to flip
    assert journey_eval.flips("Not there", ("Something else",)) == []


def test_an_edit_never_overwrites_the_picture_of_the_build_before_it(tmp_path: Path) -> None:
    first = app_eval.screenshot_path(tmp_path, "splitter")
    assert first == tmp_path / "screenshots" / "splitter.jpg"
    first.parent.mkdir(parents=True)
    first.write_bytes(b"v1")
    second = app_eval.screenshot_path(tmp_path, "splitter")
    assert second.name == "splitter-2.jpg"
    second.write_bytes(b"v2")
    assert app_eval.screenshot_path(tmp_path, "splitter").name == "splitter-3.jpg" and first.read_bytes() == b"v1"
