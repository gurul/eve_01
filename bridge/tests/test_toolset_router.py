"""toolset_router.py (which of the owner's toolsets a Telegram turn starts with: keywords, plus Jev's Nouls under
fitted gates) and tools/toolset_eval.py's arithmetic. Fakes only: no Jev call, no token.

The live evidence is tools/toolset_eval.py: --fit on toolset_tuning.json, --holdout once on the blind
toolset_holdout.json against the pre-registered bar. These pin the question's shape, the rule the code owns
(keywords always stand; Jev only adds), and the keyword nets against the policy's hard negatives.
"""

from __future__ import annotations

import json
import sys
import threading
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import toolset_router as tr

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))

import toolset_eval  # noqa: E402

ROUTES = Path(__file__).parent / "fixtures" / "routes"


def nouls(**values: float) -> dict[str, Any]:
    return {"answers": {name: {"noul": values.get(name, 0.0)} for name in tr.TOOLSETS}}


def fake(**values: float):
    seen: list[tuple[Any, dict[str, Any]]] = []

    def predict(state: Any, qs: dict[str, Any]) -> dict[str, Any]:
        seen.append((state, qs))
        return nouls(**values)

    return predict, seen


HALF = {name: 0.5 for name in tr.TOOLSETS}


# ---- the question ---------------------------------------------------------------------------------------------

def test_the_state_is_the_message_and_the_previous_one_and_every_toolset_is_one_noul() -> None:
    predict, seen = fake(calendar=0.9)
    tr.ask(predict, "  my next   class ", "  whats   up ", lambda: 0.0)
    state, qs = seen[0]
    assert state == {"about": tr.ABOUT, "previous": "whats up", "request": "my next class"}
    assert set(qs) == set(tr.TOOLSETS)
    assert all(q["type"] == "noul" and q["instructions"].startswith("To do what the owner asks") for q in qs.values())


def test_the_instructions_name_no_internal_tool_or_vendor_the_owner_would_not_say() -> None:
    # the question ids are the toolsets' own names; the instructions are the job, in the owner's words
    words = " ".join(q["instructions"] for q in tr.questions().values()).lower()
    for name in ("composio", "open_app", "apps_mcp", "imessage", "google", "jev", "toolset"):
        assert name not in words


def test_an_answer_is_read_per_toolset_and_a_missing_or_odd_noul_is_zero() -> None:
    def predict(state: Any, qs: dict[str, Any]) -> dict[str, Any]:
        return {"answers": {"calendar": {"noul": 0.7}, "maps": {"noul": True}, "gmail": "x"}}

    a = tr.ask(predict, "x", "", lambda: 0.0)
    assert a.nouls["calendar"] == 0.7 and a.nouls["maps"] == 0.0 and a.nouls["gmail"] == 0.0 and not a.error


# ---- the rule -----------------------------------------------------------------------------------------------------

def test_decide_opens_each_toolset_whose_noul_reaches_its_own_gate() -> None:
    a = tr.ToolsetAnswer({"calendar": 0.5, "maps": 0.49, "share": 0.99}, ms=100)
    assert tr.decide(a, {**HALF, "share": 1.01}) == {"calendar"}


def test_an_error_or_an_answer_slower_than_the_budget_opens_nothing() -> None:
    assert tr.decide(tr.ToolsetAnswer({"calendar": 1.0}, error="JevError: x"), HALF) == frozenset()
    assert tr.decide(tr.ToolsetAnswer({"calendar": 1.0}, ms=900), HALF, budget_ms=800) == frozenset()
    assert tr.decide(tr.ToolsetAnswer({"calendar": 1.0}, ms=700), HALF, budget_ms=800) == {"calendar"}


def test_a_predict_that_raises_is_an_error_answer_not_a_crash() -> None:
    def predict(state: Any, qs: dict[str, Any]) -> dict[str, Any]:
        raise TimeoutError("slow")

    a = tr.ask(predict, "x", "", lambda: 0.0)
    assert a.error.startswith("TimeoutError") and tr.decide(a, HALF) == frozenset()


# ---- the fit ------------------------------------------------------------------------------------------------------

def test_fit_keeps_the_highest_gate_that_still_reaches_the_tuning_recall() -> None:
    answers = [tr.ToolsetAnswer({"calendar": v}) for v in (0.9, 0.62, 0.3)]
    needed = [{"calendar"}, {"calendar"}, set()]
    gates = tr.fit(answers, [set(), set(), set()], needed)
    assert gates["calendar"] == 0.6              # 0.65 loses the 0.62 case; 0.6 keeps both, and is the highest


def test_fit_credits_keywords_so_a_case_they_catch_never_lowers_the_gate() -> None:
    answers = [tr.ToolsetAnswer({"calendar": 0.9}), tr.ToolsetAnswer({"calendar": 0.1})]
    gates = tr.fit(answers, [set(), {"calendar"}], [{"calendar"}, {"calendar"}])
    assert gates["calendar"] == 0.9


def test_fit_with_no_gate_reaching_the_recall_keeps_the_lowest_on_the_grid() -> None:
    answers = [tr.ToolsetAnswer({"maps": 0.0})] * 3
    assert tr.fit(answers, [set()] * 3, [{"maps"}] * 3)["maps"] == tr.GRID[0]


def test_fit_counts_an_answer_slower_than_the_turn_budget_as_none() -> None:
    answers = [tr.ToolsetAnswer({"gmail": 0.99}, ms=5000)]
    assert tr.fit(answers, [set()], [{"gmail"}])["gmail"] == tr.GRID[0]


def test_the_fitted_gates_in_the_module_are_the_ones_the_eval_wrote() -> None:
    written = json.loads((ROUTES / "toolset_gates.json").read_text(encoding="utf-8"))
    assert written["gates"] == tr.GATES == tr.load_gates()
    assert set(tr.GATES) == set(tr.TOOLSETS) and all(g in tr.GRID for g in tr.GATES.values())


def test_load_gates_falls_back_to_the_module_when_the_file_is_missing(tmp_path: Path) -> None:
    assert tr.load_gates(tmp_path / "nope.json") == tr.GATES


# ---- keywords -----------------------------------------------------------------------------------------------------

@pytest.mark.parametrize(("text", "toolset"), [
    ("text ana the deck", tr.SHARE), ("email the cover letter on my desktop to jordan", tr.SHARE),
    ("imessage dad the screenshot from my desktop", tr.SHARE), ("shoot the resume to my mom", tr.SHARE),
    ("any slack dms", tr.WORKSPACE), ("whats new in #eng", tr.WORKSPACE), ("did anyone @ me", tr.WORKSPACE),
    ("check my linear tickets", tr.WORKSPACE), ("open the notion page for the hackathon", tr.WORKSPACE),
])
def test_the_own_keywords_catch_the_plain_asks(text: str, toolset: str) -> None:
    assert toolset in tr.keyword_toolsets(text)


@pytest.mark.parametrize(("text", "toolset"), [
    ("send me the pdf", tr.SHARE),                           # the policy's hard negatives ("email it to me", tr.SHARE),
    ("open slack on my mac", tr.WORKSPACE), ("notion vs obsidian?", tr.WORKSPACE),
    ("lights blue", tr.WORKSPACE),
])
def test_the_own_keywords_stay_off_the_hard_negatives(text: str, toolset: str) -> None:
    assert toolset not in tr.keyword_toolsets(text)


def test_keywords_use_apps_mcp_words_for_the_direct_apps() -> None:
    assert tr.keyword_toolsets("my next class and a maps link") >= {tr.CALENDAR, tr.MAPS}
    assert tr.keyword_toolsets("any open prs on eve") >= {tr.GITHUB}


def test_a_follow_up_that_points_back_adds_the_previous_messages_keywords_and_a_plain_one_does_not() -> None:
    assert tr.keyword_toolsets("and send it to ana", "find my resume in drive") == {tr.DRIVE, tr.SHARE}
    assert tr.keyword_toolsets("dimmer", "lights blue") == set()
    assert tr.DRIVE not in tr.keyword_toolsets("and send it to ana")


# ---- toolsets_for: keywords always, Jev only adds -----------------------------------------------------------------

def test_toolsets_for_is_keywords_union_jev() -> None:
    route = tr.router_from(fake(calendar=0.9, maps=0.1)[0], lambda: 0.0, gates=HALF)
    assert tr.toolsets_for("my next class and a maps link", "", route) == {tr.CALENDAR, tr.MAPS}
    route = tr.router_from(fake(calendar=0.9)[0], lambda: 0.0, gates=HALF)
    assert tr.toolsets_for("whens my thing w the advisor", "", route) == {tr.CALENDAR}


def test_jev_hears_the_previous_message_on_a_follow_up() -> None:
    predict, seen = fake(drive=0.95, share=0.95)
    route = tr.router_from(predict, lambda: 0.0, gates=HALF)
    assert tr.toolsets_for("and send it to ana", "find my resume in drive", route) == {tr.DRIVE, tr.SHARE}
    assert seen[0][0]["previous"] == "find my resume in drive"


def test_a_router_that_times_out_leaves_the_keywords_alone() -> None:
    release = threading.Event()

    def slow(text: str, previous: str) -> tr.Route:
        release.wait(2)
        return tr.Route(frozenset({tr.GITHUB}))

    try:
        assert tr.toolsets_for("any slack dms", "", slow, timeout_s=0.05) == {tr.WORKSPACE}
    finally:
        release.set()


def test_a_router_that_raises_or_errs_or_is_none_leaves_the_keywords_alone() -> None:
    def boom(text: str, previous: str) -> tr.Route:
        raise RuntimeError("x")

    assert tr.toolsets_for("any slack dms", "", boom) == {tr.WORKSPACE}
    erring = tr.router_from(lambda s, q: (_ for _ in ()).throw(TimeoutError()), lambda: 0.0, gates=HALF)
    assert tr.toolsets_for("any slack dms", "", erring) == {tr.WORKSPACE}
    assert tr.toolsets_for("any slack dms", "", None) == {tr.WORKSPACE}


# ---- the switch ---------------------------------------------------------------------------------------------------

@pytest.mark.parametrize(("raw", "want"), [("", "auto"), ("on", "on"), ("1", "on"), ("off", "off"), ("no", "off"),
                                           ("bogus", "auto")])
def test_the_mode_reads_eve_toolset_router(raw: str, want: str) -> None:
    assert tr.mode({"EVE_TOOLSET_ROUTER": raw}) == want


def test_make_router_follows_the_mode_and_shipped_and_needs_a_jev_key(monkeypatch: pytest.MonkeyPatch) -> None:
    key = {"OPENROUTER_API_KEY": "k", "EVE_JEV_ROUTE": "openrouter"}
    tr._JEV.clear()
    assert tr.make_router({**key, "EVE_TOOLSET_ROUTER": "off"}) is None
    assert tr.make_router({"EVE_TOOLSET_ROUTER": "on", "EVE_JEV_ROUTE": "openrouter"}) is None
    on = tr.make_router({**key, "EVE_TOOLSET_ROUTER": "on"})
    assert on is not None and tr.make_router({**key, "EVE_TOOLSET_ROUTER": "on"}) is on   # once a process
    monkeypatch.setattr(tr, "SHIPPED", False)
    assert tr.make_router({**key, "EVE_TOOLSET_ROUTER": "auto"}) is None
    monkeypatch.setattr(tr, "SHIPPED", True)
    assert tr.make_router({**key, "EVE_TOOLSET_ROUTER": "auto"}) is on
    tr._JEV.clear()


# ---- the eval's arithmetic ----------------------------------------------------------------------------------------

def test_the_score_counts_full_recall_extras_and_per_toolset_numbers() -> None:
    s = toolset_eval.score([{"calendar", "maps"}, {"gmail"}, set()], [{"calendar"}, {"gmail", "drive"}, set()])
    assert s["full"] == 2 and s["mean_extras"] == pytest.approx(1 / 3) and s["missed_toolsets"] == 1
    assert s["per"]["drive"]["recall"] == 0.0 and s["per"]["maps"]["precision"] == 0.0


def test_the_bar_needs_all_four_and_strictly_beats_keywords() -> None:
    good = {"full_recall": 0.96, "mean_extras": 0.5}
    assert toolset_eval.bar(good, {"full_recall": 0.7}, 300)["shipped"]
    assert not toolset_eval.bar(good, {"full_recall": 0.96}, 300)["shipped"]          # (2) equal is not higher
    assert not toolset_eval.bar({**good, "full_recall": 0.94}, {"full_recall": 0.7}, 300)["shipped"]
    assert not toolset_eval.bar({**good, "mean_extras": 0.61}, {"full_recall": 0.7}, 300)["shipped"]
    assert not toolset_eval.bar(good, {"full_recall": 0.7}, 801)["shipped"]


def test_the_datasets_carry_the_toolsets_and_the_one_removed_which_the_eval_drops() -> None:
    """The datasets are history and are not edited: they still label the toolset removed on 2026-10-05 (ordering
    delivery). The eval reads only the toolsets that exist now."""
    removed: set[str] = set()
    for name in ("toolset_tuning.json", "toolset_holdout.json"):
        data = json.loads((ROUTES / name).read_text(encoding="utf-8"))
        removed |= {t for c in data["cases"] for t in c["toolsets"]} - set(tr.TOOLSETS)
        assert all(toolset_eval.labels(c) <= set(tr.TOOLSETS) for c in data["cases"])
        assert all({"id", "previous", "text", "toolsets"} <= set(c) for c in data["cases"])
    assert len(removed) == 1


def test_the_eval_drops_a_cached_noul_for_a_toolset_that_no_longer_exists() -> None:
    a = toolset_eval.known(tr.ToolsetAnswer({"calendar": 0.9, "retired": 0.99}, ms=120.0))
    assert dict(a.nouls) == {"calendar": 0.9} and a.ms == 120.0
    assert toolset_eval.labels({"toolsets": ["retired", "maps"]}) == {"maps"}


def test_a_router_that_answers_with_something_else_adds_nothing_and_the_keywords_stand() -> None:
    """Review, 2026-10-05: a router returning None made r.toolsets raise inside the turn's gather and the whole
    turn failed. Anything that is not a Route now counts as no answer."""
    from eve_bridge import toolset_router as r

    out = r.route_turn("whats on my calendar tmrw", "", lambda text, previous: None, timeout_s=1.0)
    assert out.jev == frozenset() and out.error == "not a route" and "calendar" in out.toolsets


@pytest.mark.parametrize(("text", "opens"), [
    ("anything new in my work email", True),
    ("whats on my work calendar tmrw", True),
    ("check the era inbox", True),
    ("i have work tomorrow", False),
    ("email me the work schedule pdf", False),
])
def test_work_mail_and_calendar_open_the_workspace_where_composio_holds_them(text: str, opens: bool) -> None:
    """2026-10-05: the owner's work Gmail and calendar are Composio's (composio_tools.OTHER_ACCOUNT_TOOLKITS)."""
    from eve_bridge import toolset_router as r

    assert ("workspace" in r.keyword_toolsets(text)) is opens
