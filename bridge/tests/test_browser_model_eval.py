"""tools/browser_model_eval.py: the pure parts — task checks, URL routing, costing, outcome, aggregation, the pick.

No model, no browser: the checks read a fake fixture-state snapshot, the report reads fake rows. One test
serves the fixture sites on 127.0.0.1 (loopback only) to prove a POST lands in the state the checks read.
"""

from __future__ import annotations

import json
import urllib.request

import browser_model_eval as bme  # tools/ is on the pytest path (pyproject.toml)
import pytest


def _state(posts=(), visits=()):
    return {"visits": list(visits), "posts": [{"path": p, "data": d} for p, d in posts]}


# ---- task checks: one right state passes, the near misses do not --------------------------------------------

def test_contact_check_wants_exactly_one_right_submission():
    good = {"name": "Ada Lovelace", "email": "ada@example.com", "message": "Please call me back"}
    assert bme.check_contact("", _state([("forms/api/contact", good)]))
    assert not bme.check_contact("", _state())
    assert not bme.check_contact("", _state([("forms/api/contact", {**good, "email": "ada@example.org"})]))
    assert not bme.check_contact("", _state([("forms/api/contact", good)] * 2))      # sent twice is a failure


def test_cart_check_rejects_a_look_alike_or_an_extra_item():
    assert bme.check_cart("", _state([("shop/api/cart", {"item": "Blue Ceramic Mug"})]))
    assert not bme.check_cart("", _state([("shop/api/cart", {"item": "Blue Glass Mug"})]))
    assert not bme.check_cart("", _state([("shop/api/cart", {"item": "Blue Ceramic Mug"}),
                                          ("shop/api/cart", {"item": "Red Ceramic Mug"})]))


def test_search_link_check_needs_the_search_and_the_right_article():
    assert bme.check_search_link("", _state(visits=["docs/", "docs/search", "docs/article/solar-panel-guide"]))
    assert not bme.check_search_link("", _state(visits=["docs/article/solar-panel-guide"]))          # no search
    assert not bme.check_search_link("", _state(visits=["docs/search", "docs/article/solar-panel-guide-v1"]))


def test_settings_check_wants_digest_on_and_nothing_else_changed():
    ok = {"push": True, "digest": True, "sms": False, "share": False}
    assert bme.check_settings("", _state([("settings/api/settings", ok)]))
    assert not bme.check_settings("", _state())                                                    # never saved
    assert not bme.check_settings("", _state([("settings/api/settings", {**ok, "sms": True})]))


def test_recipe_below_fold_and_reservation_checks():
    assert bme.check_recipe("", _state(visits=["recipes/r/lemon-tart"]))
    assert not bme.check_recipe("", _state(visits=["recipes/r/lemon-drizzle"]))
    assert bme.check_below_fold("", _state([("pricing/api/compare", {})]))
    assert not bme.check_below_fold("", _state())
    res = {"party": "4", "day": "Friday", "time": "7:00 pm", "name": "Grace Hopper"}
    assert bme.check_reservation("", _state([("bistro/api/reserve", res)]))
    assert not bme.check_reservation("", _state([("bistro/api/reserve", {**res, "time": "8:00 pm"})]))


def test_answer_checks_are_tolerant_but_refuse_the_distractor():
    table = next(t for t in bme.TASKS if t.id == "table_read").check
    assert table("Riverton has 48,213 people.", {})
    assert table("About 48213.", {})
    assert not table("Riverton Heights has 12,870; Riverton has 48,213.", {})   # hedging with the wrong row
    ship = next(t for t in bme.TASKS if t.id == "help_read_after_nav").check
    assert ship("Standard shipping takes 5 to 7 business days.", {})
    assert ship("5-7 business days", {})
    assert not ship("Express shipping takes 2 business days.", {})
    depth = next(t for t in bme.TASKS if t.id == "wikipedia_fact").check
    assert depth("About 10,935 metres.", {}) and depth("roughly 10994 m", {}) and not depth("8,848 m", {})


def test_the_task_set_shape():
    ids = [t.id for t in bme.TASKS]
    assert len(ids) == len(set(ids)) == 12
    assert sum(t.where == "public" for t in bme.TASKS) == 3
    for t in bme.TASKS:
        if t.where == "local":
            # every local goal names its https fixture site: the plan contract only opens clean https URLs
            assert f".{bme.FIXTURE_DOMAIN}" in t.goal
            assert not t.start or bme.local_url(t.start, 1) is not None


# ---- routing, costing, outcome -----------------------------------------------------------------------------

def test_local_url_maps_only_fixture_hosts():
    assert bme.local_url("https://docs.eve-eval.test/search?q=solar+panels", 8123) == \
        "http://127.0.0.1:8123/docs/search?q=solar+panels"
    assert bme.local_url("https://shop.eve-eval.test", 9) == "http://127.0.0.1:9/shop/"
    assert bme.local_url("https://example.com/", 9) is None
    assert bme.local_url("https://eve-eval.test.evil.com/", 9) is None


def test_call_cost_prefers_openrouter_cost_then_the_table_then_none():
    usage = {"input_tokens": 1000, "output_tokens": 200, "input_tokens_details": {"cached_tokens": 400}}
    assert bme.call_cost("z-ai/glm-5.3-flash", {**usage, "cost": 0.00042}) == 0.00042
    # astra: 600 uncached × $10 + 400 cached × $1 + 200 out × $50, per million
    assert bme.call_cost("gpt-6-astra", usage) == pytest.approx((600 * 10 + 400 * 1 + 200 * 50) / 1e6)
    assert bme.call_cost("some/unknown-model", usage) is None


def test_openai_models_go_direct_everything_else_through_openrouter():
    assert bme.is_openai_direct("gpt-6-sol")
    assert not bme.is_openai_direct("google/gemini-3.7-flash")


def test_outcome_of():
    assert bme.outcome_of("Done.", "") == "finished"
    assert bme.outcome_of("", "\n\n[note] a plan already did …") == "part_done"
    assert bme.outcome_of("", "") == "nothing"


# ---- the report ----------------------------------------------------------------------------------------------

def _row(model, task, success, outcome="finished", secs=5.0, usd=0.01, jev=1, errors=()):
    return {"model": model, "task": task, "success": success, "outcome": outcome, "secs": secs, "planner_usd": usd,
            "jev_usd": 0.0001, "jev_calls": jev, "planner_errors": list(errors)}


def test_aggregate_counts_handoffs_and_averages():
    rows = [_row("a", "table_read", True), _row("a", "cookie_banner", False, "part_done", secs=3.0, usd=0.02),
            _row("a", "below_the_fold", False, "nothing", secs=1.0, usd=0.03, errors=["BadRequestError: x"])]
    s = bme.aggregate(rows)["a"]
    assert (s["runs"], s["success"], s["handoffs"], s["part_done"], s["nothing"]) == (3, 1, 2, 1, 1)
    assert s["success_rate"] == pytest.approx(1 / 3, abs=1e-4)
    assert s["avg_secs"] == 3.0 and s["avg_planner_usd"] == pytest.approx(0.02)
    assert s["api_errors"] == ["BadRequestError: x"] and s["by_task"]["table_read"] == [1, 1]


def test_an_unpriced_run_makes_the_model_unpriced_not_cheap():
    s = bme.aggregate([_row("m", "table_read", True, usd=None), _row("m", "table_read", True, usd=0.001)])["m"]
    assert s["priced"] is False


def test_recommend_picks_the_cheapest_within_tolerance_of_the_baseline():
    rows = ([_row("gpt-6-astra", "t", i < 8, usd=0.02) for i in range(10)]
            + [_row("cheap", "t", i < 8, usd=0.001) for i in range(10)]          # same success, 20x cheaper
            + [_row("cheapest", "t", i < 5, usd=0.0001) for i in range(10)])     # 30 points worse
    assert bme.recommend(bme.aggregate(rows)) == "cheap"
    rows_bad = [_row("gpt-6-astra", "t", True, usd=0.02)] * 10 + [_row("cheap", "t", i < 7, usd=0.001) for i in range(10)]
    assert bme.recommend(bme.aggregate(rows_bad)) == "gpt-6-astra"
    assert bme.recommend({}) == ""


def test_markdown_has_the_table_and_the_per_task_grid():
    rows = [_row("gpt-6-astra", "table_read", True), _row("gpt-6-luna", "table_read", False, "part_done")]
    summary = bme.aggregate(rows)
    md = bme.markdown(summary, rows, bme.recommend(summary))
    assert md.splitlines()[0] == "| model | success rate | avg time | avg cost per task (planner) | handoffs | notes |"
    assert "| gpt-6-astra | 1/1 (100%) |" in md and "| table_read | 1/1 | 0/1 |" in md


# ---- the fixture server, on loopback --------------------------------------------------------------------------

def test_fixture_server_records_visits_and_posts():
    state = bme.FixtureState()
    srv, port = bme.serve_fixtures(state)
    try:
        page = urllib.request.urlopen(f"http://127.0.0.1:{port}/shop/", timeout=5).read().decode()
        assert "Blue Ceramic Mug" in page
        req = urllib.request.Request(f"http://127.0.0.1:{port}/shop/api/cart", method="POST",
                                     data=json.dumps({"item": "Blue Ceramic Mug"}).encode(),
                                     headers={"Content-Type": "application/json"})
        assert json.loads(urllib.request.urlopen(req, timeout=5).read())["count"] == 1
        snap = state.snapshot()
        assert snap["visits"] == ["shop/"]
        assert bme.check_cart("", snap)
        with pytest.raises(urllib.error.HTTPError):
            urllib.request.urlopen(f"http://127.0.0.1:{port}/shop/nope", timeout=5)
    finally:
        srv.shutdown()


def test_lane_trace_reads_the_executor_ledger_and_plan_errors(tmp_path):
    log = tmp_path / "run.jsonl"
    log.write_text("\n".join(json.dumps(x) for x in (
        {"goal": "g", "model": "m"},
        {"turn": 0, "plan": {"error": "PlanError: step 1: open_url needs a clean https URL"}},
        {"turn": 0, "ledger": {"status": "partial", "reason": "ambiguous_field",
                               "ledger": [{"effect": "confirmed", "note": "clicked"}, {"effect": "refused", "note": "x"}]}},
    )) + "\nnot json\n")
    assert bme.lane_trace(str(log)) == [
        {"status": "plan_error", "reason": "PlanError: step 1: open_url needs a clean https URL"},
        {"status": "partial", "reason": "ambiguous_field", "last_effect": "refused", "last_note": "x"}]
    assert bme.lane_trace(str(tmp_path / "missing.jsonl")) == [] and bme.lane_trace("") == []
