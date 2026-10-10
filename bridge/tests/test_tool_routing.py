"""TelegramInlet._tool routing, pinned: which handler each tool name reaches, and with what arguments.

Recorded against the if-chain before the 2026-09-23 dispatch refactor; the registry that replaced it must
route every name the same way. Handlers are replaced by recorders, so nothing real runs.
"""

from __future__ import annotations

import asyncio
from typing import Any

import pytest

from eve_bridge import second_brain, telegram, websearch
from eve_bridge.telegram import TelegramConfig, TelegramInlet

OWNER = 4242
CFG = TelegramConfig(enabled=True, token="123456:AAsecretTOKENvalue", owner_ids=frozenset({OWNER}))


class Agent:
    def __init__(self) -> None:
        self.calls: list[Any] = []

    def steer(self, text: str) -> bool:
        self.calls.append(("steer", text))
        return True

    def cancel(self, reason: str = "") -> None:
        self.calls.append(("cancel", reason))


class FakeMemory:
    """memory.Memory's tool half: the four tool names the text brain is offered, and a recorder for handle_tool."""

    NAMES = ("memory_search", "memory_read", "forget_preview", "forget_apply")

    def __init__(self, hits: list[Any]) -> None:
        self.hits = hits

    def tools(self, *, voice: bool = False) -> list[dict[str, Any]]:
        return [{"type": "function", "name": n, "parameters": {}} for n in self.NAMES]

    def handle_tool(self, name: str, args: dict[str, Any], *, since: Any = None, channel: str = "") -> dict[str, Any]:
        self.hits.append(("memory.handle_tool", name, dict(args), channel))
        return {"ok": True}


def _rig(monkeypatch: pytest.MonkeyPatch, *, agent: bool) -> tuple[TelegramInlet, list[Any]]:
    hits: list[Any] = []

    async def no_model(request: dict[str, Any]) -> dict[str, Any]:
        raise AssertionError("no model call in a routing test")

    memory = FakeMemory(hits)
    apps = type("A", (), {"names": frozenset({"GMAIL_FETCH_EMAILS"})})()
    vault = type("V", (), {"root": "/vault"})()
    inlet = TelegramInlet(CFG, object(), no_model, memory=memory, apps=apps, vault=vault)

    def rec(label: str) -> Any:
        async def handler(*a: Any, **kw: Any) -> dict[str, Any]:
            hits.append((label, *[x for x in a if not isinstance(x, int)]))
            return {"ok": True, "via": label}
        return handler

    inlet._start_task = lambda goal, chat_id: hits.append(("_start_task", goal)) or {"ok": True}
    inlet._take_photo = rec("_take_photo")
    inlet._send_screen = rec("_send_screen")
    inlet._send_file = rec("_send_file")
    inlet._robot_tool = rec("_robot_tool")
    inlet._app_tool = rec("_app_tool")
    inlet._think_hard = rec("_think_hard")
    inlet._launch_step = rec("_launch_step")
    monkeypatch.setattr(telegram, "list_files", lambda p: hits.append(("list_files", p)) or {"ok": True})
    monkeypatch.setattr(websearch, "search", lambda q, cfg: hits.append(("websearch.search", q)) or {"ok": True})
    monkeypatch.setattr(second_brain, "dispatch", lambda root, n, a: hits.append(("second_brain.dispatch", root, n)) or {"ok": True})
    if agent:
        inlet._agent = Agent()
        inlet._agent_task = asyncio.get_event_loop_policy().new_event_loop().create_future()   # not done: running
    return inlet, hits


ROUTES = [
    ("start_task", {"goal": "open notes"}, [("_start_task", "open notes")]),
    ("take_photo", {"note": "the desk"}, [("_take_photo", "the desk")]),
    ("screenshot", {"caption": "now"}, [("_send_screen", "now")]),
    ("send_file", {"path": "~/a.txt", "caption": "c"}, [("_send_file", "~/a.txt", "c")]),
    ("list_files", {"path": "~/Desktop"}, [("list_files", "~/Desktop")]),
    ("look", {}, [("_robot_tool", "look", {})]),
    ("look_around", {}, [("_robot_tool", "look_around", {})]),
    ("find", {"target": "mug"}, [("_robot_tool", "find", {"target": "mug"})]),
    ("move_head", {"yaw": 10}, [("_robot_tool", "move_head", {"yaw": 10})]),
    ("go_explore", {}, [("_robot_tool", "go_explore", {})]),
    ("set_sound", {"on": False}, [("_robot_tool", "set_sound", {"on": False})]),
    ("remember", {"claim": "x"}, [("_robot_tool", "remember", {"claim": "x"})]),
    ("take_notes", {"action": "start"}, [("_robot_tool", "take_notes", {"action": "start"})]),
    ("memory_search", {"query": "q"}, [("memory.handle_tool", "memory_search", {"query": "q"}, "telegram")]),
    ("memory_read", {"ref": "7"}, [("memory.handle_tool", "memory_read", {"ref": "7"}, "telegram")]),
    ("forget_preview", {"query": "q"}, [("memory.handle_tool", "forget_preview", {"query": "q"}, "telegram")]),
    ("forget_apply", {"token": "t"}, [("memory.handle_tool", "forget_apply", {"token": "t"}, "telegram")]),
    (websearch.TOOL_NAME, {"query": "ramen"}, [("websearch.search", "ramen")]),
    ("GMAIL_FETCH_EMAILS", {"q": 1}, [("_app_tool", "GMAIL_FETCH_EMAILS", {"q": 1})]),
    ("think_hard", {"question": "why"}, [("_think_hard", "why")]),
    ("something_unknown", {"question": "q"}, [("_think_hard", "q")]),
]


@pytest.mark.parametrize("name,args,expected", ROUTES, ids=[r[0] for r in ROUTES])
def test_each_tool_reaches_its_handler(monkeypatch: pytest.MonkeyPatch, name: str, args: dict, expected: list) -> None:
    inlet, hits = _rig(monkeypatch, agent=False)
    result = asyncio.run(inlet._tool(name, args, OWNER))
    assert hits == expected and result.get("ok") is True


@pytest.mark.parametrize("name", sorted(second_brain.SECOND_BRAIN_TOOL_NAMES))
def test_second_brain_tools_reach_the_vault(monkeypatch: pytest.MonkeyPatch, name: str) -> None:
    inlet, hits = _rig(monkeypatch, agent=False)
    asyncio.run(inlet._tool(name, {}, OWNER))
    assert hits == [("second_brain.dispatch", "/vault", name)]


def test_steer_and_stop_without_and_with_a_running_task(monkeypatch: pytest.MonkeyPatch) -> None:
    inlet, _ = _rig(monkeypatch, agent=False)
    assert asyncio.run(inlet._tool("steer_task", {"text": "left"}, OWNER)) == {"ok": False, "reason": "no task is running"}
    assert asyncio.run(inlet._tool("stop_task", {}, OWNER)) == {"ok": False, "reason": "no task is running"}
    inlet, _ = _rig(monkeypatch, agent=True)
    assert asyncio.run(inlet._tool("steer_task", {"text": "left"}, OWNER)) == {"ok": True}
    assert asyncio.run(inlet._tool("stop_task", {}, OWNER)) == {"ok": True}
    assert inlet._agent.calls == [("steer", "left"), ("cancel", "stopped from Telegram")]


def test_start_coding_session_asks_the_flow_and_spawns_its_step(monkeypatch: pytest.MonkeyPatch, tmp_path) -> None:
    (tmp_path / "work" / "era-maker").mkdir(parents=True)
    inlet, hits = _rig(monkeypatch, agent=False)
    inlet._launch_root, inlet._launch_recent = tmp_path, lambda: []

    async def go() -> dict[str, Any]:
        result = await inlet._tool("start_coding_session", {"area": "work", "folder": "", "harness": ""}, OWNER)
        await asyncio.gather(*list(inlet._jobs))
        return result

    result = asyncio.run(go())
    assert result["ok"] is True and hits and hits[0][0] == "_launch_step"
    assert inlet._launch is not None                          # the question is open: the next reply answers it


def test_a_failing_handler_is_reported_not_raised(monkeypatch: pytest.MonkeyPatch) -> None:
    inlet, _ = _rig(monkeypatch, agent=False)

    async def boom(*a: Any) -> dict[str, Any]:
        raise RuntimeError("x")

    inlet._take_photo = boom
    assert asyncio.run(inlet._tool("take_photo", {"note": "n"}, OWNER)) == {"ok": False, "reason": "take_photo failed"}


def test_memory_tools_without_memory_never_reach_a_memory(monkeypatch: pytest.MonkeyPatch) -> None:
    """Memory off: the memory tools are not offered, and a stray call is the catch-all's, never a store's."""
    inlet, hits = _rig(monkeypatch, agent=False)
    inlet._memory, inlet._memory_tools_cache = None, None
    asyncio.run(inlet._tool("memory_search", {"query": "q"}, OWNER))
    assert hits == [("_think_hard", "")]
