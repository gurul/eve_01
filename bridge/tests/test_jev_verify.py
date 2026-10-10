"""jev_verify: Jev walks the maker's journeys on the real page and judges what the screen shows.

The page is real (app_check's sandbox in headless Chromium); Jev is faked by a literal reader that answers the
questions jev_verify really sends, so the wiring, the gates and the failure paths are tested with no network.
The cut-offs themselves are measured by tools/journey_eval.py against the real Jev (numbers beside the constants).
"""

from __future__ import annotations

import asyncio
import json
from typing import Any

import pytest

from eve_bridge import jev_verify as jv
from eve_bridge.typed_ask import StepAnswer

pytest.importorskip("playwright.async_api")

from eve_bridge.app_check import check_app  # noqa: E402

# A small expense splitter: add a person, add an expense, see who owes whom.
SPLITTER = """<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>Splitter</title>
<style>body{font:16px system-ui;margin:0;padding:16px} button,input,select{min-height:44px;font-size:16px}</style>
</head><body>
<h1>Splitter</h1>
<label for="who">Name</label><input id="who" placeholder="e.g. Alex">
<button id="addp">Add person</button>
<label for="amt">Amount</label><input id="amt" type="number" placeholder="e.g. 30">
<label for="payer">Paid by</label><select id="payer"></select>
<button id="adde">Add expense</button>
<ul id="people"></ul><p id="bal"></p>
<script>
let s = { v: 1, people: [], exp: [] };
const $ = (i) => document.getElementById(i);
function render() {
  $("people").innerHTML = ""; $("payer").innerHTML = "";
  for (const p of s.people) { const li = document.createElement("li"); li.textContent = p; $("people").appendChild(li);
    const o = document.createElement("option"); o.textContent = p; $("payer").appendChild(o); }
  const total = s.exp.reduce((a, e) => a + e.amt, 0), share = s.people.length ? total / s.people.length : 0;
  $("bal").textContent = s.exp.length ? s.people.map((p) => p + " balance " +
    (s.exp.filter((e) => e.by === p).reduce((a, e) => a + e.amt, 0) - share).toFixed(2)).join(", ") : "No expenses yet";
}
$("addp").onclick = () => { const n = $("who").value.trim(); if (!n) return; s.people.push(n); $("who").value = "";
  render(); eve.save(s); };
$("adde").onclick = () => { /*ADD*/ s.exp.push({ amt: Number($("amt").value), by: $("payer").value }); render(); eve.save(s); };
(async () => { s = (await eve.load()) || s; render(); })();
</script></body></html>"""

BROKEN = SPLITTER.replace("/*ADD*/", "return;")      # the negative control: Add expense does nothing

JOURNEYS = [
    {"name": "Split a dinner", "steps": [
        {"do": "type", "target": "Name", "value": "Alex"}, {"do": "tap", "target": "Add person"},
        {"do": "type", "target": "Name", "value": "Sam"}, {"do": "tap", "target": "Add person"},
        {"do": "type", "target": "Amount", "value": "30"}, {"do": "choose", "target": "Paid by", "value": "Sam"},
        {"do": "tap", "target": "Add expense"}],
     "expect": ["Alex balance -15.00, Sam balance 15.00"]},
]


class LiteralJev:
    """Answers jev_verify's two questions the way a literal reader would, and counts the calls."""

    def __init__(self) -> None:
        self.calls = 0

    def __call__(self, state: dict[str, Any], questions: dict[str, Any]) -> dict[str, Any]:
        self.calls += 1
        if "target" in questions:
            label = questions["present"]["instructions"].split('"')[1].casefold()
            hits = [k for k, v in questions["target"]["criteria"].items()
                    if k != jv.STEP_NONE and v.split(" (")[0].casefold() == label]
            key = hits[0] if hits else jv.STEP_NONE
            return {"answers": {"target": {"choice": key, "probabilities": {key: 0.99}},
                                "present": {"noul": 0.97 if hits else 0.03}}, "usage": {"input_tokens": 500}}
        screen = " ".join(state["screen"])
        return {"answers": {q: {"noul": 0.96 if v["instructions"].split("show: ")[1].split(" ?")[0] in screen else 0.02}
                            for q, v in questions.items()}, "usage": {"input_tokens": 300}}


def journeys() -> list[jv.Journey]:
    found, problems = jv.parse_journeys(JOURNEYS)
    assert problems == []
    return found


def test_a_working_app_passes_its_journey_and_the_summary_says_so() -> None:
    fake = LiteralJev()
    r = asyncio.run(check_app(SPLITTER, journeys=journeys(), verifier=jv.Verifier(fake)))
    assert r.journeys is not None and r.journeys.passed == 1, r.journeys.as_dict()
    assert "journeys 1/1" in r.summary()
    assert not any(i.startswith("Journey") for i in r.issues)
    assert r.journeys.jev["calls"] == fake.calls == 8 and r.journeys.jev["input_tokens"] == 7 * 500 + 300


def test_the_broken_add_is_caught_as_an_expectation_not_shown_with_evidence() -> None:
    r = asyncio.run(check_app(BROKEN, journeys=journeys(), verifier=jv.Verifier(LiteralJev())))
    assert r.journeys is not None and r.journeys.passed == 0 and not r.ok
    res = r.journeys.results[0]
    assert res.status == "expect" and "No expenses yet" in res.evidence
    assert r.issues[0].startswith('Journey "Split a dinner"') and "Alex balance -15.00" in r.issues[0]
    assert "journeys 0/1" in r.summary()


def test_a_step_whose_control_is_not_there_fails_at_that_step_and_says_what_is() -> None:
    wrong = [{**JOURNEYS[0], "steps": [{"do": "tap", "target": "Add participant"}]}]
    found, _ = jv.parse_journeys(wrong)
    r = asyncio.run(check_app(SPLITTER, journeys=found, verifier=jv.Verifier(LiteralJev())))
    res = r.journeys.results[0]
    assert res.status == "step" and res.step == 1
    assert 'could not find "Add participant"' in res.reason and '"Add person"' in res.reason


def test_jev_down_never_fails_the_app_and_the_journeys_are_not_run() -> None:
    def down(state: Any, questions: Any) -> Any:
        raise TimeoutError("timed out")

    r = asyncio.run(check_app(BROKEN, journeys=journeys(), verifier=jv.Verifier(down)))
    assert r.journeys.not_run.startswith("Jev did not answer") and r.journeys.results == []
    assert not any(i.startswith("Journey") for i in r.issues)
    assert "journeys not run (Jev did not answer" in r.summary()
    r2 = asyncio.run(check_app(SPLITTER, journeys=journeys(), verifier=None))
    assert r2.journeys.not_run == "no verifier" and not any(i.startswith("Journey") for i in r2.issues)


def test_no_journeys_is_the_scripted_check_alone() -> None:
    r = asyncio.run(check_app(SPLITTER, verifier=jv.Verifier(LiteralJev())))
    assert r.journeys is None and "journeys" not in r.summary()


# ---- pure parts ------------------------------------------------------------------------------------------

def test_journeys_parse_strictly_and_drop_what_cannot_be_walked() -> None:
    raw = [{"name": "ok", "steps": [{"do": "tap", "target": "Add"}], "expect": ["1 item"]},
           {"name": "no value", "steps": [{"do": "type", "target": "Name"}], "expect": ["x"]},
           {"name": "no expect", "steps": [{"do": "tap", "target": "Add"}], "expect": []},
           {"name": "bad kind", "steps": [{"do": "swipe", "target": "x"}], "expect": ["x"]}]
    found, problems = jv.parse_journeys(raw)
    assert [j.name for j in found] == ["ok"] and len(problems) == 3
    text = "Here.\n```html\n<html></html>\n```\n```journeys\n" + json.dumps(raw[:1]) + "\n```"
    got, _ = jv.extract_journeys(text)
    assert got is not None and got[0].steps[0] == jv.Step("tap", "Add")
    assert jv.extract_journeys("```html\nx\n```") == (None, [])
    assert jv.extract_journeys("```journeys\n[oops\n```")[0] == []


def test_the_fitted_gates_act_only_when_the_noul_and_the_choice_both_clear() -> None:
    g = jv.JOURNEY_STEP_GATES
    sure = StepAnswer("c1", 0.99, 0.98, 0.95, 0.0, 0.0)
    assert g.decide(sure) == "c1"
    assert g.decide(StepAnswer("c1", 0.99, 0.98, g.present - 0.01, 0.0, 0.0)) == jv.STEP_NONE   # choice alone
    assert g.decide(StepAnswer("none", 0.99, 0.98, 0.95, 0.0, 0.0)) == jv.STEP_NONE
    assert 0 < jv.EXPECT_SHOWN < 1


def test_the_step_question_names_the_label_and_the_state_holds_the_controls() -> None:
    step = jv.Step("type", "Amount", "30")
    screen = jv.Screen((jv.Control("0", "field", "Amount", type="number", placeholder="e.g. 30"),
                        jv.Control("1", "button", "Add expense"), jv.Control("main", "main", "Save")), "H", ("H",))
    by_id = jv.offered(step, screen)
    assert [c.label for c in by_id.values()] == ["Amount"]          # a type step is offered fields only
    q = jv.step_questions(step, {k: c.render() for k, c in by_id.items()})
    assert '"Amount"' in q["target"]["instructions"] and q["present"]["type"] == "noul"
    assert set(q["target"]["criteria"]) == {"c1", jv.STEP_NONE}
    assert jv.Control("main", "main", "Save").render().startswith("Save (Telegram's main button")


def test_the_screen_text_is_short_distinct_lines() -> None:
    lines = jv.screen_lines("A\n\n  A  \nB   c\n" + "\n".join(f"n{i}" for i in range(400)))
    assert lines[:2] == ("A", "B c") and len(lines) <= jv.MAX_LINES


def test_from_env_needs_a_key_and_can_be_turned_off() -> None:
    assert jv.from_env({"EVE_JEV_ROUTE": "openrouter"})[0] is None
    assert "turned off" in jv.from_env({"EVE_APP_JOURNEYS": "0", "OPENROUTER_API_KEY": "k"})[1]
    v, why = jv.from_env({"EVE_JEV_ROUTE": "openrouter", "OPENROUTER_API_KEY": "k"})
    assert v is not None and why == ""


def test_jev_reads_a_shortlist_of_the_screen_with_the_near_misses_kept() -> None:
    # a month calendar is dozens of lines of single numbers: on the whole screen Jev missed text that was there
    lines = ("Mood Journal",) + tuple(str(d) for d in range(1, 31)) + ("average mood · Good", "Alex owes Sam", "$15.00",
                                                                      "Settings")
    got = jv.relevant_lines(lines, ("Alex owes Sam $25.00", "average mood · Good"))
    assert "average mood · Good" in got and "$15.00" in got and "Alex owes Sam" in got   # the near miss stays in
    assert len(got) <= jv.MAX_EXPECT_LINES and "Settings" in got                         # a neighbour line too
    assert jv.relevant_lines(("a", "b"), ("zzz",)) == ("a", "b")                         # nothing shared: the start


def test_a_number_jev_did_not_really_see_vetoes_shown() -> None:
    assert jv.numbers_shown("Sam owes $30", ("Sam owes", "$30.00")) is True              # the same amount, two ways
    assert jv.numbers_shown("Total $1,234.50", ("Total $1234.5",)) is True
    assert jv.numbers_shown("1 book", ("Add book", "Braiding Sweetgrass")) is False       # Jev read 0.90 here
    assert jv.numbers_shown("Sam owes $25.00", ("Sam owes $15.00",)) is False
    assert jv.numbers_shown("Saved", ("Saved",)) is True


def test_a_journey_step_never_lands_on_a_control_tagged_on_an_earlier_screen() -> None:
    # two views: the first tags "Open" as 0; after it, the form's field must be what id 0 means, not the hidden button
    page = """<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1">
<title>T</title></head><body><div id="a"><button id="open">Open</button></div>
<div id="b" hidden><label for="n">Name</label><input id="n"><p id="out"></p></div>
<script>
document.getElementById("open").onclick = () => { a.hidden = true; b.hidden = false; };
document.getElementById("n").onchange = (e) => { out.textContent = "Hello " + e.target.value; eve.save({v: 1}); };
eve.load();
</script></body></html>"""
    found, _ = jv.parse_journeys([{"name": "Greet", "steps": [{"do": "tap", "target": "Open"},
                                  {"do": "type", "target": "Name", "value": "Alex"}], "expect": ["Hello Alex"]}])
    r = asyncio.run(check_app(page, journeys=found, verifier=jv.Verifier(LiteralJev())))
    assert r.journeys.passed == 1, r.journeys.as_dict()


def test_the_main_buttons_text_is_on_the_screen_jev_reads() -> None:
    page = """<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1">
<title>T</title></head><body><button id="go">Pick</button><script>
const tg = Telegram.WebApp; eve.load();
document.getElementById("go").onclick = () => { tg.MainButton.setText("Clear 1 checked").show(); eve.save({v: 1}); };
tg.MainButton.onClick(() => {});
</script></body></html>"""
    found, _ = jv.parse_journeys([{"name": "Pick", "steps": [{"do": "tap", "target": "Pick"}],
                                   "expect": ["Clear 1 checked"]}])
    r = asyncio.run(check_app(page, journeys=found, verifier=jv.Verifier(LiteralJev())))
    assert r.journeys.passed == 1, r.journeys.as_dict()


def test_when_jev_goes_down_in_one_journey_the_others_stop_with_the_check() -> None:
    # gather does not stop the other journeys when one raises: they must not go on asking Jev and driving pages
    # after the check returned (the maker's browser stays open, as it does here)
    import time

    from playwright.async_api import async_playwright

    from eve_bridge import app_check

    asked: list[str] = []
    literal = LiteralJev()

    def predict(state: dict[str, Any], questions: dict[str, Any]) -> dict[str, Any]:
        asked.append(str(state.get("step", "")))
        if "Boom" in str(state.get("step", "")):
            raise TimeoutError("timed out")
        time.sleep(0.15)
        return literal(state, questions)

    found, _ = jv.parse_journeys([
        {"name": "Slow", "steps": [{"do": "tap", "target": "Add person"}] * 8, "expect": ["No expenses yet"]},
        {"name": "Boom", "steps": [{"do": "tap", "target": "Boom"}], "expect": ["x"]}])

    async def go() -> tuple[Any, int, int]:
        async with async_playwright() as p:
            b = await p.chromium.launch(headless=True, args=app_check.CHROMIUM_ARGS)
            try:
                r = await check_app(SPLITTER, journeys=found, verifier=jv.Verifier(predict), browser=b)
                at_return = len(asked)
                await asyncio.sleep(1.5)
                return r, at_return, len(asked)
            finally:
                await b.close()

    r, at_return, later = asyncio.run(go())
    assert r.journeys.not_run.startswith("Jev did not answer") and not any(i.startswith("Journey") for i in r.issues)
    assert later == at_return, f"{later - at_return} more Jev requests after the check returned"


# ---- review round 3: each of these failed on the code before its fix ----------------------------------------

def slow(predict: Any, secs: float) -> Any:
    import time

    def ask(state: dict[str, Any], questions: dict[str, Any]) -> dict[str, Any]:
        time.sleep(secs)
        return predict(state, questions)

    return ask


def test_a_control_rerendered_while_jev_answers_is_found_again_not_blamed_on_the_app() -> None:
    # a view re-rendered by a timer (a clock, a running timer) replaces the tagged node while Jev answers
    page = """<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1">
<title>T</title></head><body><div id="view"></div><script>
let n = 0;
function render() { view.innerHTML = '<p>Count ' + n + '</p><p>' + Date.now() + '</p><button id="add">Add one</button>';
  document.getElementById("add").onclick = () => { n++; eve.save({ v: 1, n }); render(); }; }
setInterval(render, 1000); render(); eve.load();
</script></body></html>"""
    found, _ = jv.parse_journeys([{"name": "Count", "steps": [{"do": "tap", "target": "Add one"}] * 3,
                                   "expect": ["Count 3"]}])
    r = asyncio.run(check_app(page, journeys=found, verifier=jv.Verifier(slow(LiteralJev(), 0.4))))
    assert r.journeys.passed == 1, r.journeys.as_dict()


# A custom dropdown (a button that opens a list) and a radio group under a legend: both are "choose" steps.
CHOOSER = """<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1">
<title>T</title><style>button,label{min-height:44px;display:block}</style></head><body>
<button id="cat">Category</button><div id="menu" hidden><button>Food</button><button>Travel</button></div>
<fieldset><legend>Paid by</legend><label><input type="radio" name="p" value="Alex">Alex</label>
<label><input type="radio" name="p" value="Sam">Sam</label></fieldset>
<p id="out">Nothing picked</p><script>
let s = { v: 1, cat: "", by: "" };
const show = () => { out.textContent = "Category: " + (s.cat || "none") + " · Paid by " + (s.by || "nobody"); };
cat.onclick = () => { menu.hidden = !menu.hidden; };
menu.querySelectorAll("button").forEach((b) => b.onclick = () => { s.cat = b.textContent; menu.hidden = true; show(); eve.save(s); });
document.querySelectorAll("input[name=p]").forEach((r) => r.onchange = () => { s.by = r.value; show(); eve.save(s); });
eve.load();
</script></body></html>"""


def test_choose_on_a_custom_dropdown_picks_the_value_not_just_the_opener() -> None:
    found, _ = jv.parse_journeys([{"name": "Pick", "steps": [{"do": "choose", "target": "Category", "value": "Travel"}],
                                   "expect": ["Category: Travel"]}])
    r = asyncio.run(check_app(CHOOSER, journeys=found, verifier=jv.Verifier(LiteralJev())))
    assert r.journeys.passed == 1, r.journeys.as_dict()


def test_choose_in_a_radio_group_named_by_its_legend_picks_the_option() -> None:
    found, _ = jv.parse_journeys([{"name": "Payer", "steps": [{"do": "choose", "target": "Paid by", "value": "Sam"}],
                                   "expect": ["Paid by Sam"]}])
    r = asyncio.run(check_app(CHOOSER, journeys=found, verifier=jv.Verifier(LiteralJev())))
    assert r.journeys.passed == 1, r.journeys.as_dict()


def test_choose_fails_when_the_value_is_nowhere_after_opening_the_target() -> None:
    found, _ = jv.parse_journeys([{"name": "Pick", "steps": [{"do": "choose", "target": "Category", "value": "Rent"}],
                                   "expect": ["Category: Rent"]}])
    r = asyncio.run(check_app(CHOOSER, journeys=found, verifier=jv.Verifier(LiteralJev())))
    res = r.journeys.results[0]
    assert res.status == "step" and 'could not find "Rent"' in res.reason, res.as_dict()


def test_a_journey_that_breaks_the_page_is_its_own_failure_and_the_others_still_count(
        monkeypatch: pytest.MonkeyPatch) -> None:
    from playwright.async_api import Error as PlaywrightError

    real = jv.walk

    async def walk(page: Any, journey: jv.Journey, verifier: Any, **kw: Any) -> jv.JourneyResult:
        if journey.name == "Crash":
            kw["on_step"]("step 1")
            raise PlaywrightError("Target crashed")
        return await real(page, journey, verifier, **kw)

    monkeypatch.setattr(jv, "walk", walk)
    found = journeys() + jv.parse_journeys([{"name": "Crash", "steps": [{"do": "tap", "target": "Add person"}],
                                             "expect": ["x"]}])[0]
    r = asyncio.run(check_app(BROKEN, journeys=found, verifier=jv.Verifier(LiteralJev())))
    assert r.journeys.not_run == "" and [x.status for x in r.journeys.results] == ["expect", "step"]
    assert "Target crashed" in r.journeys.results[1].reason
    assert sum(1 for i in r.issues if i.startswith("Journey")) == 2 and "journeys 0/2" in r.summary()


def test_jev_down_in_one_journey_keeps_the_journeys_that_already_failed() -> None:
    import time

    literal = LiteralJev()

    def predict(state: dict[str, Any], questions: dict[str, Any]) -> dict[str, Any]:
        if "Boom" in str(state.get("step", "")):
            time.sleep(2.0)                        # the split journey finishes (and fails) before Jev goes down
            raise TimeoutError("timed out")
        return literal(state, questions)

    found = journeys() + jv.parse_journeys([{"name": "Boom", "steps": [{"do": "tap", "target": "Boom"}],
                                             "expect": ["x"]}])[0]
    r = asyncio.run(check_app(BROKEN, journeys=found, verifier=jv.Verifier(predict)))
    assert [x.name for x in r.journeys.results] == ["Split a dinner"] and r.journeys.results[0].status == "expect"
    assert r.journeys.not_run.startswith("Jev did not answer")
    assert any(i.startswith('Journey "Split a dinner"') for i in r.issues) and not r.ok
    assert "journeys 0/2 (1 not run: Jev did not answer" in r.summary()


def test_one_failed_jev_request_is_asked_again_before_giving_up() -> None:
    literal = LiteralJev()
    failed = [0]

    def flaky(state: dict[str, Any], questions: dict[str, Any]) -> dict[str, Any]:
        if not failed[0]:
            failed[0] = 1
            raise TimeoutError("read timed out")
        return literal(state, questions)

    r = asyncio.run(check_app(SPLITTER, journeys=journeys(), verifier=jv.Verifier(flaky)))
    assert r.journeys.passed == 1 and r.journeys.not_run == "", r.journeys.as_dict()


def test_an_app_that_hangs_in_a_journey_fails_that_journey_in_seconds(monkeypatch: pytest.MonkeyPatch) -> None:
    import time

    monkeypatch.setattr(jv, "EVAL_TIMEOUT_S", 2.0)
    page = """<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1">
<title>T</title></head><body><label for="n">Name</label><input id="n"><p>Hi</p><script>
n.onchange = (e) => { if (e.target.value === "spin-forever") for (;;) {} eve.save({ v: 1 }); }; eve.load();
</script></body></html>"""
    found, _ = jv.parse_journeys([{"name": "Hang", "steps": [{"do": "type", "target": "Name", "value": "spin-forever"},
                                                             {"do": "type", "target": "Name", "value": "b"}],
                                   "expect": ["Hi b"]}])
    t0 = time.perf_counter()
    r = asyncio.run(check_app(page, journeys=found, verifier=jv.Verifier(LiteralJev())))
    assert time.perf_counter() - t0 < 30
    res = r.journeys.results[0]
    assert res.status == "step" and "stopped responding" in res.reason, res.as_dict()


def test_journeys_are_skipped_when_the_scripted_pass_already_found_the_app_hung(
        monkeypatch: pytest.MonkeyPatch) -> None:
    import time

    from eve_bridge import app_check

    monkeypatch.setattr(app_check, "CHECK_TIMEOUT_S", 6.0)
    page = """<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1">
<title>T</title></head><body><button id="go">Start</button><script>
go.onclick = () => { for (;;) {} }; eve.load();
</script></body></html>"""
    found, _ = jv.parse_journeys([{"name": "Go", "steps": [{"do": "tap", "target": "Start"}], "expect": ["x"]}])
    fake = LiteralJev()
    t0 = time.perf_counter()
    r = asyncio.run(check_app(page, journeys=found, verifier=jv.Verifier(fake)))
    assert time.perf_counter() - t0 < 25
    assert any("stopped responding" in i for i in r.issues)
    assert r.journeys.not_run.startswith("the app stopped responding") and fake.calls == 0


def test_the_owners_photo_is_the_last_screen_of_a_journey_that_passed() -> None:
    r = asyncio.run(check_app(SPLITTER, journeys=journeys(), verifier=jv.Verifier(LiteralJev())))
    assert r.photo_from == 'the journey "Split a dinner"' and r.screenshot and r.screenshot[:2] == b"\xff\xd8"
    assert r.photo_data["people"] == ["Alex", "Sam"]                  # the journey's data, not the script's
    broken = asyncio.run(check_app(BROKEN, journeys=journeys(), verifier=jv.Verifier(LiteralJev())))
    assert broken.photo_from == "the scripted check"                  # no journey passed: the script's picture


def test_an_empty_or_unusable_journeys_block_is_a_problem_not_silence() -> None:
    assert jv.parse_journeys([])[1] == ["The journeys block holds no journey that can be walked; write 2 to 4."]
    got, problems = jv.extract_journeys("```journeys\n[]\n```")
    assert got == [] and problems
    only_bad = [{"name": "x", "steps": [], "expect": ["y"]}]
    assert len(jv.parse_journeys(only_bad)[1]) == 2


def test_the_owner_reads_a_short_line_and_the_repair_round_the_details() -> None:
    step = jv.JourneyResult("Split a dinner", "step", steps=6, step=3,
                            reason='tap "Add expense": could not find "Add expense" to tap (the controls on screen: '
                                   '"Add person", "Name").', evidence='"Expense Splitter | People"')
    assert step.owner_line() == 'Couldn\'t tap "Add expense" in "Split a dinner".'
    assert "the controls on screen" in step.issue()
    exp = jv.JourneyResult("Split a dinner", "expect", steps=6,
                           expects=[{"text": "Alex owes Sam $15.00", "score": 0.1, "shown": False}])
    assert exp.owner_line() == 'After "Split a dinner", the screen didn\'t show "Alex owes Sam $15.00".'


def test_words_there_but_not_in_the_expectations_order_veto_shown() -> None:
    # a live build: Jev read 0.56 to 0.62 for "1-day streak" over this line, and the cut was 0.4
    assert jv.words_shown("1-day streak", ("Drink water", "Streak ended · best 1 day")) is False
    assert jv.words_shown("Alex owes Sam $15.00", ("Sam owes Alex $15.00",)) is False
    assert jv.words_shown("1-day streak", ("Drink water", "🔥 1-DAY streak")) is True       # case, emoji, hyphen
    assert jv.words_shown("Alex owes Sam $15.00", ("Alex owes Sam", "$15")) is True        # a line break, a format
    assert jv.words_shown("Total $1,234.50", ("Total: $1234.5",)) is True
    assert jv.words_shown("✓", ("anything",)) is True                                      # no words to hold to
