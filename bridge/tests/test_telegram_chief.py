"""The chief of staff wired into the Telegram door, the daemon and the self-context block (design P4, 2026-09-29).

Gate G4.1: when off, take_on is absent, and a stray call returns the OFF reason rather than reaching think_hard;
with the chief off, every request body, reply and / menu is byte for byte what origin/main sends; take_on makes no
second model call (a fake creator counts the calls); /jobs and the id words make 0 model calls; the turn's note
carries the chief's line; a chief Mac step queues while a task is running or the desk has the Mac; "go c12"
approves only c12's pending act at its current revision; the approved act is built on Codex alone
(floor="codex"), even with a leftover EVE_COMPUTER=holo; the self-context block stays under BUDGET with everything on.

No network: the chat's brain, the options call, the agent and Telegram are fakes (test_telegram's), the chief is a
real Chief on a temporary ledger, and its clock stands at 10:00, outside quiet hours.
"""

from __future__ import annotations

import ast
import asyncio
import importlib.util
import json
import subprocess
import sys
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Any, Optional

import pytest
from test_chief import DESK_JOB, READ_JOB, READ_SAID, SAID, Clock, at, sheet_reply
from test_self_context import fake_router, owner_env
from test_telegram import (
    CFG,
    NOW,
    OWNER,
    FakeApi,
    FakeCreate,
    call,
    say,
    settle,
    tap_update,
    update,
)

from eve_bridge import chief, lights, self_context, system_context, telegram
from eve_bridge.chief import Chief
from eve_bridge.chief_desk import Desk
from eve_bridge.chief_ledger import Ledger
from eve_bridge.chief_reflect import Memory, Reflector
from eve_bridge.codex_computer import CodexComputerAgent
from eve_bridge.computer_agent import AgentEvent
from eve_bridge.daemon import Daemon
from eve_bridge.telegram import CHIEF_OFF_REASON, CHIEF_TITLE, TASK_DONE_TITLE

ROOT = Path(__file__).resolve().parents[2]
CLOCK_NOTE = "\n\nSystem context:\nCLOCK."
# One step, one-way, from the owner's own words: the act is the first queued step, so "go c<N>" can approve it.
ACT_JOB = {**DESK_JOB, "title": "Order the desk", "done_checks": [{"kind": "ui_seen", "arg": "order confirmed"}],
           "phases": [{"do": "act", "goal": "order the desk in the open tab", "door": None}]}
ACT_SAID = "order the desk in the open tab"
# Research that the router sends to the Mac: a chief Mac step that is not an act.
MAC_JOB = {**READ_JOB, "done_checks": [{"kind": "guru_says_done", "arg": None}], "phases": READ_JOB["phases"][:1]}


# ---- the rig ------------------------------------------------------------------------------------------------

class Agent:
    """A computer agent that finishes as soon as it is released (at once unless ``hold``), with the UI states a
    real Codex run would have captured."""

    def __init__(self, on_event: Any, ask_user: Any, *, hold: bool, evidence: list[dict[str, Any]]) -> None:
        self.on_event, self.ask_user = on_event, ask_user
        self.running, self.goal, self.cancel_reason = False, "", None
        self.ui_evidence = list(evidence)
        self.release = asyncio.Event()
        if not hold:
            self.release.set()

    async def run(self, goal: str, note: str = "") -> str:
        self.running, self.goal = True, goal
        self.on_event(AgentEvent("started", goal))
        self.on_event(AgentEvent("progress", "Clicking Place order"))
        await self.release.wait()
        self.running = False
        return "Done: the order went through."

    def steer(self, text: str) -> bool:
        return False

    def cancel(self, reason: str = "") -> None:
        self.cancel_reason = reason
        self.release.set()


class Rig:
    """An inlet (this tree's telegram, or origin/main's) with test_telegram's fakes, and optionally a real Chief
    built from the inlet exactly as the daemon builds it (Daemon._make_chief), on a temporary ledger."""

    def __init__(self, tmp: Path, api: FakeApi, create: FakeCreate, *, module: ModuleType = telegram,
                 with_chief: bool = False, route: str = "web", hold: bool = False,
                 evidence: Optional[list[dict[str, Any]]] = None, **kw: Any) -> None:
        self.api, self.create = api, create
        self.agents: list[Agent] = []
        self.floors: list[Optional[str]] = []
        self.thought: list[str] = []
        self.clock = Clock(at(10))

        def factory(on_event: Any, ask_user: Any, **floor: Any) -> Agent:
            self.floors.append(floor.get("floor"))
            agent = Agent(on_event, ask_user, hold=hold, evidence=evidence or [])
            self.agents.append(agent)
            return agent

        async def no_sleep(_secs: float) -> None:
            await asyncio.sleep(0)

        async def thinker(question: str, **_: Any) -> dict[str, Any]:
            self.thought.append(question)
            return {"ok": True, "answer": "thought"}

        lent: dict[str, Any] = dict(agent_factory=factory, brief=lambda: "", on_state=lambda s: None,
                                    clock=lambda: 0.0, wall=lambda: NOW, sleep=no_sleep, screen=lambda: None,
                                    thinker=thinker)
        lent.update(kw)
        self.inlet = module.TelegramInlet(CFG, api, create, **lent)
        self.closed: list[tuple[str, str]] = []
        real_close = self.inlet._close_chat

        def close_and_keep() -> None:                            # the history, as it was when the chat closed
            self.closed.extend(self.inlet.turns)
            real_close()

        self.inlet._close_chat = close_and_keep
        self.options = FakeCreate(*[sheet_reply() for _ in range(3)])
        self.research: list[str] = []
        self.chief: Optional[Chief] = None
        if with_chief:
            folder = tmp / "chief"
            ledger = Ledger(folder, clock=self.clock)
            self.chief = Chief(ledger=ledger, desk=Desk(folder, clock=self.clock, mode="shadow"),
                               reflector=Reflector(Memory(folder, clock=self.clock), ask=lambda body: {},
                                                   clock=self.clock, offload=False),
                               research=self._research, route_research=lambda goal: route,
                               start_task=self.inlet.chief_start, stop_task=self.inlet.chief_stop,
                               mac_free=self.inlet.chief_mac_free, create=self.options, ask=self.inlet.chief_ask,
                               notify=self.inlet.chief_notify, clock=self.clock, offload=False, home=tmp,
                               environ={"EVE_CHIEF_REPLAN": "off"})     # replanning: test_chief_replan.py
            self.inlet.chief = self.chief

    def history(self) -> list[tuple[str, str]]:
        return self.closed + list(self.inlet.turns)

    def _research(self, query: str) -> dict[str, Any]:
        self.research.append(query)
        return {"ok": True, "answer": "Desk A is $549.", "sources": [], "cost_usd": 0.01}

    async def take(self, args: dict[str, Any], said: str) -> str:
        assert self.chief is not None
        out = await self.chief.handle("take_on", args, said=said, said_ref="2026-09-29 10:00:00")
        assert out["ok"], out
        return str(out["id"])


def run_rig(rig: Rig, during: Any = None) -> None:
    async def go() -> None:
        loop_task = asyncio.ensure_future(rig.inlet.run())
        await settle()
        if during is not None:
            await during()
            await settle()
        assert not loop_task.done(), f"the poll loop died: {loop_task.exception()!r}"
        loop_task.cancel()
        await asyncio.gather(loop_task, return_exceptions=True)
        tasks = [t for t in rig.chief._tasks if not t.done()] if rig.chief is not None else []
        for t in tasks:
            t.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)

    asyncio.run(go())


def texts(api: FakeApi) -> list[str]:
    return [text for _, text in api.sent]


@pytest.fixture(autouse=True)
def _plain(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(system_context, "context", lambda: CLOCK_NOTE)
    monkeypatch.delenv("EVE_CHIEF", raising=False)
    yield
    telegram.unhide_tokens()


# ---- off: absent, and byte for byte what origin/main does ------------------------------------------------------

def test_off_take_on_is_absent_and_a_stray_call_gets_the_off_reason_not_think_hard(tmp_path: Path) -> None:
    rig = Rig(tmp_path, FakeApi([update("hi")]), FakeCreate(say("Hello!")))
    run_rig(rig)
    body = rig.create.requests[0]
    names = {t.get("name") for t in body["tools"]}
    assert not names & set(chief.TOOL_NAMES)
    assert "chief of staff" not in body["instructions"] and "take_on" not in json.dumps(body)

    async def stray() -> dict[str, Any]:
        return await rig.inlet._tool("take_on", dict(DESK_JOB), OWNER)

    assert asyncio.run(stray()) == {"ok": False, "reason": CHIEF_OFF_REASON}
    assert rig.thought == []                                     # never think_hard's
    # positive control: a name that is no tool of anyone's does reach think_hard
    asyncio.run(rig.inlet._tool("some_other_name", {"question": "why"}, OWNER))
    assert rig.thought == ["why"]
    # and the model cannot call it: a take_on call in an off turn is a tool nobody defined, answered as a failure
    rig = Rig(tmp_path, FakeApi([update(SAID)]), FakeCreate(call("take_on", DESK_JOB)))
    run_rig(rig)
    assert texts(rig.api) == [telegram.FAILED_LINE] and rig.agents == []


def test_the_default_switch_builds_no_chief_and_touches_nothing(monkeypatch: pytest.MonkeyPatch) -> None:
    """auto is on only when chief.SHIPPED is True, and only the capture eval sets it: until then the daemon lends
    the door no chief, and never reads the inlet to build one."""
    monkeypatch.setattr(chief, "SHIPPED", False)
    assert Daemon._make_chief(SimpleNamespace(), object(), None) is None
    monkeypatch.setenv("EVE_CHIEF", "off")
    monkeypatch.setattr(chief, "SHIPPED", True)
    assert Daemon._make_chief(SimpleNamespace(), object(), None) is None
    # positive control: auto with SHIPPED, and on, build one from the inlet's own seams
    rig = Rig(Path("/nonexistent"), FakeApi(), FakeCreate())
    monkeypatch.setenv("EVE_CHIEF", "auto")
    built = Daemon._make_chief(SimpleNamespace(), rig.inlet, None)
    assert isinstance(built, Chief) and built._start_task == rig.inlet.chief_start
    assert built._ask == rig.inlet.chief_ask and built._notify == rig.inlet.chief_notify
    monkeypatch.setattr(chief, "SHIPPED", False)
    monkeypatch.setenv("EVE_CHIEF", "on")
    assert isinstance(Daemon._make_chief(SimpleNamespace(), rig.inlet, None), Chief)


def _stand_ins_for_what_this_tree_removed(source: str, monkeypatch: pytest.MonkeyPatch) -> None:
    """origin/main's copy may name a sibling module, or an UPPER_CASE name constant of one, that this tree has
    removed (2026-10-05: an integration was removed, and origin/main names it until this branch lands). Each gets
    a stand-in that is never on: a module whose configured() is disabled and whose TOOL_NAMES is empty, and a
    constant equal to its lowercase name (a toolset's name). Nothing the compared session sends depends on them;
    names this tree still has are left alone."""
    import eve_bridge

    here = Path(eve_bridge.__file__).parent
    tree = ast.parse(source)
    siblings: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.level == 1:
            siblings |= {a.name for a in node.names} if node.module is None else {node.module.split(".")[0]}
    for gone in sorted(m for m in siblings if not (here / f"{m}.py").exists() and not (here / m).is_dir()):
        stub = ModuleType(f"eve_bridge.{gone}")
        stub.TOOL_NAMES = frozenset()                                    # type: ignore[attr-defined]
        stub.configured = lambda *_a, **_k: SimpleNamespace(enabled=False, reason="removed")  # type: ignore[attr-defined]
        monkeypatch.setitem(sys.modules, stub.__name__, stub)
        monkeypatch.setattr(eve_bridge, gone, stub, raising=False)
    for node in ast.walk(tree):
        if (isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name) and node.attr.isupper()
                and node.value.id in siblings and (here / f"{node.value.id}.py").exists()):
            module = importlib.import_module(f"eve_bridge.{node.value.id}")
            if not hasattr(module, node.attr):
                monkeypatch.setattr(module, node.attr, node.attr.lower(), raising=False)


def _origin(name: str, monkeypatch: pytest.MonkeyPatch) -> ModuleType:
    """origin/main's copy of a module as this branch forked from it (the merge base of HEAD and origin/main), loaded
    beside this tree's (same package, its own name). The merge base, not origin/main's tip: main moves on while a
    branch is open (2026-10-09: a reactions merge to main made every switch-off comparison fail on code this branch
    never had). Skipped only when this checkout has no origin/main to compare against."""
    base = subprocess.run(["git", "-C", str(ROOT), "merge-base", "HEAD", "origin/main"],
                          capture_output=True, text=True, check=False)
    ref = base.stdout.strip() if base.returncode == 0 and base.stdout.strip() else "origin/main"
    out = subprocess.run(["git", "-C", str(ROOT), "show", f"{ref}:bridge/src/eve_bridge/{name}.py"],
                         capture_output=True, text=True, check=False)
    if out.returncode != 0:
        pytest.skip("this checkout has no origin/main to compare against")
    _stand_ins_for_what_this_tree_removed(out.stdout, monkeypatch)
    modname = f"eve_bridge._origin_{name}"
    spec = importlib.util.spec_from_loader(modname, loader=None)
    assert spec is not None
    mod = importlib.util.module_from_spec(spec)
    mod.__package__ = "eve_bridge"
    sys.modules[modname] = mod
    exec(compile(out.stdout, f"origin/main:{name}.py", "exec"), mod.__dict__)   # noqa: S102 — our own file at main
    return mod


def _off_session(tmp: Path, module: ModuleType) -> dict[str, Any]:
    """One session with the chief off: a chat turn, the chief's would-be code words (with the chief off they are a
    model turn), a task started and finished, a tap: every request body, message and / menu it makes."""
    api = FakeApi([update("hi", update_id=1)], [update("/jobs", update_id=2)], [update("go c12", update_id=3)],
                  [update("open the calculator", update_id=4)])
    create = FakeCreate(say("Hello!"), say("No jobs here."), say("Go where?"),
                        call("start_task", {"goal": "open the calculator"}))
    rig = Rig(tmp, api, create, module=module)
    run_rig(rig)
    try:
        module.unhide_tokens()
    except AttributeError:
        pass
    return {"requests": create.requests, "sent": api.sent, "titled": api.titled, "buttons": api.buttons,
            "commands": api.commands, "goals": [a.goal for a in rig.agents], "floors": rig.floors,
            "turns": rig.history()}


# The identity rewrite's old and new sentences in telegram.INSTRUCTIONS (the swap runs on decoded text).
_IDENTITY = [
    ("You are eve, a small desk robot with a cheerful, curious personality. Your owner is texting\n"
     "you from their phone, so they are probably not at the desk and cannot see the Mac's screen or hear you.",
     "You are eve, your owner's personal sidekick, with a cheerful, curious personality. You live on\n"
     "their Mac and they reach you from anywhere: by text here, by voice from their iPhone and the speakers at home.\n"
     "You run their Mac for them and look after their home: the lights, the music, the TV. Asked who or what you are,\n"
     "say that in a sentence or two in your own words, from this paragraph alone: earlier replies in today's talk\n"
     "or in your memory that describe you differently are out of date, so never repeat them. Right now they are texting\n"
     "you from their phone, so they are probably not at the Mac and cannot see its screen or hear you."),
    ("you are a pet, not an assistant menu.", "you are a friend, not an assistant menu."),
    ("take_photo is the robot's camera pointed at the room", "take_photo is the camera pointed at the room"),
    ("You are also the robot on the desk, and a text is the same as a word said to it:",
     "When your robot body is connected, a text is the same as a word said to it:"),
]


@pytest.mark.parametrize("toolset_router", ["off", "auto"])
def test_off_every_request_body_reply_and_menu_is_byte_for_byte_origin_main(tmp_path: Path,
                                                                             monkeypatch: pytest.MonkeyPatch,
                                                                             toolset_router: str) -> None:
    """The addendum's switch policy: with the chief off nothing changes. The same session is run on origin/main's
    telegram.py and on this tree's, and everything each sends is compared as JSON. Run with the toolset router off
    (2026-10-05: then the turn is the one before the router) and at its default (keywords here, no Jev key: with
    no Share, Composio or direct app lent, it has nothing to gate)."""
    monkeypatch.setenv("EVE_TOOLSET_ROUTER", toolset_router)
    old = _origin("telegram", monkeypatch)
    try:
        before = _off_session(tmp_path / "old", old)
    finally:
        sys.modules.pop("eve_bridge._origin_telegram", None)
    after = _off_session(tmp_path / "new", telegram)
    assert len(after["requests"]) == 4 and after["goals"] == ["open the calculator"]
    # The one change since origin/main that this session sees: /stick joined the menu (stick_link.py,
    # 2026-09-30). Only that entry is taken out; anything else that differs still fails below.
    stick = ("stick", "Pair Eve Link, the phone app")
    # The stick itself was removed (2026-10-07) and its menu line renamed: origin/main may still say the old words.
    old_stick = ("stick", "Pair Eve Link, the stick's phone app")
    before["commands"] = [(tuple(stick if c == old_stick else c for c in menu), chat) for menu, chat in before["commands"]]
    if not any(stick in menu for menu, _ in before["commands"]):
        assert all(stick in menu for menu, _ in after["commands"])
        after["commands"] = [(tuple(c for c in menu if c != stick), chat) for menu, chat in after["commands"]]
    # The second: eve's identity (2026-10-04, owner: "eve isnt just a desk robot anymore"). Only those
    # sentences are swapped in origin/main's bodies; anything else that differs still fails below.
    # The third: the robot body's tools are lent only with EVE_ROBOT=1 (telegram.ROBOT_BODY_TOOLS).
    for req in before["requests"]:
        if "tools" in req:
            req["tools"] = [t for t in req["tools"] if t.get("name") not in telegram.ROBOT_BODY_TOOLS]
    # The fifth: take_photo is lent without the robot body, as the Mac's camera, and its sentence stays written
    # (2026-10-06). Swapped only while origin/main is older than that change, as /stick is above.
    camera = telegram.INSTRUCTIONS[telegram.INSTRUCTIONS.index('Desktop").') + len('Desktop").'):
                                   telegram.INSTRUCTIONS.index(" If a task asks")]
    if not any(t.get("name") == "take_photo" for req in before["requests"] for t in req.get("tools", ())):
        for req in after["requests"]:
            if "tools" in req:
                req["tools"] = [t for t in req["tools"] if t.get("name") != "take_photo"]
    for i, req in enumerate(before["requests"]):
        text = req.get("instructions", "")
        for a, b in _IDENTITY:
            text = text.replace(a, b)
        text = text.replace('Desktop"). If a task asks', 'Desktop").' + camera + " If a task asks")
        # The fourth: with the robot body off its paragraph is not written either (Lean RoutedTurn P5, 2026-10-05).
        # Swapped only while origin/main is older than that change, as /stick is above: since 5f00a53 reached
        # origin/main its bodies already say INSTRUCTIONS, and the assert that it said INSTRUCTIONS_ROBOT failed.
        if telegram.INSTRUCTIONS_ROBOT in text:
            text = text.replace(telegram.INSTRUCTIONS_ROBOT, telegram.INSTRUCTIONS)
        else:
            assert telegram.INSTRUCTIONS in text
        before["requests"][i]["instructions"] = text
    # The sixth: what the brain knows of earlier messages (telegram.recall_line, 2026-10-08), written on every
    # install. Taken out only while origin/main does not write it; anything else that differs still fails below.
    recalls = [telegram.recall_line(log, mem) for log in (False, True) for mem in (False, True)]
    if not any(r in req.get("instructions", "") for req in before["requests"] for r in recalls):
        for req in after["requests"]:
            text = req.get("instructions", "")
            hits = [r for r in recalls if r in text]
            assert len(hits) <= 1
            if hits:
                req["instructions"] = text.replace(hits[0], "", 1)
    assert json.dumps(after, sort_keys=True, default=str) == json.dumps(before, sort_keys=True, default=str)
    # positive control: the same comparison sees one byte of difference in a request body
    after["requests"][0]["instructions"] += " "
    assert json.dumps(after, sort_keys=True, default=str) != json.dumps(before, sort_keys=True, default=str)


def test_off_the_self_context_block_is_byte_for_byte_origin_main(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    path = tmp_path / "lights.json"
    lights.save([lights.Light(name="a", kind="govee", ip="192.0.2.1")], path)
    old = _origin("self_context", monkeypatch)
    try:
        # auto (the default, unset too) follows chief.SHIPPED: off before the capture eval passed, on after it
        # (2026-09-29); "off" is byte for byte origin/main either way
        auto = ({}, {"EVE_CHIEF": "auto"})
        for extra in ({"EVE_CHIEF": "off"}, *(() if chief.SHIPPED else auto)):
            env = owner_env(**extra)
            assert self_context.block(env, lights_path=path, commit="abc1234") == old.block(
                env, lights_path=path, commit="abc1234")
        off = self_context.block(owner_env(EVE_CHIEF="off"), lights_path=path, commit="abc1234")
        assert "\n- Chief of staff: on.\n" not in off
        for extra in ({"EVE_CHIEF": "on"}, *(auto if chief.SHIPPED else ())):
            on = self_context.block(owner_env(**extra), lights_path=path, commit="abc1234")
            # The feature is now on origin/main too. Its positive control must
            # compare modes, rather than assume that the remote predates it.
            assert on != off
            assert "\n- Chief of staff: on.\n" in on
    finally:
        sys.modules.pop("eve_bridge._origin_self_context", None)


# ---- on: take_on, the code words, the turn line ---------------------------------------------------------------

def test_take_on_makes_no_second_model_call_and_its_backbrief_has_change_and_drop(tmp_path: Path) -> None:
    rig = Rig(tmp_path, FakeApi([update(SAID)]), FakeCreate(call("take_on", DESK_JOB)), with_chief=True)
    run_rig(rig)
    assert len(rig.create.requests) == 1                          # the fake creator counts every call
    body = rig.create.requests[0]
    assert {"take_on", "jobs_list"} <= {t.get("name") for t in body["tools"]}
    assert chief.instructions() in body["instructions"]
    card = rig.chief.ledger.get("c1")
    assert card is not None and card.phase(3).door == "one_way" and card.phase(3).grounded   # "order": his words
    assert card.source.said_ref == "2026-09-29 10:00:00" or card.source.said_ref.startswith("20")
    [(text, buttons)] = [(t, b) for (_, t), b in zip(rig.api.sent, rig.api.buttons, strict=True)]
    assert text.startswith("On it: Standing desk.") and buttons == ["Change", "Drop"]
    assert rig.api.titled[0][0] == CHIEF_TITLE
    assert ("eve", text) in rig.history()                      # the history knows what eve took on
    assert rig.research == [] and rig.agents == []                # the steps run on the chief's loop, not the turn


def eve_rows(rig: Rig) -> list[str]:
    """eve's lines as the transcript keeps them: what Eve Link's chat shows (link_chat.read_messages)."""
    from eve_bridge.link_chat import read_messages

    return [r["text"] for r in read_messages(rig.inlet.transcripts)[0] if r["role"] == "eve"]


CHANGE_DROP = 'Reply "change c1" or "drop c1".'


def test_an_app_turns_backbrief_reaches_the_app_with_change_and_drop_that_work(tmp_path: Path) -> None:
    """Gap 1 (2026-10-09): a take_on from an Eve Link app turn. Its backbrief goes to the app's stream with Change
    and Drop as the words that answer them (as a picker's do), not to Telegram; those words, typed back from the
    app, take the buttons' own path (the chief's code words); the card is recorded against the app turn."""
    from test_telegram import FakeMemory

    rig = Rig(tmp_path, FakeApi(), FakeCreate(call("take_on", DESK_JOB)), with_chief=True,
              memory=FakeMemory(tmp_path))
    rig.inlet._chat_id = OWNER
    shown: list[tuple[str, str, str]] = []
    rig.inlet.app_out = lambda turn, text, kind: shown.append((turn, text, kind)) or True     # the app is reading

    async def go() -> None:
        mark = telegram._FROM_APP.set("app-turn-1")
        try:
            await rig.inlet._turn(telegram.Inbound(OWNER, OWNER, SAID, message_id=1))
            await settle()
            [(turn, text, kind)] = shown
            assert turn == "app-turn-1" and kind == "say"
            assert text.startswith(f"**{CHIEF_TITLE}**\nOn it: Standing desk.") and text.endswith(CHANGE_DROP)
            assert rig.inlet.app_filed("app-turn-1") == "c1" and rig.inlet.app_filed("app-turn-2") is None
            rig.inlet._handle(telegram.Inbound(OWNER, OWNER, "change c1", message_id=2))   # typed in the app
            await settle()
            assert "To change it: drop c1" in shown[-1][1]
            rig.inlet._handle(telegram.Inbound(OWNER, OWNER, "drop c1", message_id=3))
            await settle()
            assert shown[-1][1].endswith("Dropped c1.")
        finally:
            telegram._FROM_APP.reset(mark)

    asyncio.run(go())
    card = rig.chief.ledger.get("c1")
    assert card is not None and card.terminal                       # Drop worked
    assert rig.api.sent == [] and rig.api.buttons == []              # nothing went to Telegram
    plain = next(t for who, t in rig.history() if who == "eve" and t.startswith("On it: Standing desk."))
    assert CHANGE_DROP not in plain                                  # the model's history is the backbrief alone
    assert f"{plain}\n\n{CHANGE_DROP}" in eve_rows(rig)           # the app's chat shows the words to reply


@pytest.mark.parametrize("app_only", [True, False], ids=["app-only", "telegram-on"])
def test_a_telegram_turns_backbrief_shows_change_and_drop_in_the_app_when_telegram_is_muted(
        tmp_path: Path, app_only: bool) -> None:
    """Telegram app-only (the owner is on the app): a take_on from a turn that is not the app's (a call, a typed
    Telegram line) still gets its buttons where Telegram would show them, and the transcript line, which the
    app's chat shows, carries the words that answer them. With Telegram on, the line is as it always was."""
    import dataclasses

    from test_telegram import FakeMemory

    rig = Rig(tmp_path, FakeApi(), FakeCreate(call("take_on", DESK_JOB)), with_chief=True,
              memory=FakeMemory(tmp_path))
    rig.inlet._chat_id = OWNER
    rig.inlet.config = dataclasses.replace(rig.inlet.config, app_only=app_only)
    rig.inlet.app_out = lambda turn, text, kind: True              # an app is linked, but this is not its turn

    async def go() -> None:
        await rig.inlet._turn(telegram.Inbound(OWNER, OWNER, SAID, message_id=1))   # not an app turn
        await settle()

    asyncio.run(go())
    [(text, buttons)] = [(t, b) for (_, t), b in zip(rig.api.sent, rig.api.buttons, strict=True)]
    assert text.startswith("On it: Standing desk.") and buttons == ["Change", "Drop"]
    assert ("eve", text) in rig.history()
    assert (f"{text}\n\n{CHANGE_DROP}" in eve_rows(rig)) is app_only
    assert (text in eve_rows(rig)) is not app_only                 # control: Telegram on keeps the line as it was
    assert rig.inlet.app_filed("anything") is None and rig.inlet._app_filed == {}   # not an app turn


def test_a_backbrief_telegram_refuses_to_button_is_still_sent(tmp_path: Path) -> None:
    api = FakeApi([update(SAID)])

    async def refuse(*a: Any, **k: Any) -> int:
        raise telegram.BotApiError(400, "Bad Request: inline keyboard refused")

    api.send_inline = refuse                                      # type: ignore[method-assign]
    rig = Rig(tmp_path, api, FakeCreate(call("take_on", DESK_JOB)), with_chief=True)
    run_rig(rig)
    assert len(rig.create.requests) == 1 and texts(api)[0].startswith("On it: Standing desk.")


def test_jobs_and_the_id_words_make_no_model_call(tmp_path: Path) -> None:
    api = FakeApi()
    rig = Rig(tmp_path, api, FakeCreate(), with_chief=True)      # any model call raises: "nobody scripted"

    async def during() -> None:
        await rig.take(dict(DESK_JOB), SAID)
        await rig.take(dict(READ_JOB), READ_SAID)
        for n, text in enumerate(["/jobs", "/jobs c1", "keep c2", "raise c2", "drop c2", "reopen c9", "change c1",
                                  "go c1", "Tomorrow c1", "happened c1", "didnt c1"], 1):
            api.feed(update(text, update_id=n))
            await settle()

    run_rig(rig, during)
    assert rig.create.requests == [] and rig.options.requests == []
    said = texts(api)
    assert said[0].startswith("c1 ") and "c2 " in said[0]           # /jobs: both cards, by code
    assert said[1].startswith("c1 Standing desk") and "Check: " in said[1]
    assert "Kept c2." in said and "Dropped c2." in said and any(s.startswith("c2 may now spend $") for s in said)
    assert "there is no card c9" in said
    assert any("To change it: drop c1" in s for s in said)
    assert "c1 is not at that step yet; I'll ask when it is" in said      # go before the pick: refused, by code
    assert "c1 has no reminder to move" in said and "c1 has no step in doubt" in said
    assert rig.chief.ledger.get("c2").status == "dropped"
    assert telegram.FAILED_LINE not in said


def test_off_the_same_words_are_a_model_turn_as_before(tmp_path: Path) -> None:
    rig = Rig(tmp_path, FakeApi([update("/jobs", update_id=1)], [update("drop c2", update_id=2)]),
              FakeCreate(say("Which jobs?"), say("Drop what?")))
    run_rig(rig)
    assert len(rig.create.requests) == 2 and texts(rig.api) == ["Which jobs?", "Drop what?"]


def test_the_turns_note_carries_the_chiefs_line(tmp_path: Path) -> None:
    api = FakeApi()
    rig = Rig(tmp_path, api, FakeCreate(say("Nothing new.")), with_chief=True)

    async def during() -> None:
        await rig.take(dict(DESK_JOB), SAID)
        api.feed(update("anything new?"))

    run_rig(rig, during)
    line = rig.chief.for_turn()
    assert line.startswith("Open jobs: 1 (c1 ") and len(line) <= 300
    notes = [i for i in rig.create.requests[0]["input"] if i.get("role") == "developer"]
    assert len(notes) == 1 and notes[0]["content"][0]["text"].endswith("\n\n" + line)
    assert line not in rig.create.requests[0]["instructions"]    # the note, after the history: cache-free


def test_the_menu_offers_jobs_only_while_the_chief_is_on(tmp_path: Path) -> None:
    on = Rig(tmp_path, FakeApi(), FakeCreate(), with_chief=True)
    run_rig(on)
    off = Rig(tmp_path, FakeApi(), FakeCreate())
    run_rig(off)
    [(on_menu, _)], [(off_menu, _)] = on.api.commands, off.api.commands
    assert off_menu == telegram.BOT_COMMANDS and telegram.JOBS_MENU not in off_menu
    assert on_menu[-2:] == (telegram.JOBS_MENU, telegram.BOT_COMMANDS[-1]) and len(on_menu) == len(off_menu) + 1


# ---- on: the Mac slot, the Go, the floor ----------------------------------------------------------------------

def test_a_chief_mac_step_queues_while_a_task_runs_or_the_desk_has_the_mac(tmp_path: Path) -> None:
    busy = {"desk": False}
    api = FakeApi([update("open the calculator")])
    rig = Rig(tmp_path, api, FakeCreate(call("start_task", {"goal": "open the calculator"})), with_chief=True,
              route="mac", hold=True, busy=lambda: busy["desk"])

    async def during() -> None:
        await rig.take(dict(MAC_JOB), READ_SAID)
        assert rig.inlet.task_running and len(rig.agents) == 1   # the owner's own task has the Mac
        await rig.chief.tick()
        card = rig.chief.ledger.get("c1")
        assert card.status == "waiting" and card.waiting_for == "mac_slot" and len(rig.agents) == 1
        rig.agents[0].release.set()
        await settle()
        assert not rig.inlet.task_running
        busy["desk"] = True                                       # someone is talking at the desk
        await rig.chief.tick()
        assert len(rig.agents) == 1 and rig.chief.ledger.get("c1").waiting_for == "mac_slot"
        # queued in the ledger, not tried and refused: nothing was dispatched while the Mac was taken
        assert not [e for e in rig.chief.ledger.events("c1") if e["e"] in ("dispatched", "result")]
        busy["desk"] = False
        await rig.chief.tick()                                    # free: the step goes, read-only, on the floor
        await settle()
        assert len(rig.agents) == 2 and rig.floors == [None, None]
        assert rig.agents[1].goal.startswith(chief.READ_ONLY)
        assert rig.inlet._chief_task == ("c1", 1)
        rig.agents[1].release.set()
        await settle()

    run_rig(rig, during)
    events = [e for e in rig.chief.ledger.events("c1") if e["e"] == "result"]
    assert events and events[-1]["phase"] == 1 and not events[-1].get("stale")
    assert "On it: c1, step 1." in texts(api)
    # "Task result" once, for the owner's own task; the chief's step gets its receipt in place of one
    assert [title for title, _, _ in api.titled].count(TASK_DONE_TITLE) == 1
    assert texts(api).count("Done: the order went through.") == 1


def test_go_c12_approves_only_that_cards_pending_act_at_its_current_revision(tmp_path: Path) -> None:
    api = FakeApi()
    rig = Rig(tmp_path, api, FakeCreate(say("Sure.")), with_chief=True)

    async def during() -> None:
        await rig.take(dict(ACT_JOB), ACT_SAID)
        await rig.take(dict(ACT_JOB), ACT_SAID)
        api.feed(update("go c2", update_id=1))
        await settle()
        c1, c2 = rig.chief.ledger.get("c1"), rig.chief.ledger.get("c2")
        assert c1.phase(1).approval is None
        assert c2.phase(1).approval is not None and c2.phase(1).approval.rev == c2.rev
        api.feed(update("yes", update_id=2))                    # a bare yes approves nothing that was not asked
        await settle()
        assert rig.chief.ledger.get("c1").phase(1).approval is None
        new = rig.chief.change("c2", {"never": ["over $500"]}, said="make it under $500")
        assert new["ok"] and rig.chief.ledger.get("c2").phase(1).approval is None   # a new revision cancels it
        api.feed(update("go c2", update_id=3))
        await settle()
        c2 = rig.chief.ledger.get("c2")
        assert c2.phase(1).approval is not None and c2.phase(1).approval.rev == c2.rev == 2

    run_rig(rig, during)
    assert len(rig.create.requests) == 1                          # only the bare "yes" was a model turn
    assert "Go: c2, step 1, runs now." in texts(api)


def test_the_go_is_asked_with_yes_and_no_and_the_approved_act_runs_on_codex(tmp_path: Path) -> None:
    evidence = [{"call_id": "u1", "state": "Window: Shop\nOrder confirmed, thank you"}]
    api = FakeApi()
    rig = Rig(tmp_path, api, FakeCreate(), with_chief=True, evidence=evidence)

    async def during() -> None:
        await rig.take(dict(ACT_JOB), ACT_SAID)
        await rig.chief.tick()                                    # the act needs a Go: asked, with Yes and No
        await settle()
        question = texts(api)[-1]
        assert question.startswith("c1, Order the desk: order the desk in the open tab.") and question.endswith("Go?")
        assert api.buttons[-1] == ["Yes", "No"] and api.titled[-1][0] == CHIEF_TITLE
        assert ("eve", "(I asked your Go for c1.)") in rig.history() and ("eve", question) not in rig.history()
        message_id, key = api.key("Yes")
        api.feed(tap_update(key, message_id=message_id))
        await settle()
        await rig.chief.tick()                                    # dispatched: on Codex, whatever the floor
        await settle()
        await rig.chief.tick()                                    # nothing left to run: the card closes
        await settle()

    run_rig(rig, during)
    assert rig.floors == ["codex"] and rig.agents[0].goal.startswith("order the desk in the open tab.")
    assert chief.ACT_GUARD in rig.agents[0].goal
    card = rig.chief.ledger.get("c1")
    assert card.status == "done", card
    receipt = texts(api)[-1]
    assert receipt.startswith("Done: Order the desk") and api.titled[-1][0] == CHIEF_TITLE
    assert api.buttons[-1] == ["Reopen"]
    assert rig.create.requests == []                              # the Go, the act and the receipt: no chat turn


def test_the_go_is_never_asked_over_another_waiting_question(tmp_path: Path) -> None:
    rig = Rig(tmp_path, FakeApi(), FakeCreate(), with_chief=True)

    async def main() -> None:
        loop = asyncio.get_running_loop()
        rig.inlet._pending_answer = loop.create_future()          # a task's question is waiting
        with pytest.raises(RuntimeError):
            await rig.inlet.chief_ask("c1, Order the desk: order it.\nGo?")
        rig.inlet._pending_answer = None

    asyncio.run(main())
    assert rig.api.sent == []


def test_a_tap_on_drop_under_the_backbrief_drops_the_card_by_code(tmp_path: Path) -> None:
    api = FakeApi([update(SAID, update_id=1)])
    rig = Rig(tmp_path, api, FakeCreate(call("take_on", DESK_JOB)), with_chief=True)

    async def during() -> None:
        message_id, key = api.key("Drop")
        api.feed(tap_update(key, message_id=message_id, update_id=2))
        await settle()

    run_rig(rig, during)
    assert rig.chief.ledger.get("c1").status == "dropped" and "Dropped c1." in texts(api)
    assert len(rig.create.requests) == 1


def test_a_leftover_holo_setting_builds_codex_and_the_approved_act_is_codex_alone(
        monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture) -> None:
    """Holo was removed on 2026-09-30: EVE_COMPUTER=holo left in an env file is harmless, tasks run on Codex
    and the daemon says so once at start."""
    made: list[tuple[object, dict[str, Any]]] = []
    monkeypatch.setattr("eve_bridge.app_reflex.ReflexFirstAgent",
                        lambda make_inner, *a, **k: made.append((make_inner(), k)) or SimpleNamespace())
    monkeypatch.setattr(Daemon, "_bodies", lambda self, *a: {"route_body": "the chrome lane"})
    monkeypatch.setenv("EVE_COMPUTER", "holo")
    host = SimpleNamespace(_codex_warm=None, _active_agent=None)
    Daemon._make_agent(host, lambda e: None, None)
    Daemon._make_agent(host, lambda e: None, None, floor="codex")
    (floor_agent, floor_kw), (act_agent, act_kw) = made
    assert isinstance(floor_agent, CodexComputerAgent)            # the floor: Codex, whatever the old setting says
    assert floor_kw.get("route_body") == "the chrome lane"
    assert isinstance(act_agent, CodexComputerAgent)              # the approved act: Codex, which can ask
    assert act_kw.get("enabled") is False and "route_body" not in act_kw and "bodies" not in act_kw
    with caplog.at_level("WARNING", logger="eve_bridge.daemon"):
        assert Daemon._warn_removed_computer() is True
        assert Daemon._warn_removed_computer({"EVE_COMPUTER": "codex"}) is False      # control
    assert [r.getMessage() for r in caplog.records].count(
        "agent: EVE_COMPUTER=holo is set, but Holo was removed (2026-09-30); tasks run on Codex") == 1


# ---- the self-context line ------------------------------------------------------------------------------------

def test_the_self_context_block_stays_under_budget_with_everything_on(monkeypatch: pytest.MonkeyPatch,
                                                                     tmp_path: Path) -> None:
    path = tmp_path / "lights.json"
    lights.save([lights.Light(name="a", kind="govee", ip="192.0.2.1"), lights.Light(name="b", kind="wiz", ip="192.0.2.2"),
                 lights.Light(name="c", kind="triones", address="u"), lights.Light(name="d", kind="tuya", id="x")], path)
    fake_router(monkeypatch, ("perplexity", "tinyfish", "firecrawl"))
    env = owner_env(EVE_VOICE="1", EVE_WEB_READER="1", TINYFISH_API_KEY="test-tinyfish",
                    EVE_WATCH_TLS="1", EVE_WATCH_BROWSER="1",
                    EVE_CHIEF="on")
    text = self_context.block(env, lights_path=path, commit="abcdef123456")
    assert "\n- Chief of staff: on.\n" in text and len(text) < self_context.BUDGET, len(text)
    assert "Voice:" in text and "Jev picks" in text and "Tasks on the Mac: Codex" in text
    monkeypatch.setattr(chief, "SHIPPED", False)
    assert "Chief of staff" not in self_context.block({**env, "EVE_CHIEF": "auto"}, lights_path=path,
                                                       commit="abcdef123456")


# ---- the reviewer's attacks after P4 (2026-09-29): each was reproduced first, then fixed ----------------------

ORDERED = [{"call_id": "u", "state": "Window: Shop\nOrder confirmed, thank you"}]


def test_attack_a_sentence_that_opens_with_ok_never_answers_the_go(tmp_path: Path) -> None:
    """With no relay on, the owner's next message answered any question, and the Go read only its first word:
    "ok, also what's the weather tomorrow?" approved the order and ran it, and never reached the brain. The Go is
    strict like a permission prompt, relay or not: a bare yes or no, or a button, answers it; any other words go
    to the brain and the Go keeps waiting."""
    api = FakeApi()
    rig = Rig(tmp_path, api, FakeCreate(say("Sunny tomorrow.")), with_chief=True, evidence=ORDERED)

    async def during() -> None:
        await rig.take(dict(ACT_JOB), ACT_SAID)
        await rig.chief.tick()
        await settle()
        assert texts(api)[-1].endswith("Go?")
        api.feed(update("ok, also what's the weather tomorrow?", update_id=1))
        await settle()
        await rig.chief.tick()
        await settle()
        assert rig.agents == [] and rig.chief.ledger.get("c1").phase(1).approval is None
        assert rig.inlet._awaiting_answer                          # the Go still waits
        api.feed(update("yes", update_id=2))                       # control: a bare yes answers it
        await settle()
        await rig.chief.tick()
        await settle()

    run_rig(rig, during)
    assert len(rig.create.requests) == 1 and "Sunny tomorrow." in texts(api)
    assert rig.floors == ["codex"]
    kinds = [e["e"] for e in rig.chief.ledger.events("c1") if e["e"] in ("answered", "approved", "dispatched")]
    assert kinds == ["answered", "approved", "dispatched"]


class FailingAgent(Agent):
    """Codex as it fails: an error event, then its failure sentence returned (it does not raise)."""

    async def run(self, goal: str, note: str = "") -> str:
        self.running, self.goal = True, goal
        self.on_event(AgentEvent("started", goal))
        self.on_event(AgentEvent("error", "Codex could not start or timed out."))
        self.running = False
        return "Codex could not start or timed out. No alternate UI automation was used."


def test_attack_a_codex_failure_never_closes_the_card_done(tmp_path: Path) -> None:
    job = {**DESK_JOB, "done_checks": [{"kind": "read_links", "arg": "1"}, {"kind": "cited_pick", "arg": None}]}
    api = FakeApi()
    rig = Rig(tmp_path, api, FakeCreate(), with_chief=True)

    def factory(on_event: Any, ask_user: Any, **k: Any) -> Any:
        rig.floors.append(k.get("floor"))
        agent = FailingAgent(on_event, ask_user, hold=False, evidence=ORDERED)   # even with a screen seen
        rig.agents.append(agent)
        return agent

    rig.inlet._agent_factory = factory
    rig.chief._research = lambda q: {"ok": True, "answer": "Desk A $549",
                                     "sources": [{"url": u} for u in chief_links()]}

    async def during() -> None:
        await rig.take(job, SAID)
        for _ in range(6):
            await rig.chief.tick()
            await settle()
            c = rig.chief.ledger.get("c1")
            if c.status == "waiting" and c.waiting_for == "go":
                break
        api.feed(update("go c1", update_id=9))
        await settle()
        for _ in range(4):
            await rig.chief.tick()
            await settle()

    run_rig(rig, during)
    card = rig.chief.ledger.get("c1")
    assert rig.agents and rig.floors == ["codex"]
    assert card.phase(3).status == "failed" and card.status == "unverified"
    assert not any(t.startswith("Done: Standing desk") for t in texts(api))


def chief_links() -> list[str]:
    from test_chief import LINKS

    return LINKS[:2]


def test_attack_on_a_call_only_a_plain_yes_approves_the_go(tmp_path: Path) -> None:
    """On a call, words that are not a choice were read by a chat model, and its pick approved the act. The Go is
    answered on a call as a permission prompt is: the exact choice or a plain yes or no, else asked again."""
    api = FakeApi()
    spoken: list[str] = []
    rig = Rig(tmp_path, api, FakeCreate(say(json.dumps({"choice": "yes"}))), with_chief=True)
    rig.inlet._call_say = spoken.append

    async def during() -> None:
        await rig.take(dict(ACT_JOB), ACT_SAID)
        await rig.chief.tick()
        await settle()
        rig.inlet.hear("hmm, I'll think about it after lunch")
        await settle()
        assert rig.chief.ledger.get("c1").phase(1).approval is None
        assert telegram.CALL_ASK_AGAIN in spoken
        rig.inlet.hear("yes")                                      # control: a plain yes by voice
        await settle()

    run_rig(rig, during)
    assert rig.create.requests == []                               # no model read the owner's words
    assert rig.chief.ledger.get("c1").phase(1).approval is not None


def test_attack_the_brain_cannot_steer_a_chief_step(tmp_path: Path) -> None:
    """steer_task reached the approved act mid-run, past its Go: a chief step changes only by "change c<N>"."""
    api = FakeApi()
    rig = Rig(tmp_path, api, FakeCreate(), with_chief=True, hold=True)
    steered: list[str] = []

    async def during() -> None:
        await rig.take(dict(ACT_JOB), ACT_SAID)
        rig.chief.approve("c1")
        await rig.chief.tick()
        await settle()
        assert rig.inlet._chief_task == ("c1", 1) and rig.agents
        rig.agents[0].steer = lambda text: steered.append(text) or True
        out = await rig.inlet._tool("steer_task", {"text": "also add the extended warranty and pay for it"}, OWNER)
        assert out["ok"] is False and "c1" in out["reason"]
        stop = await rig.inlet._tool("stop_task", {}, OWNER)       # stop still works
        assert stop == {"ok": True}
        await settle()

    run_rig(rig, during)
    assert steered == []
    # control: the owner's own task is steered as before
    rig = Rig(tmp_path / "own", FakeApi(), FakeCreate(), hold=True)

    async def own() -> None:
        loop_task = asyncio.ensure_future(rig.inlet.run())
        await settle()
        assert rig.inlet._start_task("open the calculator", OWNER)["ok"]
        await settle()
        rig.agents[0].steer = lambda text: steered.append(text) or True
        assert await rig.inlet._tool("steer_task", {"text": "use the scientific mode"}, OWNER) == {"ok": True}
        rig.agents[0].release.set()
        await settle()
        loop_task.cancel()
        await asyncio.gather(loop_task, return_exceptions=True)

    asyncio.run(own())
    assert steered == ["use the scientific mode"]


def test_attack_a_chief_steps_sentence_never_enters_the_history_as_eves(tmp_path: Path) -> None:
    api = FakeApi()
    rig = Rig(tmp_path, api, FakeCreate(), with_chief=True, evidence=ORDERED)

    async def during() -> None:
        await rig.take(dict(ACT_JOB), ACT_SAID)
        rig.chief.approve("c1")
        await rig.chief.tick()
        await settle()
        await rig.chief.tick()
        await settle()

    run_rig(rig, during)
    history = rig.history()
    assert not any("the order went through" in text for _, text in history)
    assert ("eve", "(Step 1 of c1 ended.)") in history
