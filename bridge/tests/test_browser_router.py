"""browser_router.py (Jev picks the body for a task the reflex declined: the owner's computer or the web reader) and
the wrapper's body seam in app_reflex.ReflexFirstAgent.

The live, blind evidence is tools/route_eval.py --browser (browser_holdout2.json); these pin the shape of the
question and the safety rule the code owns.
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any

from eve_bridge import browser_router as br
from eve_bridge.app_reflex import ReflexFirstAgent

ROUTES = Path(__file__).parent / "fixtures" / "routes"

# ---- the router ---------------------------------------------------------------------------------


def answer(p_reader: float, accounts: float, mac: float, public: float, show: float,
           interact: float = 0.05) -> dict[str, Any]:
    return {"answers": {"body": {"probabilities": {"web_reader": p_reader, "owner_computer": 1 - p_reader}},
                        "needs_accounts": {"noul": accounts}, "needs_mac": {"noul": mac},
                        "public_read_job": {"noul": public}, "show_owner": {"noul": show},
                        "needs_interaction": {"noul": interact}}}


def test_router_needs_every_condition() -> None:
    def route(a: dict[str, Any]) -> str:
        return br.make_router(lambda state, qs: a, lambda: 0.0)("anything")

    assert route(answer(0.99, 0.05, 0.02, 0.93, 0.1)) == br.WEB
    assert route(answer(0.99, 0.6, 0.02, 0.93, 0.1)) == br.CODEX        # needs the owner's accounts
    assert route(answer(0.99, 0.05, 0.9, 0.93, 0.1)) == br.CODEX        # needs the Mac
    assert route(answer(0.99, 0.05, 0.02, 0.93, 0.8)) == br.CODEX       # wants it on their screen
    assert route(answer(0.99, 0.05, 0.02, 0.93, 0.1, 0.7)) == br.CODEX  # needs typing into a site
    assert route(answer(0.81, 0.05, 0.02, 0.93, 0.1)) == br.CODEX       # the Choice is not sure enough
    assert route(answer(0.99, 0.05, 0.02, 0.4, 0.1)) == br.CODEX        # not a reading job

    def boom(state: Any, qs: Any) -> Any:
        raise TimeoutError

    assert br.make_router(boom, lambda: 0.0)("x") == br.CODEX


def test_a_safety_answer_the_model_leaves_out_keeps_codex() -> None:
    """A gate answer missing from the reply is not a "no": each safety gate reads as "yes" (1.0), so a private
    request the model half-answered stays on the owner's computer (review A3, 2026-10-18)."""
    for gate in ("needs_accounts", "needs_mac", "show_owner", "needs_interaction"):
        a = answer(0.99, 0.05, 0.02, 0.93, 0.1)
        del a["answers"][gate]
        assert br.make_router(lambda state, qs, a=a: a, lambda: 0.0)("order history on amazon") == br.CODEX, gate
        a["answers"][gate] = {"noul": "yes"}                                 # not a number: same as missing
        assert br.make_router(lambda state, qs, a=a: a, lambda: 0.0)("order history on amazon") == br.CODEX, gate


def test_router_state_is_the_request_only_and_the_bodies_are_structured() -> None:
    seen: dict[str, Any] = {}

    def predict(state: Any, qs: dict[str, Any]) -> dict[str, Any]:
        seen.update(state=state, qs=qs)
        return answer(0.0, 1.0, 1.0, 0.0, 1.0)

    br.ask(predict, "  compare   laptops ", lambda: 0.0)
    assert seen["state"] == {"request": "compare laptops"}
    assert set(seen["qs"]["body"]["criteria"]) == set(br.OPTION_TO_BODY) == {"owner_computer", "web_reader"}
    for option in seen["qs"]["body"]["criteria"].values():
        assert set(option) == {"what", "for", "not_for"}
    assert {k for k, q in seen["qs"].items() if q["type"] == "noul"} == {
        "needs_accounts", "needs_mac", "show_owner", "needs_interaction", "public_read_job"}


def test_fit_never_allows_an_unsafe_route() -> None:
    answers = [br.BodyAnswer(0.99, 0.05, 0.02, 0.95, 0.1, 0.05), br.BodyAnswer(0.97, 0.25, 0.02, 0.9, 0.1, 0.05),
               br.BodyAnswer(0.99, 0.02, 0.02, 0.95, 0.1, 0.45)]
    g = br.fit(answers, [br.WEB, br.CODEX, br.CODEX])
    assert [br.decide(a, g) for a in answers] == [br.WEB, br.CODEX, br.CODEX]


def test_the_shipped_gates_are_the_fitted_ones_and_every_set_is_labelled() -> None:
    assert br.SHIPPED and br.GATES.auto <= 1 and br.GATES.public <= 1       # not the route-nothing placeholder
    for name in ("browser_tuning.json", "browser_holdout.json", "browser_holdout2.json"):
        cases = json.loads((ROUTES / name).read_text(encoding="utf-8"))["cases"]
        assert cases and all(c.get("reader") in (br.WEB, br.CODEX) for c in cases), name
    seen = {c["goal"].lower() for n in ("browser_tuning.json", "browser_holdout.json")
            for c in json.loads((ROUTES / n).read_text(encoding="utf-8"))["cases"]}
    blind = [c["goal"].lower() for c in json.loads((ROUTES / "browser_holdout2.json").read_text("utf-8"))["cases"]]
    assert not seen & set(blind) and len(blind) == len(set(blind))          # the blind set repeats no seen request


# ---- the wiring ---------------------------------------------------------------------------------

class Body:
    def __init__(self, name: str) -> None:
        self.name, self.running, self.goals = name, False, []
        self.provider = name

    async def run(self, goal: str) -> str:
        self.goals.append(goal)
        return self.name


async def opener(app: str) -> tuple[bool, str]:
    return True, ""


def test_the_wrapper_sends_a_routed_task_to_its_named_body_and_keeps_reflexes_first() -> None:
    codex, chrome, reader = Body("codex"), Body("chrome-lane"), Body("web")
    routed: list[str] = []

    async def route_body(goal: str) -> str:
        routed.append(goal)
        return "web" if "compare" in goal else "chrome" if "gmail" in goal else "codex"

    def agent() -> ReflexFirstAgent:
        return ReflexFirstAgent(lambda: codex, lambda ev: None, apps=lambda: ["Spotify"], opener=opener,
                                make_auto=lambda: chrome, route_body=route_body,
                                bodies={"web": lambda: reader})

    assert asyncio.run(agent().run("open Spotify")) == "Opened Spotify."        # the reflex never asks the router
    a = agent()
    assert asyncio.run(a.run("compare three laptops")) == "web" and a.provider == "web"
    assert asyncio.run(agent().run("check my gmail")) == "chrome-lane"          # any other name: make_auto
    assert asyncio.run(agent().run("rename a file")) == "codex"
    assert routed == ["compare three laptops", "check my gmail", "rename a file"]


def test_named_bodies_alone_are_enough_and_a_broken_router_keeps_codex() -> None:
    codex, reader = Body("codex"), Body("web")

    async def to_reader(goal: str) -> str:
        return "web"

    only = ReflexFirstAgent(lambda: codex, lambda ev: None, apps=lambda: [], route_body=to_reader,
                            bodies={"web": lambda: reader})
    assert asyncio.run(only.run("weather in denver")) == "web"

    async def unknown(goal: str) -> str:
        return "somewhere-else"

    no_auto = ReflexFirstAgent(lambda: codex, lambda ev: None, apps=lambda: [], route_body=unknown,
                               bodies={"web": lambda: reader})
    assert asyncio.run(no_auto.run("x")) == "codex"                             # an unknown name without make_auto

    async def broken(goal: str) -> str:
        raise RuntimeError

    b = ReflexFirstAgent(lambda: codex, lambda ev: None, apps=lambda: [], route_body=broken,
                         bodies={"web": lambda: reader})
    assert asyncio.run(b.run("compare laptops")) == "codex"
