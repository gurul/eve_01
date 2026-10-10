"""A menu longer than one group (typed_ask.ask_jev_step_grouped, after computah's JevChoice, 2026-10-06)."""

from __future__ import annotations

import itertools
from typing import Any

from eve_bridge import fast_lane, typed_ask
from eve_bridge.typed_ask import STEP_NONE, ask_jev_step, ask_jev_step_grouped, make_step_asker


def clock() -> float:
    return 0.0


def menu(n: int) -> dict[str, str]:
    return {str(i): f"Control {i} (button)" for i in range(1, n + 1)}


def choice(pick: str, keys: list[str], p: float = 0.95) -> dict[str, Any]:
    rest = (1.0 - p) / max(1, len(keys) - 1)
    return {"choice": pick, "probabilities": {k: (p if k == pick else rest) for k in keys}, "confidence": p}


class Fake:
    """A predict that answers each choice question with `pick(qid, criteria)`, nouls with fixed values."""

    def __init__(self, pick, nouls: tuple[float, float, float] = (0.9, 0.1, 0.1),
                 final_nouls: tuple[float, float, float] | None = None) -> None:
        self.pick, self.nouls, self.final_nouls = pick, nouls, final_nouls
        self.calls: list[tuple[dict[str, Any], dict[str, Any]]] = []

    def __call__(self, state: dict[str, Any], questions: dict[str, Any]) -> dict[str, Any]:
        self.calls.append((state, questions))
        nouls = self.final_nouls if (self.final_nouls and "target" in questions) else self.nouls
        answers: dict[str, Any] = {}
        for qid, q in questions.items():
            if q["type"] == "choice":
                keys = list(q["criteria"])
                answers[qid] = choice(self.pick(qid, keys), keys)
        answers.update(present={"noul": nouls[0]}, already_done={"noul": nouls[1]}, risky={"noul": nouls[2]})
        return {"answers": answers}


def test_a_menu_that_fits_one_group_is_asked_exactly_as_before() -> None:
    options = menu(25)
    old, new = Fake(lambda q, keys: "7"), Fake(lambda q, keys: "7")
    a = ask_jev_step(old, "press control 7", app="App", context="w", options=options, clock=clock)
    b = ask_jev_step_grouped(new, "press control 7", app="App", context="w", options=options, clock=clock)
    assert old.calls == new.calls and len(new.calls) == 1          # the same single request, byte for byte
    assert a == b and b.target == "7" and b.requests == 1


def test_a_long_menu_is_one_request_of_parallel_groups_then_one_final() -> None:
    options = menu(80)
    fake = Fake(lambda q, keys: "63" if "63" in keys else STEP_NONE)
    a = ask_jev_step_grouped(fake, "press control 63", app="App", context="w", options=options, clock=clock)
    assert len(fake.calls) == 2 and a.requests == 2 and a.k == 81
    first = fake.calls[0][1]
    parts = sorted(q for q in first if q.startswith("target_part"))
    assert parts == ["target_part0", "target_part1", "target_part2", "target_part3"]
    assert {"present", "already_done", "risky"} <= set(first)
    assert [len(first[p]["criteria"]) - 1 for p in parts] == [25, 25, 25, 5]   # each group plus its none
    assert all(first[p]["criteria"][STEP_NONE] == typed_ask.STEP_PART_NONE for p in parts)
    assert len(fake.calls[0][0]["controls"]) == 80                  # the nouls see the whole window
    assert a.target == "63" and typed_ask.JEV_STEP_GATES.decide(a) == "63"


def test_the_final_round_offers_each_groups_top_three_in_menu_order() -> None:
    options = menu(60)
    fake = Fake(lambda q, keys: "40" if "40" in keys else STEP_NONE)
    ask_jev_step_grouped(fake, "press control 40", app="App", context="w", options=options, clock=clock)
    final = fake.calls[1][1]["target"]["criteria"]
    offered = [k for k in final if k != STEP_NONE]
    assert len(offered) == 9 and "40" in offered                    # three groups × three finalists
    assert offered == sorted(offered, key=int)                      # menu order, not probability order


def test_every_group_answering_none_abstains_without_a_second_request() -> None:
    fake = Fake(lambda q, keys: STEP_NONE)
    a = ask_jev_step_grouped(fake, "press the moon", app="App", context="w", options=menu(70), clock=clock)
    assert len(fake.calls) == 1 and a.target == STEP_NONE
    assert typed_ask.JEV_STEP_GATES.decide(a) == STEP_NONE


def test_risk_from_the_first_round_is_kept_even_if_the_final_reads_it_lower() -> None:
    fake = Fake(lambda q, keys: "30" if "30" in keys else STEP_NONE, nouls=(0.9, 0.1, 0.8),
                final_nouls=(0.9, 0.1, 0.1))
    a = ask_jev_step_grouped(fake, "delete control 30", app="App", context="w", options=menu(40), clock=clock)
    assert a.risky == 0.8 and typed_ask.JEV_STEP_GATES.decide(a) == "confirm"


def test_an_answer_outside_its_group_is_an_error_and_never_a_press() -> None:
    fake = Fake(lambda q, keys: "999")
    a = ask_jev_step_grouped(fake, "x", app="App", context="w", options=menu(30), clock=clock)
    assert a.error and a.target == "" and typed_ask.JEV_STEP_GATES.decide(a) == STEP_NONE and len(fake.calls) == 1


def test_a_failing_predict_abstains() -> None:
    def boom(state, questions):
        raise TimeoutError("slow")
    a = ask_jev_step_grouped(boom, "x", app="App", context="w", options=menu(30), clock=clock)
    assert a.error.startswith("TimeoutError") and typed_ask.JEV_STEP_GATES.decide(a) == STEP_NONE


def test_the_switch_picks_the_asker_and_its_menu() -> None:
    fake = Fake(lambda q, keys: keys[0])
    on = make_step_asker(fake, clock, {"EVE_JEV_GROUPS": "1"})
    off = make_step_asker(fake, clock, {"EVE_JEV_GROUPS": "0"})
    assert on.func is ask_jev_step_grouped and on.max_options == typed_ask.GROUPED_MAX_OPTIONS
    assert off.func is ask_jev_step and off.max_options is None
    unset = make_step_asker(fake, clock, {})
    assert (unset.func is ask_jev_step_grouped) == typed_ask.GROUPS_DEFAULT


# ---- the lane offers a grouped asker more than 25 controls ---------------------------------------------------

class _Senses:
    def __init__(self, snap) -> None:
        self.snap = snap

    def snapshot(self):
        return self.snap

    def text_visible(self, s):
        return False

    def focused(self):
        return None

    def frontmost_pid(self):
        return 1

    def screen_changed(self):
        return None


def _snapshot(n: int):
    from eve_bridge.ax_candidates import Candidate, Snapshot

    els = tuple(Candidate(id=str(i), role="button", label=f"Thing {i}", value="", frame=(10, 10 + 30 * i, 80, 20),
                          actions=("AXPress",), enabled=True, app="App", in_content=False, in_dialog=False,
                          secure=False, editable=False) for i in range(n))
    return Snapshot(seq=next(_SEQ), app="App", bundle="x.app", title="w", pid=1, elements=els)


_SEQ = itertools.count(1)


def test_run_delegate_offers_a_grouped_asker_up_to_its_max_and_a_plain_one_25() -> None:
    seen: list[int] = []

    def asker(objective, *, app, context, options, recent=()):
        seen.append(len(options))
        return typed_ask.StepAnswer(STEP_NONE, 0.0, 0.0, 0.0, 0.0, 0.0)

    class Eff:
        def click_candidate(self, c):
            raise AssertionError("nothing should be pressed")

    for max_options, expect in ((None, 25), (typed_ask.GROUPED_MAX_OPTIONS, 90)):
        asker.max_options = max_options
        fast_lane.run_delegate("press thing", senses=_Senses(_snapshot(90)), effectors=Eff(), decider=None,
                               decide="jev", asker=asker, max_steps=1)
        assert seen[-1] == expect
