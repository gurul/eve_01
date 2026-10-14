"""profile.apply under EVE_PROFILE=lite: each switch eve lite forces reads as off (or as lite's one choice) through its
own module's reader, secrets are removed but the OpenRouter key, the defaults fill only what is unset, the log line
carries names only, and the tool filter keeps exactly lite's tools (docs/plans/eve-lite.md M1 and M2, WP-2).

Every forced switch is checked twice with the module's own configured()/enabled(): in an environment that turns it
on (the positive control: the reader can see "on"), and in that same environment with lite's value on top.
"""

from __future__ import annotations

import ast
import importlib.util
import logging
import os
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Callable, Mapping
from unittest import mock

import pytest

from eve_bridge import (
    agentcard,
    america,
    app_registry,
    apps_mcp,
    browser_lane,
    canvas,
    chat_log,
    chief,
    codex_warm,
    composio_tools,
    computer_agent,
    desk_lane,
    ears,
    explore,
    firetv,
    grade_router,
    jev,
    lights,
    listen_key,
    live_expressions,
    mac_camera,
    meet,
    mem0_memory,
    memory,
    memory_bus,
    menu_tools,
    miniapp,
    monid,
    notify,
    obsbot_panel,
    obsbot_usb,
    opentable,
    phone_call,
    phone_location,
    pika,
    profile,
    scene,
    science_search,
    search_router,
    second_brain,
    share,
    small_talk,
    spotify,
    stick_link,
    telegram,
    think,
    toolset_router,
    transcripts,
    updates,
    version_check,
    watch,
    web_reader,
    websearch,
    work_sessions,
)

SRC = Path(__file__).resolve().parents[1] / "src" / "eve_bridge"
CANARY = "CANARY-NOT-A-KEY-7f3c"


@pytest.fixture(autouse=True)
def _a_home_of_its_own(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Readers that look for a file under HOME (the Connect sign-in, the chat log) find this test's own."""
    monkeypatch.setenv("HOME", str(tmp_path))


@pytest.fixture(autouse=True)
def _installed_extras(monkeypatch: pytest.MonkeyPatch) -> None:
    """Playwright and curl_cffi as importable and an OBSBOT camera as plugged in, whatever this venv has: so the
    watcher's browser and TLS switches, and the camera's, can read as on in the controls. Nothing is imported."""
    real = importlib.util.find_spec

    def find_spec(name: str, *args: Any, **kwargs: Any) -> Any:
        return SimpleNamespace(name=name) if name in ("playwright", "curl_cffi") else real(name, *args, **kwargs)

    monkeypatch.setattr(importlib.util, "find_spec", find_spec)
    monkeypatch.setattr(obsbot_usb, "present", lambda *a, **k: True)


def process_env(reader: Callable[[], Any]) -> Callable[[Mapping[str, str]], Any]:
    """A reader that reads os.environ only, given an environment of its own for the call."""
    def read(env: Mapping[str, str]) -> Any:
        with mock.patch.dict(os.environ, dict(env), clear=True):
            return reader()
    return read


def inline(module: str, name: str) -> Callable[[Mapping[str, str]], Any]:
    """A switch read inline, with no configured() to call (daemon.py's EVE_LEARNING): the comparison that reads it,
    compiled from the module's own source and evaluated with the environment given as os.environ."""
    path = SRC / f"{module}.py"
    for node in ast.walk(ast.parse(path.read_text(encoding="utf-8"))):
        if isinstance(node, ast.Compare) and any(isinstance(n, ast.Constant) and n.value == name for n in ast.walk(node)):
            code = compile(ast.Expression(body=node), str(path), "eval")
            return lambda env: eval(code, {"os": SimpleNamespace(environ=dict(env))})  # noqa: S307 — the module's own
    raise AssertionError(f"{name} is not read inline in {path.name}")


def command_risk(env: Mapping[str, str]) -> str:
    """daemon.py's Auto Mode gate, built the way the daemon builds it: its mode, or "off"."""
    from eve_bridge.daemon import Daemon

    built = process_env(lambda: Daemon._command_risk(SimpleNamespace()))(env)
    return built[0] if built else "off"


KEY = {"OPENROUTER_API_KEY": CANARY}
TG = {"EVE_TELEGRAM": "1", "EVE_TELEGRAM_TOKEN": CANARY, "EVE_TELEGRAM_OWNER": "4242"}

# switch -> (its module's reader, what it reads under lite's value, an environment that turns it on)
CHECKS: dict[str, tuple[Callable[[Mapping[str, str]], Any], Any, dict[str, str]]] = {
    # doors
    "EVE_TELEGRAM": (lambda e: telegram.configured(e).app_only, True, {**TG}),
    "EVE_MINIAPP": (lambda e: miniapp.configured(e).enabled, False, {"EVE_MINIAPP": "1", **TG, **KEY}),
    "EVE_LINK_RENDEZVOUS": (stick_link.rendezvous_base, "", {}),
    "EVE_NOTIFY": (lambda e: notify.configured(e).enabled, False, {}),
    # routing
    "EVE_JEV_ROUTE": (lambda e: jev.route_config(e)[0], jev.ROUTES["openrouter"][0],
                      {"TYPESAFE_API_KEY": CANARY, **KEY}),
    "EVE_TOOLSET_ROUTER": (toolset_router.mode, "off", {}),
    "EVE_GRADE_ROUTER": (grade_router.mode, "off", {}),
    "EVE_SEARCH_ROUTER": (search_router.mode, "off", {}),
    "EVE_MENU_TOOLS": (menu_tools.enabled, False, {"EVE_MENU_TOOLS": "on"}),
    # memory
    "EVE_MEMORY": (lambda e: transcripts.configured(e).enabled, True, {"EVE_MEMORY": "0"}),
    "EVE_MEM0_EMBEDDER": (mem0_memory.embedder_of, "openrouter", {"EVE_MEM0_EMBEDDER": "gemma"}),
    "EVE_SECOND_BRAIN": (lambda e: second_brain.configured(e).enabled, False, {"EVE_SECOND_BRAIN": "1"}),
    "EVE_CLAUDE_MEM": (lambda e: memory_bus.configured(e).claude_mem, False, {"EVE_CLAUDE_MEM": "1"}),
    "EVE_ROSBRIDGE": (lambda e: memory_bus.configured(e).rosbridge, False, {"EVE_ROSBRIDGE": "1"}),
    # apps
    "EVE_APPS": (lambda e: apps_mcp.configured(e).enabled, False, {"EVE_APPS": "1"}),
    "EVE_COMPOSIO": (lambda e: composio_tools.configured(e, frozenset({profile.LITE_CHAT_ID})).enabled, False,
                     {"EVE_COMPOSIO": "1", "COMPOSIO_API_KEY": CANARY}),
    "EVE_COMPOSIO_POLICY": (lambda e: composio_tools.toolkit_policy(e)["gmail"], "auto", {}),
    "EVE_SHARE": (lambda e: share.configured(e).enabled, False, {"EVE_SHARE": "1"}),
    # web
    "EVE_WEB_READER": (lambda e: web_reader.configured(e).enabled, False,
                       {"EVE_WEB_READER": "1", "TINYFISH_API_KEY": CANARY}),
    "EVE_MONID": (monid.mode, "off", {}),
    "EVE_WATCH_HOSTED": (lambda e: watch.configured(e).hosted, False, {"TINYFISH_API_KEY": CANARY}),
    "EVE_WATCH_BROWSER": (lambda e: watch.configured(e).browser, False, {"EVE_WATCH_BROWSER": "1"}),
    "EVE_WATCH_TLS": (lambda e: watch.configured(e).tls, False, {"EVE_WATCH_TLS": "1"}),
    # the Mac
    "EVE_VOICE": (lambda e: ears.configured(e).enabled, False, {}),
    "EVE_CALL_LIVE_STT": (lambda e: phone_call.make_voice(e).live, False,
                          {"EVE_CALL_LIVE_STT": "1", "OPENAI_API_KEY": CANARY, **KEY}),
    "EVE_LISTEN_KEY": (process_env(listen_key.configured_listen_key), "off", {"EVE_LISTEN_KEY": "option"}),
    "EVE_MAC_CAMERA": (mac_camera.device, None, {}),
    "EVE_SCENE": (lambda e: scene.configured(e).enabled, False, {}),
    "EVE_EXPLORE": (lambda e: explore.configured(e).enabled, False, {"EVE_EXPLORE": "1"}),
    "EVE_ROBOT": (telegram.robot_enabled, False, {"EVE_ROBOT": "1"}),
    "EVE_COMPUTER_CONTROL": (lambda e: computer_agent.configured(e).enabled, False, {}),
    "EVE_CODEX_PREWARM": (lambda e: codex_warm.configured(e)[0], False, {}),
    "EVE_DESK_LANE": (desk_lane.enabled, False, {}),
    "EVE_BROWSER_ATTACH": (lambda e: browser_lane.configured(e).attach, False, {"EVE_BROWSER_ATTACH": "1"}),
    "EVE_BROWSER_OWN": (lambda e: browser_lane.configured({**e, "EVE_BROWSER_STEEL": "0"}).own, False,  # Steel wins
                        {"EVE_BROWSER_OWN": "1"}),
    "EVE_BROWSER_STEEL": (lambda e: browser_lane.configured(e).steel, False, {"EVE_BROWSER_STEEL": "1"}),
    "EVE_WORK_SESSIONS": (work_sessions.enabled, False, {}),
    "EVE_LEARNING": (inline("daemon", "EVE_LEARNING"), False, {}),
    # devices
    "EVE_LIGHTS": (lights.enabled, False, {}),
    "EVE_SPOTIFY": (spotify.enabled, False, {}),
    "EVE_FIRETV": (firetv.enabled, False, {}),
    "EVE_PIKA": (pika.enabled, False, {"EVE_PIKA": "1"}),
    "EVE_OBSBOT": (obsbot_panel.enabled, False, {}),
    "EVE_PHONE_EXPRESSIONS": (live_expressions.phone_expressions_on, False, {}),
    # other
    "EVE_MEET": (lambda e: meet.configured(e).enabled, False, {}),
    "EVE_AMERICA": (lambda e: america.configured(e).enabled, False, {"EVE_AMERICA": "1"}),
    "EVE_OPENTABLE": (opentable.configured, False, {"EVE_OPENTABLE": "1"}),
    "EVE_AGENTCARD": (lambda e: agentcard.configured(e).enabled, False,
                      {"EVE_AGENTCARD": "1", "AGENTCARD_ACCESS_TOKEN": CANARY}),
    "EVE_CANVAS": (lambda e: canvas.configured(e).enabled, False,
                   {"CANVAS_BASE_URL": "https://school.instructure.example", "CANVAS_API_TOKEN": CANARY}),
    "EVE_CHIEF": (chief.enabled, False, {"EVE_CHIEF": "on"}),
    "EVE_UPDATES": (updates.enabled, False, {}),
    "EVE_PHONE_LOCATION": (phone_location.enabled, False, {}),
    "EVE_COMMAND_RISK": (command_risk, "off", {"EVE_COMMAND_RISK": "shadow", "TYPESAFE_API_KEY": CANARY}),
    "EVE_BRIDGE_NO_UPDATE_CHECK": (process_env(version_check._disabled), True, {}),
}
# Every app in the registry, the ones added later included: its switch alone, as every wiring site reads it.
for _app in app_registry.APPS:
    CHECKS.setdefault(_app.switch, (lambda e, a=_app: app_registry.switched_on(a, e), False, {_app.switch: "1"}))


def owner_env() -> dict[str, str]:
    """Every environment above that turns a switch on, at once: an owner-shaped eve with every door, key and lane."""
    env: dict[str, str] = {}
    for _reader, _lite, on in CHECKS.values():
        env.update(on)
    env.update({"EVE_MEMORY": "0", "EVE_CHIEF": "on", "EVE_MEM0_EMBEDDER": "gemma", "EVE_LISTEN_KEY": "option"})
    return env


def lite_env() -> dict[str, str]:
    env = {**owner_env(), "EVE_PROFILE": "lite", "HOME": os.environ["HOME"]}
    profile.apply(env)
    return env


@pytest.mark.parametrize("name", sorted(profile.LITE_FORCED))
def test_each_forced_switch_reads_as_lite_in_its_own_module(name: str) -> None:
    assert name in CHECKS, f"{name} is forced but nothing here reads it through its module"
    reader, lite_value, on = CHECKS[name]
    assert reader(on) != lite_value, f"control: {name} should read as on here"
    assert reader({**on, name: profile.LITE_FORCED[name]}) == lite_value


def test_every_reader_reads_lite_from_the_applied_environment() -> None:
    """The whole owner environment through apply(): every switch reads as lite's at once (keys removed too)."""
    env = lite_env()
    wrong = {name: reader(env) for name, (reader, lite_value, _on) in CHECKS.items() if reader(env) != lite_value}
    assert wrong == {}
    owner = owner_env()
    assert all(reader(owner) != lite_value for reader, lite_value, _on in CHECKS.values())   # control


def test_apply_reports_what_it_changed_and_runs_once() -> None:
    env = {**owner_env(), "EVE_PROFILE": "lite", "HOME": "/Users/someone"}
    changed = profile.apply(env)
    assert set(changed) >= {"EVE_TELEGRAM", "EVE_MINIAPP", "EVE_BRIDGE_SOCK", "TINYFISH_API_KEY", "EVE_TELEGRAM_TOKEN"}
    assert changed == sorted(changed) and "OPENROUTER_API_KEY" not in changed
    again = dict(env)
    assert profile.apply(again) == [] and again == env              # already lite: nothing to do


def test_the_kept_switches_keep_their_defaults() -> None:
    """EVE_WATCH, EVE_THINK, EVE_SMALL_TALK, EVE_SCIENCE_SEARCH, EVE_WEB_SEARCH and EVE_CHAT_LOG are left alone, and
    lite's environment has each feature on."""
    kept = ("EVE_WATCH", "EVE_THINK", "EVE_SMALL_TALK", "EVE_SCIENCE_SEARCH", "EVE_WEB_SEARCH", "EVE_CHAT_LOG")
    assert not set(kept) & set(profile.LITE_FORCED)
    env = {"EVE_PROFILE": "lite", "HOME": os.environ["HOME"], **KEY}
    profile.apply(env)
    assert not set(kept) & set(env)
    assert watch.configured(env).enabled and think.configured(env).enabled and chat_log.configured(env).enabled
    assert science_search.enabled(env) and small_talk.enabled(env) is small_talk.SHIPPED
    assert websearch.configured(env).engine == websearch.DEFAULT_ENGINE == "openrouter-perplexity"


# ---- secrets and defaults -----------------------------------------------------------------------------------------

def test_secrets_are_removed_but_the_openrouter_key() -> None:
    secrets = ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "COMPOSIO_API_KEY", "TINYFISH_API_KEY", "EVE_TELEGRAM_TOKEN",
               "EVE_GITHUB_MCP_TOKEN", "EVE_GOOGLE_MAPS_KEY", "SPOTIFY_CLIENT_ID", "SPOTIFY_CLIENT_SECRET",
               "SOME_SERVICE_PASSWORD", "lowercase_token", "EVE_TELEGRAM_OWNER"]
    plain = {"HOME": "/Users/someone", "PATH": "/usr/bin:/bin", "CANVAS_BASE_URL": "https://x.example",
             "EVE_WATCH_RATE": "6", "TOKEN_FILE": "/tmp/x", "EVE_KEY_METHOD": "post"}
    env = {"EVE_PROFILE": "lite", **plain, **KEY, **dict.fromkeys(secrets, CANARY), "EVE_LISTEN_KEY": "option"}
    removed = set(profile.apply(env)) - set(profile.LITE_FORCED) - set(profile.LITE_DEFAULTS)
    assert removed == set(secrets)
    assert not set(secrets) & set(env)
    assert env["OPENROUTER_API_KEY"] == CANARY                         # the allowlist
    assert {k: env[k] for k in plain} == plain                         # not secret-shaped: untouched
    assert env["EVE_LISTEN_KEY"] == "off"                              # ends in _KEY, and is forced: forced wins


def test_secret_shape() -> None:
    for name in ("X_KEY", "X_TOKEN", "X_SECRET", "X_PASSWORD", "X_CLIENT_ID", "x_api_key", "EVE_TELEGRAM_OWNER"):
        assert profile.is_secret_name(name), name
    for name in ("KEYCHAIN", "TOKEN_FILE", "EVE_KEY_METHOD", "PATH", "EVE_TELEGRAM", "CLIENT_ID_FILE"):
        assert not profile.is_secret_name(name), name
    assert profile.LITE_SECRET_ALLOW == {"OPENROUTER_API_KEY"}


def test_the_ipc_socket_default_fills_only_what_is_unset() -> None:
    env = {"EVE_PROFILE": "lite", "HOME": "/Users/someone"}
    profile.apply(env)
    assert env["EVE_BRIDGE_SOCK"] == "/Users/someone/.eve-lite/ipc.sock"
    env = {"EVE_PROFILE": "lite", "HOME": "/Users/someone", "EVE_BRIDGE_SOCK": " "}
    profile.apply(env)
    assert env["EVE_BRIDGE_SOCK"] == "/Users/someone/.eve-lite/ipc.sock"    # blank is unset: never /tmp's socket
    env = {"EVE_PROFILE": "lite", "HOME": "/Users/someone", "EVE_BRIDGE_SOCK": "/Users/someone/elsewhere.sock"}
    assert "EVE_BRIDGE_SOCK" not in profile.apply(env)
    assert env["EVE_BRIDGE_SOCK"] == "/Users/someone/elsewhere.sock"


def test_the_log_line_carries_names_only(caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level(logging.INFO, logger="eve_bridge.profile")
    env = {"EVE_PROFILE": "lite", "HOME": "/Users/someone", "TINYFISH_API_KEY": CANARY, "EVE_MINIAPP": "1", **KEY}
    profile.apply(env)
    lines = [r.getMessage() for r in caplog.records if r.name == "eve_bridge.profile"]
    assert len(lines) == 1
    assert "TINYFISH_API_KEY" in lines[0] and "EVE_BRIDGE_SOCK" in lines[0]
    assert CANARY not in lines[0] and "/Users/someone" not in lines[0]


# ---- the tools eve lite has -----------------------------------------------------------------------------------------

def test_every_lite_tool_name_is_a_real_tool() -> None:
    real = (set(telegram.TOOL_NAMES) | {telegram.RECENT_MESSAGES_TOOL["name"]} | {t["name"] for t in memory.VOICE_TOOLS}
            | set(watch.TOOL_NAMES) | set(science_search.TOOL_NAMES) | {websearch.TOOL_NAME})
    assert profile.LITE_TOOL_NAMES <= real, sorted(profile.LITE_TOOL_NAMES - real)
    assert profile.LITE_TOOL_PREFIXES == ("COMPOSIO_",)


def test_the_tool_filter() -> None:
    for name in profile.LITE_TOOL_NAMES | {"COMPOSIO_SEARCH_TOOLS", "COMPOSIO_MANAGE_CONNECTIONS"}:
        assert profile.lite_tool_allowed(name), name
    for name in ("screenshot", "start_task", "send_file", "list_files", "read_file", "take_photo",
                 "start_coding_session", "claude_relay", "codex_relay", "COMPOSIO_", "composio_search_tools",
                 "GMAIL_SEND_EMAIL", "", None, 7):
        assert not profile.lite_tool_allowed(name), name
    tools = [{"type": "function", "name": "start_task"}, {"type": "function", "name": "remember"},
             {"type": "web_search"}, {"type": "function", "function": {"name": "COMPOSIO_SEARCH_TOOLS"}},
             {"type": "function"}, {"type": "code_interpreter"}, {"type": "function", "name": "think_hard"}]
    kept = profile.filter_tools(tools)
    expected = [tools[1], tools[2], tools[3], tools[6]]
    assert kept == expected and all(a is b for a, b in zip(kept, expected, strict=True))   # the same objects


def test_the_lite_chat_and_the_contract_names() -> None:
    assert profile.LITE_CHAT_ID == 1 and profile.PROFILE_ENV == "EVE_PROFILE"
    assert set(profile.LITE_DEFAULTS) == {"EVE_BRIDGE_SOCK"}
    assert set(profile.LITE_FORCED) >= {app.switch for app in app_registry.APPS}
