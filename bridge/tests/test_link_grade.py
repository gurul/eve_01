"""Jev's grade of an "auto" Eve Link app turn (grade_router.py wired into telegram.TelegramInlet._think).

The real Mini App server and the real TelegramInlet (test_link_chat.py's rig), with a fake grader and a fake toolset
router standing in for Jev: graded think runs as the explicit think grade, graded instant keeps today's payload,
Jev failing or slow is instant, Telegram turns and explicit grades never ask, and the two Jev calls overlap."""
from __future__ import annotations

import asyncio
import logging
import time
from pathlib import Path
from typing import Any

import pytest
from test_link_chat import events, http, real_rig, serve
from test_telegram import CFG, say

from eve_bridge import grade_router, link_chat, telegram, toolset_router
from eve_bridge.grade_router import GradeAnswer

TEXT = "plan my week around three deadlines and a flight"


class FakeGrader:
    """Jev's grade: a fixed Noul (or an error raised), after ``sleep`` seconds; records each call's times."""

    def __init__(self, noul: float = 0.0, sleep: float = 0.0, fail: bool = False) -> None:
        self.noul, self.sleep, self.fail = noul, sleep, fail
        self.calls: list[tuple[str, str]] = []
        self.spans: list[tuple[float, float]] = []

    def __call__(self, text: str, previous: str = "") -> GradeAnswer:
        t0 = time.perf_counter()
        self.calls.append((text, previous))
        time.sleep(self.sleep)
        self.spans.append((t0, time.perf_counter()))
        if self.fail:
            raise RuntimeError("jev is down")
        return GradeAnswer(self.noul, (time.perf_counter() - t0) * 1000.0)


class FakeRouter:
    """The toolset router's Jev: opens nothing, after ``sleep`` seconds; records each call's times."""

    def __init__(self, sleep: float = 0.0) -> None:
        self.sleep = sleep
        self.spans: list[tuple[float, float]] = []

    def __call__(self, text: str, previous: str = "") -> toolset_router.Route:
        t0 = time.perf_counter()
        time.sleep(self.sleep)
        self.spans.append((t0, time.perf_counter()))
        return toolset_router.Route(ms=(time.perf_counter() - t0) * 1000.0)


def rig_with(tmp_path: Path, grader: Any, *replies: str, router: Any = None) -> Any:
    rig, _ = real_rig(tmp_path, *(say(r) for r in replies or ("ok",)), grader=grader,
                      toolset_mode="auto" if router is not None else "off", toolset_route=router)
    return rig


def app_turns(rig: Any, tmp_path: Path, *grades: str, text: str = TEXT) -> list[tuple[list[str], bool, int]]:
    out: list[tuple[list[str], bool, int]] = []

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        for i, grade in enumerate(grades):
            body = {"text": text, "client_id": f"c{i}", "grade": grade}
            out.append(events((await http(port, "POST", link_chat.TURN_PATH, body, token=token))[2]))
        await server.close()

    asyncio.run(go())
    return out


def tool_names(request: dict[str, Any]) -> list[str]:
    return [t.get("name") for t in request["tools"]]


def test_an_auto_app_turn_graded_think_runs_on_the_think_model(tmp_path: Path,
                                                               caplog: pytest.LogCaptureFixture) -> None:
    grader = FakeGrader(noul=0.99)
    rig = rig_with(tmp_path, grader, "Thought it through.")
    with caplog.at_level(logging.INFO, logger="eve_bridge.telegram"):
        assert app_turns(rig, tmp_path, "auto") == [(["Thought it through."], True, 0)]
    (req,) = rig.create.requests
    assert (req["model"], req["reasoning"]) == (CFG.app_think_model, {"effort": CFG.app_think_effort})
    assert req["model"] == "gpt-6.1-sol"
    assert grader.calls == [(TEXT, "")]
    lines = [r.getMessage() for r in caplog.records if r.getMessage().startswith("telegram: grade:")]
    assert len(lines) == 1 and lines[0].startswith("telegram: grade: think (noul 0.99); jev ")
    assert all(TEXT not in r.getMessage() for r in caplog.records)          # numbers only, never the message


def test_an_auto_app_turn_graded_instant_keeps_todays_payload(tmp_path: Path) -> None:
    graded = rig_with(tmp_path / "graded", FakeGrader(noul=0.01), "Quick.")
    today = rig_with(tmp_path / "today", None, "Quick.")                   # no grader: today's auto turn
    assert app_turns(graded, tmp_path / "graded", "auto") == [(["Quick."], True, 0)]
    assert app_turns(today, tmp_path / "today", "auto") == [(["Quick."], True, 0)]
    (g,), (t,) = graded.create.requests, today.create.requests
    assert (g["model"], g["reasoning"]) == (t["model"], t["reasoning"]) == (CFG.model, {"effort": CFG.effort})
    assert tool_names(g) == tool_names(t) and "think_hard" in tool_names(g)
    assert {k: v for k, v in g.items() if k not in ("input", "instructions")} == {
        k: v for k, v in t.items() if k not in ("input", "instructions")}


@pytest.mark.parametrize("grader", [FakeGrader(fail=True), FakeGrader(noul=0.99, sleep=grade_router.TIMEOUT_S + 0.4)],
                         ids=["error", "timeout"])
def test_jev_failing_or_slow_is_instant(tmp_path: Path, grader: FakeGrader, caplog: pytest.LogCaptureFixture) -> None:
    rig = rig_with(tmp_path, grader, "Quick.")
    with caplog.at_level(logging.INFO, logger="eve_bridge.telegram"):
        assert app_turns(rig, tmp_path, "auto") == [(["Quick."], True, 0)]
    (req,) = rig.create.requests
    assert (req["model"], req["reasoning"]) == (CFG.model, {"effort": CFG.effort})
    assert len(grader.calls) == 1
    lines = [r.getMessage() for r in caplog.records if r.getMessage().startswith("telegram: grade:")]
    assert len(lines) == 1 and lines[0].startswith("telegram: grade: instant (noul 0.00)")


def test_a_telegram_turn_never_calls_the_grader(tmp_path: Path) -> None:
    grader = FakeGrader(noul=0.99)
    rig = rig_with(tmp_path, grader, "In Telegram.", "In the app.")

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        rig.inlet.hear(TEXT, on_call=False)                                # typed, not from the app
        for _ in range(300):
            await asyncio.sleep(0.01)
            if rig.api.sent:
                break
        assert rig.api.sent and grader.calls == []
        # positive control: the same grader is asked on an auto app turn
        body = {"text": TEXT, "client_id": "c", "grade": "auto"}
        got = events((await http(port, "POST", link_chat.TURN_PATH, body, token=token))[2])
        assert got == (["In the app."], True, 0)
        await server.close()

    asyncio.run(go())
    assert len(grader.calls) == 1
    telegram_turn, app_turn = rig.create.requests
    assert telegram_turn["model"] == CFG.model and app_turn["model"] == CFG.app_think_model


def test_explicit_instant_and_think_bypass_the_grader(tmp_path: Path) -> None:
    grader = FakeGrader(noul=0.99)
    rig = rig_with(tmp_path, grader, "Instant.", "Think.")
    assert app_turns(rig, tmp_path, "instant", "think") == [(["Instant."], True, 0), (["Think."], True, 0)]
    assert grader.calls == []
    instant, think = rig.create.requests
    assert (instant["model"], instant["reasoning"]) == (CFG.model, {"effort": "low"})   # not raised to think
    assert (think["model"], think["reasoning"]) == (CFG.app_think_model, {"effort": CFG.app_think_effort})


def test_the_grader_and_the_toolset_router_run_at_the_same_time(tmp_path: Path) -> None:
    grader, router = FakeGrader(noul=0.01, sleep=0.3), FakeRouter(sleep=0.3)
    rig = rig_with(tmp_path, grader, "Quick.", router=router)
    assert app_turns(rig, tmp_path, "auto") == [(["Quick."], True, 0)]
    ((g0, g1),), ((r0, r1),) = grader.spans, router.spans
    assert g0 < r1 and r0 < g1                                             # the two calls overlap
    assert max(g1, r1) - min(g0, r0) < 0.5                                 # in well under 0.6 s, not one after the other


def test_grade_router_off_makes_no_grader(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVE_GRADE_ROUTER", "off")
    assert grade_router.make_grader() is None
    rig_kw = telegram.TelegramInlet.__init__.__kwdefaults__ or {}
    assert rig_kw.get("grader") is telegram._FROM_ENV                      # the inlet builds it from the env once
