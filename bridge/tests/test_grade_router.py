"""The grade router (grade_router.py): its rule in code, its fail-safe paths, and the kept verdict."""

from __future__ import annotations

import sys
import threading
from pathlib import Path

import pytest

from eve_bridge import grade_router as gr

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))


def test_decide_thinks_only_above_the_gate() -> None:
    assert gr.decide(gr.GradeAnswer(0.9, 200), 0.65) == gr.THINK
    assert gr.decide(gr.GradeAnswer(0.65, 200), 0.65) == gr.THINK
    assert gr.decide(gr.GradeAnswer(0.64, 200), 0.65) == gr.INSTANT


@pytest.mark.parametrize("a", [gr.GradeAnswer(0.99, 200, "TimeoutError"), gr.GradeAnswer(0.99, 900)])
def test_an_error_or_a_late_answer_is_instant_todays_behaviour(a: gr.GradeAnswer) -> None:
    assert gr.decide(a, 0.5, budget_ms=800) == gr.INSTANT
    assert gr.decide(gr.GradeAnswer(0.99, 200), 0.5, budget_ms=800) == gr.THINK          # positive control


def test_ask_reads_the_noul_and_survives_a_broken_reply() -> None:
    clock = iter([0.0, 0.2, 0.0, 0.1, 0.0, 0.1]).__next__
    assert gr.ask(lambda s, q: {"answers": {"think": {"noul": 0.8}}}, "plan my week", "", clock).noul == 0.8
    assert gr.ask(lambda s, q: {"answers": {"think": {"noul": True}}}, "x", "", clock).noul == 0.0

    def boom(s, q):
        raise RuntimeError("down")
    assert gr.ask(boom, "x", "", clock).error.startswith("RuntimeError")


def test_grade_turn_is_instant_without_a_grader_on_timeout_and_on_a_raise() -> None:
    assert gr.grade_turn("hi") == gr.Grade()
    hold = threading.Event()
    slow = gr.grade_turn("hi", grader=lambda t, p: (hold.wait(1), gr.GradeAnswer(0.99, 5))[1], timeout_s=0.05)
    hold.set()
    assert (slow.grade, slow.error) == (gr.INSTANT, "timeout")

    def boom(t, p):
        raise ValueError("x")
    assert gr.grade_turn("hi", grader=boom, gate=0.5).grade == gr.INSTANT
    ok = gr.grade_turn("plan it", grader=lambda t, p: gr.GradeAnswer(0.9, 5), gate=0.5)   # positive control
    assert ok.grade == gr.THINK and ok.asked


def test_the_log_line_carries_numbers_never_the_message() -> None:
    line = gr.Grade(gr.THINK, 0.91, 230, True).log_line()
    assert line == "think (noul 0.91); jev 230 ms"


def test_fit_keeps_the_highest_gate_with_the_recall() -> None:
    answers = [gr.GradeAnswer(n, 1) for n in (0.9, 0.8, 0.7, 0.2)]
    gate, rec = gr.fit(answers, [["think"], ["think"], ["think"], ["instant"]])
    assert gate == 0.7 and rec == 1.0


def test_the_question_names_only_the_live_grade_and_the_state_drops_long_text() -> None:
    assert set(gr.questions()) == {gr.THINK}
    assert len(gr.state("x" * 900)["request"]) == 500


def test_shipped_equals_the_kept_holdout_verdict() -> None:
    import grade_eval

    assert grade_eval.check_default().startswith("passed")
