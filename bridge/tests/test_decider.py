"""decider.py against a scripted predict: the one-question contract, every answer
rejection rule, the head-budget trim, the three prompt styles, the overflow flag,
the available flag and the predict lock."""

from __future__ import annotations

import math
import threading

import pytest

from eve_bridge import decider as dm
from eve_bridge.decider import (
    RESERVED,
    STYLES,
    Choice,
    Decider,
    instruction_tokens,
    render_instructions,
    render_option,
    render_question,
    trim_options,
)

# ---- fakes -------------------------------------------------------------------------

MENU = {"1": "click radio button: Week", "2": "click button: Today", "3": "click button: Add Event",
        "reobserve": "the screen is still changing, look again",
        "abstain": "none of these advances the objective"}


def _answer(choice: str, probs: dict, confidence: float = 0.4, tokens: int = 300) -> dict:
    return {"answers": {"pick": {"type": "choice", "choice": choice, "probabilities": probs,
                                 "confidence": confidence, "action": {"act_probability": 0.5}}},
            "usage": {"input_tokens": tokens, "output_tokens": 0}}


class FakePredict:
    """Serves scripted results in order; records every (state, questions) it was asked."""

    def __init__(self, *results, raise_first: BaseException | None = None) -> None:
        self.results = list(results)
        self.raise_first = raise_first
        self.calls: list[tuple] = []

    def __call__(self, state, questions):
        self.calls.append((state, questions))
        if self.raise_first is not None:
            e, self.raise_first = self.raise_first, None
            raise e
        if not self.results:
            raise AssertionError("predict ran out of scripted results")
        return self.results.pop(0)


class Clock:
    def __init__(self) -> None:
        self.t = 0.0

    def __call__(self) -> float:
        self.t += 0.005
        return self.t


def _decider(*results, **kw) -> tuple[Decider, FakePredict]:
    p = FakePredict(*results, raise_first=kw.pop("raise_first", None))
    return Decider(p, clock=Clock(), **kw), p


PROBS_OK = {"1": 0.7, "2": 0.2, "3": 0.05, "reobserve": 0.03, "abstain": 0.02}


# ---- a valid pick --------------------------------------------------------------------

def test_valid_pick_carries_p_top_margin_and_metadata() -> None:
    d, p = _decider(_answer("1", PROBS_OK, confidence=0.46, tokens=300))
    c = d.choose("switch to week view", app="Calendar", context="title: September 2026", options=MENU)
    assert isinstance(c, Choice) and c.id == "1" and not c.error
    assert c.p_top == pytest.approx(0.7) and c.margin == pytest.approx(0.5)
    assert c.confidence == pytest.approx(0.46) and c.k == 5 and c.input_tokens == 300
    assert c.overflow is False and c.dropped_for_budget == 0 and c.ms == pytest.approx(5.0)
    assert list(c.probabilities) == list(MENU)           # option order, not the model's
    state, questions = p.calls[0]
    assert list(questions) == ["pick"] and questions["pick"]["type"] == "choice"
    assert questions["pick"]["criteria"] == MENU


def test_state_has_goal_first_and_never_repeats_the_candidates() -> None:
    d, p = _decider(_answer("1", PROBS_OK))
    d.choose("switch to week view", app="Calendar", context="title: September 2026", options=MENU,
             recent=["clicked Today"])
    state, _ = p.calls[0]
    assert list(state) == ["goal", "app", "context", "recent"]
    assert state == {"goal": "switch to week view", "app": "Calendar", "context": "title: September 2026",
                     "recent": ["clicked Today"]}
    assert "Week" not in str(state) and "Add Event" not in str(state)


def test_tie_within_tolerance_is_accepted_with_zero_margin() -> None:
    probs = {"1": 0.5, "2": 0.5, "3": 0.0, "reobserve": 0.0, "abstain": 0.0}
    d, _ = _decider(_answer("2", probs))
    c = d.choose("go", app="A", context="", options=MENU)
    assert c.id == "2" and c.margin == 0.0 and not c.error


# ---- rejection rules -----------------------------------------------------------------

@pytest.mark.parametrize("choice, probs, why", [
    ("9", PROBS_OK, "not an option"),
    ("1", {k: v for k, v in PROBS_OK.items() if k != "3"}, "do not cover"),
    ("1", {**PROBS_OK, "extra": 0.0}, "do not cover"),
    ("1", {**PROBS_OK, "2": math.nan}, "not in [0, 1]"),
    ("1", {**PROBS_OK, "2": math.inf}, "not in [0, 1]"),
    ("1", {**PROBS_OK, "2": 1.5}, "not in [0, 1]"),
    ("1", {**PROBS_OK, "2": -0.1}, "not in [0, 1]"),
    ("1", {**PROBS_OK, "2": "0.2"}, "not in [0, 1]"),
    ("2", PROBS_OK, "not the most probable"),
])
def test_rejected_answers_have_empty_id_and_a_reason(choice: str, probs: dict, why: str) -> None:
    d, _ = _decider(_answer(choice, probs))
    c = d.choose("go", app="A", context="", options=MENU)
    assert c.id == "" and why in c.error and c.k == 5


def test_malformed_results_are_rejected_not_raised() -> None:
    for result in ("nope", {}, {"answers": {}}, {"answers": {"pick": "x"}}, {"answers": {"pick": {}}}):
        d, _ = _decider(result)
        c = d.choose("go", app="A", context="", options=MENU)
        assert c.id == "" and c.error


def test_predict_raising_becomes_an_error_choice() -> None:
    d, _ = _decider(raise_first=RuntimeError("Metal device lost"))
    c = d.choose("go", app="A", context="", options=MENU)
    assert c.id == "" and c.error == "predict raised RuntimeError: Metal device lost"
    assert c.ms > 0 and c.k == 5


def test_caller_contract_violations_raise() -> None:
    d, _ = _decider()
    with pytest.raises(ValueError, match="reserved"):
        d.choose("go", app="A", context="", options={"1": "click button: Today"})
    with pytest.raises(ValueError, match="at least one real"):
        d.choose("go", app="A", context="", options={k: MENU[k] for k in RESERVED})
    with pytest.raises(ValueError, match="non-empty"):
        d.choose("go", app="A", context="", options={})
    with pytest.raises(ValueError, match="style"):
        Decider(lambda s, q: {}, style="verbose")


# ---- overflow ---------------------------------------------------------------------------

def test_overflow_flag_when_the_state_fills_the_sequence() -> None:
    d, _ = _decider(_answer("1", PROBS_OK, tokens=1024), max_len=1024)
    assert d.choose("go", app="A", context="x" * 5000, options=MENU).overflow is True
    d, _ = _decider(_answer("1", PROBS_OK, tokens=1023), max_len=1024)
    assert d.choose("go", app="A", context="", options=MENU).overflow is False


# ---- the head-budget trim --------------------------------------------------------------

def test_trim_drops_the_last_real_option_first_and_never_the_reserved_two() -> None:
    options = {"1": "a b", "2": "c d", "3": "e f", "reobserve": "look again", "abstain": "none"}
    # word counts: each real option costs 1 + 2 = 3, reobserve 3, abstain 2 → 14 total
    kept, dropped = trim_options(options, budget=14)
    assert kept == options and dropped == 0
    kept, dropped = trim_options(options, budget=11)
    assert list(kept) == ["1", "2", "reobserve", "abstain"] and dropped == 1
    kept, dropped = trim_options(options, budget=8)
    assert list(kept) == ["1", "reobserve", "abstain"] and dropped == 2
    kept, dropped = trim_options(options, budget=1)              # never below one real option
    assert list(kept) == ["1", "reobserve", "abstain"] and dropped == 2


def test_choose_trims_before_predict_and_reports_the_count() -> None:
    long = " ".join(["word"] * 60)                                  # capped at 48 tokens + 1 marker
    options = {"1": long, "2": long, "3": long, "4": long, "5": long,
               "reobserve": "look again", "abstain": "none"}
    # two capped options (49 each) + "look again" (3) + "none" (2) = 103 head tokens exactly
    fits_two = 16 + instruction_tokens("compact") + 103
    d, p = _decider(_answer("1", {"1": 0.9, "2": 0.05, "reobserve": 0.03, "abstain": 0.02}),
                    head_max_len=fits_two)
    c = d.choose("go", app="A", context="", options=options)
    sent = p.calls[0][1]["pick"]["criteria"]
    assert list(sent) == ["1", "2", "reobserve", "abstain"]
    assert c.id == "1" and c.dropped_for_budget == 3 and c.k == 4
    d, p = _decider(_answer("1", {"1": 0.9, "reobserve": 0.07, "abstain": 0.03}), head_max_len=fits_two - 1)
    c = d.choose("go", app="A", context="", options=options)
    assert list(p.calls[0][1]["pick"]["criteria"]) == ["1", "reobserve", "abstain"]
    assert c.dropped_for_budget == 4 and c.k == 3


def test_option_and_instruction_token_estimates_use_the_injected_tokenizer() -> None:
    calls: list[str] = []

    def tokenize(text: str) -> int:
        calls.append(text)
        return 7

    assert dm.option_tokens("click button: Week", tokenize) == 8
    assert dm.option_tokens(" ".join(["w"] * 200), lambda t: 200) == 49         # the 48-token cap
    assert instruction_tokens("compact", tokenize=tokenize) == 7
    assert calls[0] == " click button: Week" and calls[-1].startswith("choice question: ")
    assert dm.word_count("a  b c") == 3


# ---- styles ---------------------------------------------------------------------------

def test_three_styles_render_distinct_instructions() -> None:
    rendered = {s: render_instructions(s, "switch to week view", MENU) for s in STYLES}
    assert STYLES == ("jev", "compact", "hinted") and len(set(rendered.values())) == 3
    assert rendered["jev"].endswith("Goal: switch to week view")
    assert "reobserve" in rendered["compact"] and "abstain" in rendered["compact"]
    assert rendered["hinted"].startswith(rendered["compact"])
    assert rendered["hinted"].endswith("matches: 1")                      # only Week shares a goal token
    assert render_instructions("hinted", "do something else", MENU) == rendered["compact"]
    with pytest.raises(ValueError):
        render_instructions("loud", "x", MENU)


def test_hinted_lists_every_overlapping_real_option_and_never_the_reserved() -> None:
    options = {"1": "click button: Today", "2": "click row: Today's Reminders", "3": "click button: Week",
               "reobserve": "the screen is still changing, look again",
               "abstain": "none of these advances the objective"}
    text = render_instructions("hinted", "show today's reminders", options)
    assert text.endswith("matches: 1, 2")
    q = render_question("hinted", "show today's reminders", options)
    assert q == {"type": "choice", "instructions": text, "criteria": options}


def test_decider_sends_its_style_and_the_question_id_is_pick() -> None:
    for style in STYLES:
        d, p = _decider(_answer("1", PROBS_OK), style=style)
        d.choose("switch to week view", app="Calendar", context="", options=MENU)
        q = p.calls[0][1]
        assert list(q) == ["pick"]
        assert q["pick"]["instructions"] == render_instructions(style, "switch to week view", MENU)


# ---- the available flag ------------------------------------------------------------------

def test_a_bare_decider_is_not_available_until_a_loader_warms_it() -> None:
    d, _ = _decider()
    assert d.available is False and d.load_ms == 0.0 and d.warm_ms == 0.0


# ---- the predict lock ---------------------------------------------------------------------

def test_predict_runs_under_the_lock() -> None:
    seen: list[bool] = []
    holder: dict = {}

    def predict(state, questions):
        seen.append(holder["d"]._lock.locked())
        return _answer("1", PROBS_OK)

    d = Decider(predict)
    holder["d"] = d
    assert isinstance(d._lock, type(threading.Lock()))
    d.choose("go", app="A", context="", options=MENU)
    assert seen == [True] and not d._lock.locked()


# ---- render_option: the one wording the lane and the eval share ----------------------------


def test_render_option_label_role_value() -> None:
    from types import SimpleNamespace as NS

    assert render_option(NS(label="Week", role="radio button", value="")) == "Week (radio button)"
    assert render_option(NS(label="Month", role="radio button", value="selected")) == "Month (radio button, selected)"
    assert render_option(NS(label="  Add   Event ", role="button", value=None)) == "Add Event (button)"
    long = render_option(NS(label="x" * 60, role="link", value=""))
    assert long == "x" * 40 + " (link)"                             # the label is capped, the role never cut
    assert render_option(NS(label="Loose", role="", value="")) == "Loose"
