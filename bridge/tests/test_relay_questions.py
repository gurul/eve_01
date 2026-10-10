"""The relay's questions: AskUserQuestion reaches the phone numbered, and every permission dialog is a yes/no."""

from __future__ import annotations

import asyncio
import io
import json
from types import SimpleNamespace
from typing import Any, Optional

import pytest

from eve_bridge import installer
from eve_bridge.daemon import Daemon
from eve_bridge.hooks import permission_request, pretooluse
from eve_bridge.telegram import CLAUDE_ASKS_TITLE, TelegramConfig, TelegramInlet

CFG = TelegramConfig(enabled=True, token="123456:AAsecretTOKENvalue", owner_ids=frozenset({4242}))


def test_question_options_are_numbered() -> None:
    hint = pretooluse._summarize({"questions": [{"question": "Quit all?", "options": [
        {"label": "Keep terminals"}, {"label": "Everything"}]}]})
    assert hint == "Quit all? (1. Keep terminals / 2. Everything)"


class Relay:
    claude = True

    def __init__(self, answer: Optional[str] = "allow") -> None:
        self.relayed: list[tuple[str, str]] = []
        self.asked: list[tuple[str, str]] = []
        self.answer = answer

    def relay_tool_call(self, tool: str, hint: str) -> None:
        self.relayed.append((tool, hint))

    async def decide_permission(self, tool: str, hint: str, cwd: str = "", *, always: bool = False) -> Optional[str]:
        self.asked.append((tool, hint))
        return self.answer


def host(relay: Any) -> SimpleNamespace:
    return SimpleNamespace(_telegram=relay, _ensure_session=lambda req: None,
                           state=SimpleNamespace(note_tool=lambda s, t: None),
                           audit=SimpleNamespace(record=lambda **kw: None))


def test_ask_user_question_is_relayed_and_never_decided() -> None:
    relay = Relay()
    h = host(relay)
    out = asyncio.run(Daemon._handle_pretooluse(h, {"tool_use_id": "t1", "tool_name": "AskUserQuestion",
                                                    "hint": "rm the build? (1. yes / 2. no)"}))
    assert out == {"ok": True}                                   # no decision, and not the rm matcher's
    assert relay.relayed == [("AskUserQuestion", "rm the build? (1. yes / 2. no)")] and relay.asked == []


def test_permission_request_asks_the_phone_when_the_relay_is_on() -> None:
    relay = Relay("deny")
    out = asyncio.run(Daemon._handle_permission_request(host(relay), {"tool_name": "Edit", "hint": "/x/app.py"}))
    assert out == {"ok": True, "decision": "deny"} and relay.asked == [("Edit", "/x/app.py")]


def test_permission_request_without_relay_or_answer_leaves_the_dialog() -> None:
    off = Relay()
    off.claude = False
    assert asyncio.run(Daemon._handle_permission_request(host(off), {"tool_name": "Edit"})) == {"ok": True}
    assert asyncio.run(Daemon._handle_permission_request(host(Relay(None)), {"tool_name": "Edit"})) == {"ok": True}
    q = Relay()
    assert asyncio.run(Daemon._handle_permission_request(host(q), {"tool_name": "AskUserQuestion"})) == {"ok": True}
    assert q.asked == []
    assert asyncio.run(Daemon._handle_permission_request(host(None), {"tool_name": "Edit"})) == {"ok": True}


def run_hook(monkeypatch: Any, resp: Optional[dict[str, Any]]) -> tuple[str, dict[str, Any]]:
    sent: dict[str, Any] = {}

    def post(event: dict[str, Any], timeout: float = 0) -> Optional[dict[str, Any]]:
        sent.update(event)
        return resp

    monkeypatch.setattr(permission_request, "post", post)
    monkeypatch.setattr(permission_request, "read_hook_input",
                        lambda: {"tool_name": "Write", "tool_input": {"file_path": "/x/a.py"}, "session_id": "s"})
    out = io.StringIO()
    monkeypatch.setattr("sys.stdout", out)
    assert permission_request.main() == 0
    return out.getvalue(), sent


def test_permission_hook_output_is_the_shape_claude_code_validates(monkeypatch: Any) -> None:
    text, sent = run_hook(monkeypatch, {"ok": True, "decision": "allow"})
    assert sent["evt"] == "permissionrequest" and sent["hint"] == "/x/a.py"
    assert json.loads(text) == {"hookSpecificOutput": {"hookEventName": "PermissionRequest",
                                                       "decision": {"behavior": "allow"}}}
    text, _ = run_hook(monkeypatch, {"ok": True, "decision": "deny"})
    denied = json.loads(text)["hookSpecificOutput"]["decision"]
    assert denied["behavior"] == "deny" and denied["message"]
    assert run_hook(monkeypatch, {"ok": True})[0] == "" and run_hook(monkeypatch, None)[0] == ""


def test_installer_registers_the_question_and_every_dialog() -> None:
    assert ("PreToolUse", "eve_bridge.hooks.pretooluse", "AskUserQuestion", True) in installer.HOOK_DEFS
    assert ("PermissionRequest", "eve_bridge.hooks.permission_request", "*", True) in installer.HOOK_DEFS


class Api:
    def __init__(self) -> None:
        self.sent: list[tuple[Optional[str], str]] = []

    async def send_message(self, chat_id: int, text: str, title: Optional[str] = None,
                           subtitle: Optional[str] = None, buttons: Any = ()) -> None:
        self.sent.append((title, text))


def test_the_question_goes_numbered_and_the_waiting_echo_is_dropped() -> None:
    async def go() -> Api:
        api = Api()
        clock = [100.0]

        async def no_sleep(s: float) -> None:
            return None

        inlet = TelegramInlet(CFG, api, lambda r: None, clock=lambda: clock[0], sleep=no_sleep)
        inlet.claude = True
        inlet.relay_tool_call("AskUserQuestion", "Quit all? (1. Keep terminals / 2. Everything)")
        await asyncio.gather(*list(inlet._jobs))
        await inlet.relay_notification("permission_prompt", "Claude needs your input", True)   # the echo
        clock[0] += 60
        await inlet.relay_notification("permission_prompt", "Claude needs your input", True)   # a real wait
        return api

    api = asyncio.run(go())
    assert api.sent[0][0] == CLAUDE_ASKS_TITLE and "Reply with the option's number." in api.sent[0][1]
    assert len(api.sent) == 2 and api.sent[1][1] == "Claude needs your input"


@pytest.mark.parametrize("text,for_eve", [
    ("Hey eve how many unread emails are on Gmail", "how many unread emails are on Gmail"),   # live, 2026-09-23
    ("hey eve, stop", "stop"),
    ("eve: look left", "look left"),
    ("Eve, what time is it", "what time is it"),
    ("eve is broken fix it", None),          # a bare "eve" still needs its comma or colon: this is for Claude
    ("the eve thing", None),
])
def test_what_reaches_eve_while_relaying(text: str, for_eve) -> None:
    from eve_bridge.telegram import EVE_PREFIX

    m = EVE_PREFIX.match(text)
    assert (m.group(2) if m else None) == for_eve


def test_a_restart_during_a_texted_task_tells_the_owner() -> None:
    from eve_bridge.telegram import RESTART_LINE

    class Api:
        def __init__(self) -> None:
            self.sent: list[str] = []

        async def send_message(self, chat_id, text, title=None, subtitle=None) -> None:
            self.sent.append(text)

    class Agent:
        cancelled = ""

        def cancel(self, reason: str = "") -> None:
            self.cancelled = reason

    async def go() -> tuple[Api, Agent]:
        api, agent = Api(), Agent()
        inlet = TelegramInlet(CFG, api, lambda r: None)
        inlet._agent = agent
        inlet._agent_task = asyncio.get_running_loop().create_future()      # running
        inlet._task_chat, inlet._task_goal = 4242, "search amazon for AA batteries"
        await inlet._shutdown()
        return api, agent

    api, agent = asyncio.run(go())
    assert agent.cancelled == "the daemon is stopping"
    assert api.sent == [RESTART_LINE.format(goal="search amazon for AA batteries")]
