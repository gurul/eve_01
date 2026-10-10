"""search_router.py (Jev picks the provider for every web search: a quick page reader, a many-source search engine, or
live data services) and its one routed entry point, ``answer``. Fakes only: no Jev call, no search, no token.

The live, blind evidence is tools/route_eval.py --search (search_holdout.json, written by another agent); these pin
the shape of the question, the rule the code owns (perplexity whenever in doubt), and the fixtures' labels.
"""

from __future__ import annotations

import json
import sys
import threading
import time
from collections import Counter
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import firecrawl, spend, watch, websearch
from eve_bridge import search_router as sr

ROUTES = Path(__file__).parent / "fixtures" / "routes"
ALL = (sr.PERPLEXITY, sr.TINYFISH, sr.FIRECRAWL)
OPEN = sr.SearchGates(firecrawl=0.8, structured=0.8, tinyfish=0.8, single=0.8, many_max=0.3, changes_max=0.3)


def jev(p_tf: float, p_fc: float, structured: float, many: float, single: float,
        changes: float = 0.0) -> dict[str, Any]:
    return {"answers": {"provider": {"probabilities": {"quick_page_read": p_tf, "live_data_service": p_fc,
                                                       "deep_web_research": max(0.0, 1 - p_tf - p_fc)}},
                        "needs_live_structured_data": {"noul": structured},
                        "needs_many_sources_or_searches": {"noul": many},
                        "answer_changes_over_time": {"noul": changes},
                        "single_lookup_or_page_read": {"noul": single}}}


@pytest.fixture(autouse=True)
def quiet(monkeypatch: pytest.MonkeyPatch) -> list[Any]:
    rows: list[Any] = []
    monkeypatch.setattr(spend, "record", lambda *a, **k: rows.append(("record", a, k)))
    monkeypatch.setattr(spend, "record_chat_completion", lambda *a, **k: rows.append(("chat", a, k)))
    monkeypatch.delenv("EVE_FIRECRAWL_PER_DAY", raising=False)
    firecrawl._CALLS.clear()
    return rows


# ---- the question and the rule ------------------------------------------------------------------------------

def test_the_state_is_the_request_and_jev_always_sees_all_three_options_as_structure() -> None:
    seen: list[Any] = []

    def predict(state: Any, qs: dict[str, Any]) -> dict[str, Any]:
        seen.append((state, qs))
        return jev(0.0, 0.0, 0.0, 1.0, 0.0)

    sr.ask(predict, "  opening   hours ", lambda: 0.0)
    state, qs = seen[0]
    assert state == {"about": sr.ABOUT, "request": "opening hours"}
    assert set(qs["provider"]["criteria"]) == set(sr.OPTION_TO_PROVIDER) == {
        "quick_page_read", "deep_web_research", "live_data_service"}
    for option in qs["provider"]["criteria"].values():
        assert set(option) == {"what", "for", "not_for"}
    assert {k for k, q in qs.items() if q["type"] == "noul"} == {
        "needs_live_structured_data", "answer_changes_over_time", "needs_many_sources_or_searches",
        "single_lookup_or_page_read"}
    # the wording names the job, never a brand: Jev reads what a provider does
    words = json.dumps(sr.questions()).lower()
    assert not any(brand in words for brand in ("tinyfish", "perplexity", "firecrawl", "alexandria", "openrouter"))
    # the quick-read boundary is written on both sides
    assert "changes over time" in " ".join(sr.OPTIONS["quick_page_read"]["not_for"])
    assert "stays the same over time" in " ".join(sr.OPTIONS["deep_web_research"]["not_for"])


@pytest.mark.parametrize("available", [ALL, (sr.PERPLEXITY, sr.TINYFISH), (sr.PERPLEXITY, sr.FIRECRAWL)])
def test_the_question_never_varies_with_the_keys_and_a_missing_pick_is_masked(available: tuple[str, ...]) -> None:
    seen: list[dict[str, Any]] = []

    def predict(state: Any, qs: dict[str, Any]) -> dict[str, Any]:
        seen.append(qs)
        return jev(0.97, 0.01, 0.02, 0.05, 0.97)                    # a clear quick read

    r = sr.make_router(predict, lambda: 0.0, OPEN)("opening hours", available)
    assert seen == [sr.questions()]                                  # the full three-option question, every Mac
    if sr.TINYFISH in available:
        assert r.provider == sr.TINYFISH and not r.error
    else:
        assert r.provider == sr.PERPLEXITY and r.error == "tinyfish is not available"


def test_decide_needs_both_the_choice_and_the_noul_and_else_is_perplexity() -> None:
    def said(*a: float, changes: float = 0.0, error: str = "") -> str:
        return sr.decide(sr.SearchAnswer(a[0], 0.0, a[1], a[2], a[3], a[4], changes=changes, error=error), OPEN)

    assert said(0.05, 0.9, 0.9, 0.2, 0.1) == sr.FIRECRAWL
    assert said(0.05, 0.9, 0.5, 0.2, 0.1) == sr.PERPLEXITY            # the Choice alone is not a gate
    assert said(0.05, 0.5, 0.95, 0.2, 0.1) == sr.PERPLEXITY           # nor the Noul alone
    assert said(0.9, 0.05, 0.05, 0.1, 0.9) == sr.TINYFISH
    assert said(0.9, 0.05, 0.05, 0.6, 0.9) == sr.PERPLEXITY           # many sources or searches: never the reader
    assert said(0.9, 0.05, 0.05, 0.1, 0.9, changes=0.6) == sr.PERPLEXITY    # an answer that moves: never either
    assert said(0.9, 0.05, 0.05, 0.1, 0.9, changes=0.3) == sr.TINYFISH       # at its maximum, still the reader
    assert said(0.9, 0.05, 0.05, 0.1, 0.5) == sr.PERPLEXITY           # not a single lookup
    assert said(0.5, 0.05, 0.05, 0.1, 0.9) == sr.PERPLEXITY
    assert said(0.9, 0.9, 0.9, 0.1, 0.9) == sr.FIRECRAWL              # a data service first when both clear
    assert said(0.9, 0.05, 0.05, 0.1, 0.9, error="TimeoutError") == sr.PERPLEXITY


def test_the_router_never_picks_what_is_not_available_and_a_failure_is_perplexity() -> None:
    route = sr.make_router(lambda s, q: jev(0.05, 0.95, 0.95, 0.1, 0.1), lambda: 0.0, OPEN)
    assert route("cheapest flight", ALL).provider == sr.FIRECRAWL
    assert route("cheapest flight", ALL).p == pytest.approx(0.95)
    assert route("cheapest flight", (sr.PERPLEXITY, sr.TINYFISH)).provider == sr.PERPLEXITY

    def boom(state: Any, qs: Any) -> Any:
        raise TimeoutError("jev slow")

    r = sr.make_router(boom, lambda: 0.0, OPEN)("x", ALL)
    assert r.provider == sr.PERPLEXITY and "TimeoutError" in r.error
    # an empty or odd envelope is no answer, and no answer is perplexity
    assert sr.make_router(lambda s, q: {"answers": {}}, lambda: 0.0, OPEN)("x", ALL).provider == sr.PERPLEXITY


def test_fit_allows_zero_wrong_routes_and_prefers_the_stricter_tie() -> None:
    answers = [sr.SearchAnswer(0.97, 0.0, 0.01, 0.02, 0.05, 0.97),                # tinyfish
               sr.SearchAnswer(0.92, 0.0, 0.01, 0.02, 0.25, 0.9),                 # perplexity: not to the reader
               sr.SearchAnswer(0.97, 0.0, 0.01, 0.02, 0.02, 0.95, changes=0.25),  # perplexity: the answer moves
               sr.SearchAnswer(0.02, 0.0, 0.97, 0.97, 0.05, 0.1),                 # firecrawl
               sr.SearchAnswer(0.02, 0.0, 0.85, 0.9, 0.3, 0.2),                   # tinyfish-labelled: never firecrawl
               sr.SearchAnswer(0.2, 0.7, 0.1, 0.1, 0.9, 0.1)]                     # perplexity
    truths = [sr.TINYFISH, sr.PERPLEXITY, sr.PERPLEXITY, sr.FIRECRAWL, sr.TINYFISH, sr.PERPLEXITY]
    g = sr.fit(answers, truths)
    said = [sr.decide(a, g) for a in answers]
    assert said == [sr.TINYFISH, sr.PERPLEXITY, sr.PERPLEXITY, sr.FIRECRAWL, sr.PERPLEXITY, sr.PERPLEXITY]
    assert not any(sr.wrong(s, t) for s, t in zip(said, truths, strict=True))
    assert g.tinyfish == 0.95 and g.firecrawl == 0.95 and g.many_max == 0.05 and g.changes_max == 0.05
    # nothing safe to route: the gates route nothing
    nothing = sr.fit([sr.SearchAnswer(0.99, 0.0, 0.0, 0.0, 0.0, 0.99)], [sr.PERPLEXITY])
    assert sr.decide(sr.SearchAnswer(0.99, 0.0, 0.0, 0.0, 0.0, 0.99), nothing) == sr.PERPLEXITY


def test_wrong_is_the_costly_direction_only() -> None:
    assert sr.wrong(sr.TINYFISH, sr.PERPLEXITY) and sr.wrong(sr.TINYFISH, sr.FIRECRAWL)
    assert sr.wrong(sr.FIRECRAWL, sr.TINYFISH) and sr.wrong(sr.FIRECRAWL, sr.PERPLEXITY)
    assert not sr.wrong(sr.PERPLEXITY, sr.TINYFISH) and not sr.wrong(sr.PERPLEXITY, sr.FIRECRAWL)


def test_the_placeholder_gates_route_nothing() -> None:
    sure = sr.SearchAnswer(1.0, 0.0, 1.0, 1.0, 0.0, 1.0)
    assert sr.decide(sure, sr.SearchGates()) == sr.PERPLEXITY


# ---- the switch ---------------------------------------------------------------------------------------------

KEYS = {"OPENROUTER_API_KEY": "or-test", "TYPESAFE_API_KEY": "ts-test", "TINYFISH_API_KEY": "tf-test",
        "FIRECRAWL_API_KEY": "fc-test"}


def test_available_follows_the_mode_the_keys_and_the_engine(monkeypatch: pytest.MonkeyPatch) -> None:
    engine = "openrouter-perplexity"
    monkeypatch.setattr(sr, "SHIPPED", False)
    assert sr.available(KEYS, engine) == ()                                        # auto, not shipped
    on = {**KEYS, "EVE_SEARCH_ROUTER": "on"}
    assert sr.available(on, engine) == ALL
    assert sr.available({**on, "EVE_SEARCH_ROUTER": "off"}, engine) == ()
    assert sr.available(on, "openai") == ()                                         # no OpenRouter engine
    assert sr.available({k: v for k, v in on.items() if k != "OPENROUTER_API_KEY"}, engine) == ()
    assert sr.available({k: v for k, v in on.items() if k != "TYPESAFE_API_KEY"}, engine) == ()   # no Jev route
    assert sr.available({k: v for k, v in on.items() if k != "FIRECRAWL_API_KEY"}, engine) == (
        sr.PERPLEXITY, sr.TINYFISH)
    assert sr.available({k: v for k, v in on.items() if k not in ("FIRECRAWL_API_KEY", "TINYFISH_API_KEY")},
                        engine) == ()                                               # perplexity alone is no routing
    monkeypatch.setattr(sr, "SHIPPED", True)
    assert sr.available(KEYS, engine) == ALL                                        # auto, shipped
    assert sr.available({**KEYS, "EVE_SEARCH_ROUTER": "off"}, engine) == ()   # off beats shipped
    assert sr.mode({"EVE_SEARCH_ROUTER": "sometimes"}) == "auto"


# ---- the routed search ----------------------------------------------------------------------------------------

PAGES = [{"url": "https://hours.example/exploratorium", "title": "Hours", "markdown": "Sunday 10am-5pm."}]


def model(*replies: dict[str, Any]) -> Any:
    sent: list[dict[str, Any]] = []

    def ask(body: dict[str, Any]) -> dict[str, Any]:
        sent.append(body)
        return {"choices": [{"message": {"content": json.dumps(replies[len(sent) - 1])}}],
                "usage": {"prompt_tokens": 800, "completion_tokens": 30, "cost": 0.0002}}
    ask.sent = sent  # type: ignore[attr-defined]
    return ask


def engine(answer: str = "Perplexity says 10 to 5.") -> Any:
    calls: list[tuple[str, str, str]] = []

    def run(query: str, system: str, feature: str) -> dict[str, Any]:
        calls.append((query, system, feature))
        return {"ok": True, "answer": answer, "sources": [{"title": "S", "url": "https://s.example/a", "snippet": ""}],
                "usage": {"in": 1, "out": 1, "cost": 0.006}, "ms": 900, "cost_usd": 0.006}
    run.calls = calls  # type: ignore[attr-defined]
    return run


CFG = websearch.SearchConfig(engine="openrouter-perplexity", providers=ALL)


def to(provider: str) -> Any:
    return lambda text, available: sr.Route(provider, 0.97, 180.0)


def test_a_routed_quick_read_answers_in_the_brains_result_shape(caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level("INFO")
    per = engine()
    paths = sr.Paths(perplexity=per, read=lambda q: {"pages": PAGES},
                     ask=model({"answered": True, "answer": "Open 10am to 5pm on Sunday.", "sources": [1]}))
    out = sr.answer("secret query words", CFG, paths=paths, route=to(sr.TINYFISH), clock=iter([0.0, 1.2]).__next__)
    assert set(out) == {"ok", "answer", "sources", "usage", "ms", "cost_usd"}
    assert out["answer"] == "Open 10am to 5pm on Sunday." and out["ms"] == 1200
    assert out["sources"] == [{"title": "Hours", "url": "https://hours.example/exploratorium",
                               "snippet": "Sunday 10am-5pm."}]
    assert out["usage"] == {"in": 800, "out": 30, "cost": 0.0002} and out["cost_usd"] == 0.0002
    assert per.calls == []
    assert "search router: tinyfish (p=0.97, jev 180 ms)" in caplog.text and "secret" not in caplog.text


@pytest.mark.parametrize("read", [
    lambda q: {"pages": []},
    lambda q: (_ for _ in ()).throw(watch.FetchError("TinyFish: the site answered HTTP 429", 429)),
    lambda q: (_ for _ in ()).throw(RuntimeError("boom")),
])
def test_a_routed_provider_that_fails_is_followed_by_one_perplexity_search(read: Any) -> None:
    per = engine()
    paths = sr.Paths(perplexity=per, read=read, ask=model({"answered": True, "answer": "x", "sources": []}))
    out = sr.answer("q", CFG, paths=paths, route=to(sr.TINYFISH))
    assert out["ok"] and out["answer"] == "Perplexity says 10 to 5." and len(per.calls) == 1
    assert per.calls[0][1] == websearch.SYSTEM and per.calls[0][2] == spend.SEARCH


def test_a_pages_non_answer_is_followed_by_perplexity_too() -> None:
    per = engine()
    paths = sr.Paths(perplexity=per, read=lambda q: {"pages": PAGES},
                     ask=model({"answered": False, "answer": "", "sources": []}))
    assert sr.answer("q", CFG, paths=paths, route=to(sr.TINYFISH))["answer"] == "Perplexity says 10 to 5."


def test_a_router_that_fails_or_names_a_missing_provider_is_perplexity(caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level("INFO")
    per = engine()

    def broken(text: str, available: Any) -> Any:
        raise RuntimeError("jev down")

    assert sr.answer("q", CFG, paths=sr.Paths(perplexity=per), route=broken)["ok"]
    only_tf = websearch.SearchConfig(engine="openrouter-perplexity", providers=(sr.PERPLEXITY, sr.TINYFISH))
    assert sr.answer("q", only_tf, paths=sr.Paths(perplexity=per), route=to(sr.FIRECRAWL))["ok"]
    assert len(per.calls) == 2 and "firecrawl is not available" in caplog.text


def test_routing_off_is_the_engine_for_the_brain_and_tinyfish_for_the_reader() -> None:
    per = engine()
    read: list[str] = []
    paths = sr.Paths(perplexity=per, read=lambda q: read.append(q) or {"pages": []},
                     ask=model({"answered": True, "answer": "x", "sources": []}))

    def never(text: str, available: Any) -> Any:
        raise AssertionError("routing is off: Jev is never asked")

    off = websearch.SearchConfig(engine="openrouter-perplexity")
    assert sr.answer("q", off, paths=paths, route=never)["ok"] and read == []
    out = sr.answer("q", off, reader=True, paths=paths, route=never)
    assert out == {"ok": False, "reason": "TinyFish found no readable pages"} and read == ["q"]
    assert len(per.calls) == 1                                                     # no perplexity for the reader


def test_links_the_search_did_not_read_are_removed_whichever_provider_answered() -> None:
    per = engine("Book at https://s.example/a or https://evil.example/pay now.")
    out = sr.answer("q", CFG, paths=sr.Paths(perplexity=per), route=to(sr.PERPLEXITY))
    assert out["answer"] == "Book at https://s.example/a or (link removed) now."


# ---- the time budget, the Firecrawl cap, the cost ---------------------------------------------------------------

def test_a_slow_routed_provider_is_abandoned_at_its_budget_and_perplexity_answers(
        caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level("INFO")
    per = engine()
    release, stopped = threading.Event(), threading.Event()

    def slow_read(q: str) -> dict[str, Any]:              # a reader that hangs past any per-call timeout
        release.wait(5.0)
        return {"pages": PAGES}

    def ask(body: dict[str, Any]) -> dict[str, Any]:       # reached only if the worker ignored being abandoned
        stopped.set()
        return {"choices": [{"message": {"content": "{}"}}]}

    cfg = websearch.SearchConfig(engine="openrouter-perplexity", providers=ALL, routed_secs=0.05)
    t0 = time.monotonic()
    out = sr.answer("q", cfg, paths=sr.Paths(perplexity=per, read=slow_read, ask=ask), route=to(sr.TINYFISH))
    waited = time.monotonic() - t0
    assert out["ok"] and out["answer"] == "Perplexity says 10 to 5." and len(per.calls) == 1
    assert waited < 2.0                                                   # the budget, not the reader's hang
    assert "tinyfish did not answer (no answer within 0.05 s); perplexity takes it" in caplog.text
    release.set()                                                         # the abandoned worker wakes, sees it
    time.sleep(0.1)                                                       # was dropped, and stops before its
    assert not stopped.is_set()                                           # model call


def test_the_budget_is_the_brains_setting_and_longer_but_bounded_for_the_reader(
        monkeypatch: pytest.MonkeyPatch) -> None:
    budgets: list[float] = []
    real = sr.bounded

    def spy(work: Any, budget: float) -> dict[str, Any]:
        budgets.append(budget)
        return real(work, budget)

    monkeypatch.setattr(sr, "bounded", spy)
    per = engine()
    paths = sr.Paths(perplexity=per, read=lambda q: {"pages": []})
    cfg = websearch.SearchConfig(engine="openrouter-perplexity", providers=ALL)
    sr.answer("q", cfg, paths=paths, route=to(sr.TINYFISH))
    sr.answer("q", cfg, reader=True, paths=paths, route=to(sr.TINYFISH))
    sr.answer("q", cfg, paths=paths, route=to(sr.PERPLEXITY))            # perplexity: its own timeout, unwrapped
    assert budgets == [websearch.DEFAULT_ROUTED_SECS, sr.READER_ROUTED_SECS]
    assert websearch.DEFAULT_ROUTED_SECS == 10.0 and sr.READER_ROUTED_SECS < 120
    assert websearch.configured({"EVE_SEARCH_ROUTED_SECS": "4"}).routed_secs == 4.0
    assert websearch.configured({"EVE_SEARCH_ROUTED_SECS": "600"}).routed_secs == 60.0
    assert websearch.configured({"EVE_SEARCH_ROUTED_SECS": "soon"}).routed_secs == 10.0


def test_the_firecrawl_cap_is_one_count_for_both_surfaces_per_local_day(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("FIRECRAWL_API_KEY", "fc-test")
    sent: list[str] = []
    monkeypatch.setattr(watch, "http_request", lambda url, **k: sent.append(url) or (200, json.dumps(
        {"success": True, "creditsUsed": 1, "data": {"web": [], "alexandria": [{"data": {}, "creditsCost": 1}]}})))
    assert firecrawl.per_day({}) == 5 and firecrawl.per_day({"EVE_FIRECRAWL_PER_DAY": "x"}) == 5
    monkeypatch.setenv("EVE_FIRECRAWL_PER_DAY", "3")
    firecrawl.search("a")                                                # the brain's search
    firecrawl.run_tool({"provider": "p", "capability": "c", "options": {}})   # a data tool: a call too
    firecrawl.search("b")                                                # the web reader's
    assert sent == [firecrawl.SEARCH_URL, firecrawl.TOOL_URL, firecrawl.SEARCH_URL] and firecrawl.left() == 0
    with pytest.raises(watch.FetchError, match="today's 3 calls are used up") as e:
        firecrawl.search("d")                                            # never sent
    assert len(sent) == 3 and e.value.host == firecrawl.HOST
    tomorrow = time.time() + 86400
    assert firecrawl.left(now=tomorrow) == 3 and firecrawl.take(now=tomorrow)


def test_a_spent_firecrawl_day_routes_to_perplexity_with_the_reason_logged(caplog: pytest.LogCaptureFixture,
                                                                        monkeypatch: pytest.MonkeyPatch) -> None:
    caplog.set_level("INFO")
    monkeypatch.setenv("EVE_FIRECRAWL_PER_DAY", "1")
    assert firecrawl.take()                                              # the reader spent today's one call
    per = engine()

    def never(*a: Any, **k: Any) -> Any:
        raise AssertionError("the cap is spent: Firecrawl is never called")

    out = sr.answer("q", CFG, paths=sr.Paths(perplexity=per, firecrawl_search=never), route=to(sr.FIRECRAWL))
    assert out["ok"] and len(per.calls) == 1
    assert "search router: perplexity (p=0.97, jev 180 ms, today's 1 Firecrawl calls are used up)" in caplog.text


FC_PAGES = {"pages": PAGES, "tools": [], "credits": 4}
ANSWERED = {"answered": True, "answer": "Open 10am to 5pm on Sunday.", "sources": [1]}


def test_cost_usd_is_a_number_only_when_every_part_is_priced(monkeypatch: pytest.MonkeyPatch) -> None:
    def unpriced_model(body: dict[str, Any]) -> dict[str, Any]:
        return {"choices": [{"message": {"content": json.dumps(ANSWERED)}}],
                "usage": {"prompt_tokens": 800, "completion_tokens": 30}}           # no cost reported

    def fc(ask: Any, found: dict[str, Any] = FC_PAGES) -> dict[str, Any]:
        return sr.answer("q", CFG, paths=sr.Paths(perplexity=engine(), firecrawl_search=lambda q, **k: found,
                                                  ask=ask), route=to(sr.FIRECRAWL))

    monkeypatch.delenv("EVE_FIRECRAWL_USD", raising=False)
    out = fc(model(ANSWERED))
    assert set(out) == {"ok", "answer", "sources", "usage", "ms", "cost_usd"}             # the same shape
    assert out["cost_usd"] is None                        # credits with no price: unpriced, not the model's share
    monkeypatch.setenv("EVE_FIRECRAWL_USD", "0.004")
    assert fc(model(ANSWERED))["cost_usd"] == pytest.approx(0.0002 + 4 * 0.004)
    assert fc(unpriced_model)["cost_usd"] is None                            # the answer model reported none
    assert fc(model(ANSWERED), {**FC_PAGES, "credits": None})["cost_usd"] is None   # Firecrawl did not say
    tf = sr.answer("q", CFG, paths=sr.Paths(perplexity=engine(), read=lambda q: {"pages": PAGES},
                                            ask=unpriced_model), route=to(sr.TINYFISH))
    assert set(tf) == set(out) and tf["cost_usd"] is None and tf["usage"] == {"in": 800, "out": 30}


# ---- the fixtures ----------------------------------------------------------------------------------------------

def test_the_tuning_set_is_labelled_and_mixed() -> None:
    data = json.loads((ROUTES / "search_tuning.json").read_text(encoding="utf-8"))
    cases = data["cases"]
    assert len(cases) >= 60 and data.get("reader_note")
    assert all(c["provider"] in ALL and c["goal"].strip() for c in cases)
    counts = Counter(c["provider"] for c in cases)
    assert 0.3 <= counts[sr.TINYFISH] / len(cases) <= 0.5 and 0.3 <= counts[sr.PERPLEXITY] / len(cases) <= 0.5
    assert 0.12 <= counts[sr.FIRECRAWL] / len(cases) <= 0.3
    assert sum(1 for c in cases if c.get("tricky")) >= 10
    goals = [c["goal"].lower() for c in cases]
    assert len(goals) == len(set(goals))
    holdout = ROUTES / "search_holdout.json"
    if holdout.exists():                                                  # the blind set repeats no tuning request
        blind = {c["goal"].lower() for c in json.loads(holdout.read_text(encoding="utf-8"))["cases"]}
        assert not blind & set(goals)


def _route_eval(monkeypatch: pytest.MonkeyPatch) -> Any:
    import importlib.util

    spec = importlib.util.spec_from_file_location("route_eval", Path(__file__).parents[1] / "tools" / "route_eval.py")
    assert spec and spec.loader
    route_eval = importlib.util.module_from_spec(spec)
    monkeypatch.setitem(sys.modules, "route_eval", route_eval)      # its dataclasses look their module up
    spec.loader.exec_module(route_eval)
    return route_eval


def test_the_eval_fits_on_every_seen_set_and_scores_the_fresh_holdout_once(
        tmp_path: Path, capsys: pytest.CaptureFixture, monkeypatch: pytest.MonkeyPatch) -> None:
    route_eval = _route_eval(monkeypatch)
    assert route_eval.SEARCH_SEEN == ("search_tuning.json", "search_holdout.json")
    assert route_eval.SEARCH_BLIND == "search_holdout2.json"
    cases = [{"goal": f"hours {i}", "provider": sr.TINYFISH} for i in range(16)] + [
        {"goal": f"fare {i}", "provider": sr.FIRECRAWL} for i in range(4)] + [
        {"goal": f"news {i}", "provider": sr.PERPLEXITY} for i in range(6)]
    for name in ("t.json", "h1.json", "h2.json"):
        (tmp_path / name).write_text(json.dumps({"cases": cases}))
    asked: list[str] = []

    def predict(state: Any, qs: Any) -> dict[str, Any]:
        text = state["request"]
        asked.append(text)
        if text.startswith("hours"):
            return jev(0.97, 0.01, 0.02, 0.05, 0.97)
        if text.startswith("fare"):
            return jev(0.01, 0.97, 0.97, 0.1, 0.1)
        return jev(0.3, 0.01, 0.02, 0.9, 0.3, changes=0.9)

    assert route_eval.search_eval(tmp_path, ("t.json", "h1.json"), "h2.json", predict=predict) == 0
    assert len(asked) == 3 * len(cases)                              # two seen sets fitted, the blind one scored
    out = capsys.readouterr().out
    assert "fitted on 52 seen requests" in out
    assert "routed to tinyfish: 16, right 16" in out and "routed to firecrawl: 4, right 4" in out
    assert "SHIPPED=True" in out and "SEARCH DECISION: ship" in out and "SEARCH_EVAL_COMPLETE" in out


def test_the_printed_verdict_is_exactly_the_pre_registered_bar(monkeypatch: pytest.MonkeyPatch) -> None:
    bar = _route_eval(monkeypatch).search_bar
    tf, fc, pp = sr.TINYFISH, sr.FIRECRAWL, sr.PERPLEXITY
    # 20 tinyfish routes, one of them wrong: 95% precision passes (c); a wrong tinyfish route is inside the bar
    ok = bar([tf] * 20 + [pp], [tf] * 19 + [pp, pp])
    assert ok["shipped"] and ok["routed"] == 20 and ok["tinyfish_precision"] == pytest.approx(0.95)
    assert not bar([tf] * 19 + [pp], [tf] * 19 + [pp])["shipped"]                          # (a) 19 routed
    fc_wrong = bar([tf] * 20 + [fc], [tf] * 20 + [pp])
    assert not fc_wrong["shipped"] and not fc_wrong["b"] and fc_wrong["a"] and fc_wrong["c"]   # (b)
    low = bar([tf] * 20, [tf] * 18 + [pp, fc])
    assert not low["shipped"] and not low["c"] and low["a"] and low["b"]                     # (c) 90%
    assert bar([fc] * 20, [fc] * 20)["shipped"]                        # no tinyfish routes: nothing imprecise


def test_the_firecrawl_cap_survives_a_restart(tmp_path: Any, monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge import spend

    monkeypatch.setattr(spend, "spend_dir", lambda: tmp_path)
    monkeypatch.setattr(firecrawl, "_CALLS", {})
    monkeypatch.setattr(firecrawl, "_LOADED", set())
    env = {"EVE_FIRECRAWL_PER_DAY": "3"}
    now = 1.8e9
    assert firecrawl.take(env, now) and firecrawl.take(env, now) and firecrawl.left(env, now) == 1
    monkeypatch.setattr(firecrawl, "_CALLS", {})                        # the daemon restarts the same day
    monkeypatch.setattr(firecrawl, "_LOADED", set())
    assert firecrawl.left(env, now) == 1                                # the day's two calls were kept
    assert firecrawl.take(env, now) and not firecrawl.take(env, now)
    monkeypatch.setattr(firecrawl, "_CALLS", {})
    monkeypatch.setattr(firecrawl, "_LOADED", set())
    assert firecrawl.left(env, now + 86400) == 3                        # the next day starts fresh
    (tmp_path / firecrawl.COUNT_FILE).write_text("not json")
    monkeypatch.setattr(firecrawl, "_LOADED", set())
    assert firecrawl.left(env, now) == 3                                # an unreadable file counts as nothing


def test_a_tool_that_needs_an_id_looked_up_first_is_not_offered() -> None:
    lookup = {"provider": "p", "capability": "flights/search_flights", "name": "Search", "options": [
        {"name": "destination_entity_id", "type": "string", "about": "Destination entity id.", "required": True}]}
    plain = {"provider": "g", "capability": "flights/search_flights", "name": "Search", "options": [
        {"name": "origin", "type": "string", "required": True,
         "about": "Airport IATA code or Google city id returned by suggest_places."},   # "SFO" fills it
        {"name": "departure_date", "type": "string", "about": "YYYY-MM-DD", "required": True},
        {"name": "selection_id", "type": "string", "about": "a chosen outbound id", "required": False}]}
    assert firecrawl._tool(lookup) is None
    assert firecrawl._tool(plain) is not None                           # an optional id does not matter
