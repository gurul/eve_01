"""typed_ask.ask_laya_request: laya's own idiom for a whole request (a short ranking, a code shortlist)."""

from __future__ import annotations

import time
from typing import Any

from eve_bridge import typed_ask as ta

APPS = ["Safari", "Google Chrome", "Preview", "System Settings", "Notes", "Warp"]


def test_shortlist_is_close_names_only() -> None:
    assert ta.app_shortlist("Get me Preview.", APPS) == ["Preview"]
    assert ta.app_shortlist("can you switch over to google chrome", APPS)[0] == "Google Chrome"
    assert ta.app_shortlist("open safary", APPS) == ["Safari"]            # a close spelling
    assert ta.app_shortlist("play some jazz", APPS) == []


def test_laya_is_asked_short_choices_and_read_as_a_request_answer() -> None:
    seen: dict[str, Any] = {}

    def predict(state: Any, questions: dict[str, Any]) -> dict[str, Any]:
        seen.update(state=state, questions=questions)
        return {"answers": {"kind": {"choice": "open", "probabilities": {"open": 0.9, "send": 0.04, "write": 0.02}},
                            "app": {"choice": "app0", "probabilities": {"app0": 0.97, "none": 0.03}}}}

    a = ta.ask_laya_request(predict, "Get me Preview.", APPS, time.perf_counter)
    assert seen["state"] == {"owner said": "Get me Preview."}
    assert all(len(v.split()) <= 5 for v in seen["questions"]["kind"]["criteria"].values())
    assert a.app == "Preview" and a.launch_only == 0.9 and abs(a.risky - 0.06) < 1e-9


def test_no_shortlist_means_no_app_question_and_a_failure_abstains() -> None:
    asked: list[dict[str, Any]] = []

    def predict(state: Any, questions: dict[str, Any]) -> dict[str, Any]:
        asked.append(questions)
        return {"answers": {}}

    assert ta.ask_laya_request(predict, "play some jazz", APPS, time.perf_counter).app == ""
    assert "app" not in asked[0]

    def boom(state: Any, questions: dict[str, Any]) -> dict[str, Any]:
        raise RuntimeError("down")

    a = ta.ask_laya_request(boom, "Get me Preview.", APPS, time.perf_counter)
    assert a.error and ta.decide_request(a, ta.JEV_REQUEST_GATES) == "other"
