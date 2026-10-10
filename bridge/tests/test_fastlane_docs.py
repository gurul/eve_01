"""The fast-lane docs name every knob, every result status and the helper, and the documented
defaults match the shipped constants (fast_lane.FAST_LANE_DEFAULT, fast_lane.DEFAULT_STYLE)."""

from __future__ import annotations

import re
from pathlib import Path

from eve_bridge.fast_lane import (
    DEFAULT_DECIDE,
    DEFAULT_STYLE,
    FAST_LANE_DEFAULT,
    LANE_FIRST_DEFAULT,
    STATUSES,
)

ROOT = Path(__file__).resolve().parents[2]
VOICE = ROOT / "docs" / "stackchan" / "voice.md"
KNOBS = ("EVE_FAST_LANE", "EVE_FAST_LANE_STYLE",
         "EVE_LANE_FIRST", "EVE_FAST_LANE_DECIDE", "EVE_DECIDER")


def _knob_row(text: str, knob: str) -> str:
    match = re.search(rf"^\| `{knob}` \| `([^`]*)` \|", text, re.M)
    assert match, f"no Knobs row for {knob}"
    return match.group(1)


def test_voice_doc_names_every_knob_status_and_the_helper() -> None:
    text = VOICE.read_text(encoding="utf-8")
    for knob in KNOBS:
        assert knob in text, knob
        _knob_row(text, knob)
    section = text[text.index("## The fast lane"):text.index("## Safety")]
    for status in STATUSES:
        assert re.search(rf"^{status}:", section, re.M), status
    assert "delegate(objective" in section and "`delegate`" in text
    for word in ("confirm", "escalate", "approve", "done_when", "timing", "settle"):
        assert word in section, word


def test_documented_defaults_match_the_shipped_constants() -> None:
    text = VOICE.read_text(encoding="utf-8")
    assert _knob_row(text, "EVE_FAST_LANE") == ("1" if FAST_LANE_DEFAULT else "0")
    assert _knob_row(text, "EVE_FAST_LANE_STYLE") == DEFAULT_STYLE
    assert _knob_row(text, "EVE_LANE_FIRST") == ("1" if LANE_FIRST_DEFAULT else "0")
    assert _knob_row(text, "EVE_FAST_LANE_DECIDE") == DEFAULT_DECIDE
    assert _knob_row(text, "EVE_DECIDER") == "unset"
    assert "EVE_LAYA_MODEL" not in text           # the Laya click lane and its checkpoint knob are gone


def test_voice_doc_describes_the_router_the_script_form_and_the_prompt_teaches_it() -> None:
    from eve_bridge import computer_agent as ca
    from eve_bridge import lane_router as lr

    text = VOICE.read_text(encoding="utf-8")
    section = text[text.index("### Lane first: the router before the planner"):text.index("### The gates in code")]
    for word in ("complete", "partial", "refused", "lane_router.py", "--router", "--live-route", "no planner call",
                 "delegate(steps=[", "script: {complete|partial|none}", "keyword", "uncovered"):
        assert word in section, word
    for reason in ("no_match", "uncovered", "ambiguous_target"):
        assert reason in text, reason
    # every route status the code can return is in the doc's table, and the prompt names the script form
    for status in lr.STATUSES:
        assert f"`{status}`" in section, status
    prompt = ca.instructions(True)
    assert "delegate(steps=[" in prompt and "script: complete" in prompt and "approve=" in prompt
    assert "lane_router.py" in (ROOT / "README.md").read_text(encoding="utf-8")
    assert "EVE_LANE_FIRST" in (ROOT / "bridge" / "README.md").read_text(encoding="utf-8")


def test_readmes_point_at_the_lane() -> None:
    root = (ROOT / "README.md").read_text(encoding="utf-8")
    assert "fast_lane.py" in root and "decider.py" in root and "ax_candidates.py" in root and "[laya]" in root
    bridge = (ROOT / "bridge" / "README.md").read_text(encoding="utf-8")
    assert '".[laya]"' in bridge and "EVE_FAST_LANE" in bridge and "voice.md#the-fast-lane" in bridge
    assert "[fast]" not in root and "[fast]" not in bridge       # the extra is `laya` now (the eye expressions)
