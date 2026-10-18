"""chrome_lane.ChromeLaneAgent: plan once in the owner's Chrome, Codex for whatever is left."""

from __future__ import annotations

import asyncio
from typing import Any

from eve_bridge.agent_contract import AgentEvent
from eve_bridge.chrome_lane import ChromeLaneAgent


class Planner:
    def __init__(self, result: Any, on_event: Any) -> None:
        self.result, self.on_event, self.running, self.goals = result, on_event, False, []
        self.cancelled = ""

    async def run_in_browser(self, goal: str) -> tuple[str, str]:
        self.goals.append(goal)
        self.on_event(AgentEvent("started", goal))
        self.on_event(AgentEvent("progress", "opened gmail"))
        if isinstance(self.result, Exception):
            raise self.result
        return self.result

    def cancel(self, reason: str = "") -> None:
        self.cancelled = reason


class Codex:
    def __init__(self) -> None:
        self.running, self.goals = False, []

    async def run(self, goal: str) -> str:
        self.goals.append(goal)
        return "codex finished it"


def rig(result: Any):
    events: list[tuple[str, str]] = []
    made: dict[str, Any] = {}

    def make_planner(on_event, ask_user):
        made["planner"] = Planner(result, on_event)
        return made["planner"]

    def make_fallback():
        made["codex"] = Codex()
        return made["codex"]

    async def ask(q: str) -> str:
        return "yes"

    agent = ChromeLaneAgent(make_planner, make_fallback, lambda ev: events.append((ev.kind, ev.text)), ask)
    return agent, made, events


def test_a_finished_plan_answers_and_codex_never_starts() -> None:
    agent, made, events = rig(("You have 3 unread emails.", ""))
    assert asyncio.run(agent.run("how many unread emails do I have")) == "You have 3 unread emails."
    assert "codex" not in made and agent.handed_on is False
    assert events == [("started", "how many unread emails do I have"), ("progress", "opened gmail"),
                      ("final", "You have 3 unread emails.")]                     # the planner's own start is not repeated


def test_part_done_hands_codex_the_goal_and_what_was_done() -> None:
    agent, made, events = rig(("", "\n\n[note] a plan already did: open gmail."))
    assert asyncio.run(agent.run("archive the newsletters")) == "codex finished it"
    assert made["codex"].goals == ["archive the newsletters\n\n[note] a plan already did: open gmail."]
    assert agent.handed_on and any(k == "progress" and "Codex is finishing" in t for k, t in events)


def test_nothing_done_gives_codex_the_whole_task() -> None:
    agent, made, _ = rig(("", ""))
    asyncio.run(agent.run("reply to Sam"))
    assert made["codex"].goals == ["reply to Sam"]


def test_a_broken_lane_never_costs_the_task() -> None:
    agent, made, _ = rig(RuntimeError("Chrome went away"))
    assert asyncio.run(agent.run("check my orders")) == "codex finished it" and made["codex"].goals == ["check my orders"]


def test_the_owners_no_is_the_answer_and_codex_does_not_retry_it() -> None:
    agent, made, _ = rig(("Okay, I stopped before that: click Place order.", ""))
    assert asyncio.run(agent.run("buy the batteries")) == "Okay, I stopped before that: click Place order."
    assert "codex" not in made


def test_a_cancel_stops_without_handing_on() -> None:
    agent, made, events = rig(("", "\n\n[note] something"))

    async def go() -> str:
        agent._cancel_reason = "stopped from Telegram"          # as cancel() does while the lane runs
        return await agent.run("anything")

    assert asyncio.run(go()) == "Stopped." and "codex" not in made and events[-1][0] == "cancelled"


# ---- the daemon's wiring -------------------------------------------------------------------------------

def test_with_the_lane_off_nothing_changes() -> None:
    from types import SimpleNamespace

    from eve_bridge.daemon import Daemon

    assert Daemon._chrome_body(SimpleNamespace(_chrome_lane=None), lambda: None, lambda ev: None, None) == {}


def test_with_the_lane_on_web_goals_take_the_chrome_body_and_the_rest_stay_codex() -> None:
    from types import SimpleNamespace

    from eve_bridge.daemon import Daemon

    host = SimpleNamespace(_chrome_lane=SimpleNamespace(screenshot=lambda: None), _telegram=None,
                           _planner_create=lambda req: None,
                           _agent_cfg=SimpleNamespace())
    wiring = Daemon._chrome_body(host, lambda: None, lambda ev: None, None)
    route = wiring["route_body"]
    assert asyncio.run(route("how many unread emails are in my gmail inbox")) == "chrome"
    assert asyncio.run(route("search amazon for AA batteries")) == "chrome"
    assert asyncio.run(route("open spotify and play jazz")) == "codex"
    assert asyncio.run(route("put the calendar on year view")) == "codex"
    assert wiring["make_auto"]().provider == "chrome-lane"


def test_chrome_approval_is_asked_in_the_owners_telegram_chat() -> None:
    from types import SimpleNamespace

    from eve_bridge.daemon import Daemon

    asked = []

    async def ask_user(q, chat, title=""):
        asked.append((q, chat, title))
        return "yes"

    host = SimpleNamespace(_telegram=SimpleNamespace(_chat_id=4242, _ask_user=ask_user))
    assert asyncio.run(Daemon._ask_owner_on_phone(host, "Allow?")) == "yes"
    assert asked == [("Allow?", 4242, "Chrome access")]
    try:
        asyncio.run(Daemon._ask_owner_on_phone(SimpleNamespace(_telegram=None), "Allow?"))
        raise AssertionError("expected no way to ask")
    except RuntimeError:
        pass


def test_a_dead_connection_is_replaced_on_the_next_connect() -> None:
    from types import SimpleNamespace

    from eve_bridge.browser_lane import BrowserLane, BrowserLaneConfig

    lane = BrowserLane(BrowserLaneConfig(attach=True))
    events = []
    lane._context, lane._browser = object(), SimpleNamespace(is_connected=lambda: False)
    lane._close = lambda: (events.append("closed"), setattr(lane, "_context", None), setattr(lane, "_browser", None))
    lane._ensure = lambda: (events.append("connected"), setattr(lane, "_context", object()))

    async def prompt():
        events.append("asked")

    asyncio.run(lane.connect(prompt))
    # the dead one closes first; the new connection and the owner's consent then run side by side
    assert events[0] == "closed" and sorted(events[1:]) == ["asked", "connected"]
    events.clear()
    lane._browser = SimpleNamespace(is_connected=lambda: True)
    asyncio.run(lane.connect(prompt))
    assert events == []                                      # alive: no reconnect, no new question


def test_stop_while_waiting_for_chromes_allow_is_immediate() -> None:
    import time as _time

    agent, made, events = rig(("never", ""))

    async def slow_prepare(goal: str) -> None:
        await asyncio.sleep(30)                     # the owner has not answered yet

    agent._prepare = slow_prepare

    async def go() -> tuple[str, float]:
        t = _time.perf_counter()
        task = asyncio.ensure_future(agent.run("check my gmail"))
        await asyncio.sleep(0.05)
        agent.cancel("stopped from Telegram")
        return await task, _time.perf_counter() - t

    said, secs = asyncio.run(go())
    assert said == "Stopped." and secs < 1 and "codex" not in made and events[-1][0] == "cancelled"


# ---- the browser picture ---------------------------------------------------------------------------------

def test_a_lane_task_pictures_eves_tab_never_the_desktop() -> None:
    agent, _, _ = rig(("Done.", ""))

    async def tab() -> bytes:
        return b"JPEGBYTES"

    agent._lane_screenshot = tab
    asyncio.run(agent.run("open my github"))
    assert asyncio.run(agent.browser_shot()) == (b"JPEGBYTES", ".jpg", "Your Chrome: eve's tab")


def test_after_codex_took_over_the_picture_is_codexs_tab() -> None:
    from types import SimpleNamespace

    agent, made, _ = rig(("", ""))
    asyncio.run(agent.run("reply to Sam"))
    made["codex"].browser_screenshot = SimpleNamespace(data=b"CODEXTAB", suffix=".png")
    assert asyncio.run(agent.browser_shot())[:2] == (b"CODEXTAB", ".png")
    made["codex"].browser_screenshot = None
    assert asyncio.run(agent.browser_shot()) is None


def test_the_telegram_screenshot_prefers_the_browser_and_falls_back_to_the_desktop(tmp_path) -> None:
    from typing import Optional

    from eve_bridge.telegram import TelegramConfig, TelegramInlet

    sent: list[tuple[str, str]] = []

    class Api:
        async def send_photo(self, chat_id: int, path, caption: str = "") -> None:
            sent.append((open(path, "rb").read()[:9].decode(), caption))

    desk = tmp_path / "desk.jpg"

    def screen() -> Optional[object]:
        desk.write_bytes(b"DESKTOPXX")
        return desk

    inlet = TelegramInlet(TelegramConfig(enabled=True, token="1:A", owner_ids=frozenset({1})), Api(),
                          lambda r: None, screen=screen)

    class Lane:
        provider = "chrome-lane"

        async def browser_shot(self):
            return b"JPEGBYTES", ".jpg", "Your Chrome: eve's tab"

    class Nothing:
        provider = "chrome-lane"

        async def browser_shot(self):
            return None

    assert asyncio.run(inlet._send_screen(1, "", agent=Lane()))["source"] == "chrome_lane"
    assert sent[-1] == ("JPEGBYTES", "Your Chrome: eve's tab")
    asyncio.run(inlet._send_screen(1, "the screen", agent=Nothing()))
    assert sent[-1] == ("DESKTOPXX", "the screen")                    # no browser picture: the desktop, as before


# ---- the "nothing done yet" budget ----------------------------------------------------------------------

class _Lane:
    plans_started = 0


class StuckPlanner:
    """A planner that never gets a plan going (a slow plan call, a lane that hangs) until it is cancelled."""
    def __init__(self, on_event: Any) -> None:
        self.on_event, self.running, self.browser, self.cancelled = on_event, False, _Lane(), ""
        self._stop = asyncio.Event()

    async def run_in_browser(self, goal: str) -> tuple[str, str]:
        await self._stop.wait()
        return f"Stopped: {self.cancelled}.", ""

    def cancel(self, reason: str = "") -> None:
        self.cancelled = reason
        self._stop.set()


class WorkingPlanner(StuckPlanner):
    """A plan starts executing at once and takes longer than the budget to finish."""
    async def run_in_browser(self, goal: str) -> tuple[str, str]:
        await asyncio.sleep(0.02)
        self.browser.plans_started += 1               # browser_lane.BrowserLane.run_plan: a plan is executing
        await asyncio.sleep(0.4)
        return "You have 3 unread emails.", ""


def budget_rig(planner_cls: Any, budget: float = 0.1):
    made: dict[str, Any] = {}

    def make_planner(on_event, ask_user):
        made["planner"] = planner_cls(on_event)
        return made["planner"]

    def make_fallback():
        made["codex"] = Codex()
        return made["codex"]

    async def ask(q: str) -> str:
        return "yes"

    agent = ChromeLaneAgent(make_planner, make_fallback, lambda ev: None, ask, idle_budget_secs=budget)
    return agent, made


def _timed(agent: ChromeLaneAgent, goal: str) -> tuple[str, float]:
    import time as _time

    async def go() -> tuple[str, float]:
        t = _time.perf_counter()
        said = await agent.run(goal)
        return said, _time.perf_counter() - t
    return asyncio.run(go())


def test_a_lane_with_nothing_done_hands_codex_the_task_within_the_budget() -> None:
    """Production, 2026-09-23/24: runs that did nothing still spent 11.3 s and 25.6 s before Codex started."""
    agent, made = budget_rig(StuckPlanner)
    said, secs = _timed(agent, "open that map link")
    assert said == "codex finished it" and made["codex"].goals == ["open that map link"]
    assert secs < 1.0 and made["planner"].cancelled.startswith("no progress")
    assert agent.handed_on


def test_waiting_on_chromes_allow_past_the_budget_hands_codex_the_task() -> None:
    """Production, 2026-09-23: the lane waited 36 s on Chrome's Allow until the owner pressed Stop."""
    agent, made = budget_rig(StuckPlanner)

    async def never_allowed(goal: str) -> None:
        await asyncio.sleep(30)

    agent._prepare = never_allowed
    said, secs = _timed(agent, "check my gmail")
    assert said == "codex finished it" and made["codex"].goals == ["check my gmail"] and secs < 1.0


def test_a_plan_under_way_is_never_cut_off_by_the_budget() -> None:
    agent, made = budget_rig(WorkingPlanner)
    said, secs = _timed(agent, "how many unread emails do I have")
    assert said == "You have 3 unread emails." and "codex" not in made
    assert made["planner"].cancelled == "" and secs >= 0.4


def test_codex_gets_the_owners_chrome_ready_before_it_takes_over() -> None:
    order: list[str] = []
    agent, made, _ = rig(("", " Already done: opened the login page."))

    async def ready() -> None:
        order.append("ready")
    agent._before_fallback = ready
    real = agent._make_fallback
    agent._make_fallback = lambda: (order.append("codex"), real())[1]
    assert asyncio.run(agent.run("log in to turnitin")) == "codex finished it"
    assert order == ["ready", "codex"]


def test_a_failed_chrome_start_still_hands_codex_the_task() -> None:
    agent, made, _ = rig(("", ""))

    async def broken() -> None:
        raise OSError("open failed")
    agent._before_fallback = broken
    assert asyncio.run(agent.run("log in to turnitin")) == "codex finished it"


def test_a_finished_plan_never_starts_the_owners_chrome() -> None:
    agent, made, _ = rig(("done", ""))
    called: list[str] = []

    async def ready() -> None:
        called.append("ready")
    agent._before_fallback = ready
    assert asyncio.run(agent.run("x")) == "done" and called == []


def test_a_stop_while_the_owners_chrome_gets_ready_never_starts_codex() -> None:
    """A cancel() during _before_fallback only reached the finished planner; Codex then ran the whole task."""
    agent, made, events = rig(("", ""))

    async def go() -> str:
        entered, release = asyncio.Event(), asyncio.Event()

        async def ready() -> None:
            entered.set()
            await release.wait()
        agent._before_fallback = ready
        task = asyncio.create_task(agent.run("log in to turnitin"))
        await entered.wait()
        agent.cancel(reason="stopped from Telegram")
        release.set()
        return await task

    assert asyncio.run(go()) == "Stopped." and "codex" not in made and events[-1] == ("cancelled", "Stopped.")
