"""Fan (eve_bridge/fan.py, docs/fan.md): written by `eve-bridge app new fan`; it passes as generated."""

from __future__ import annotations

import asyncio
import re

import eve_bridge.fan as app_mod
from eve_bridge import app_registry

ON = {"EVE_FAN": "1"}


def test_off_by_default_and_on_with_its_switch() -> None:
    assert app_mod.make({}) is None
    assert app_mod.make({"EVE_FAN": "0"}) is None
    assert app_mod.make(ON) is not None


def test_its_tools_are_its_own() -> None:
    hub = app_mod.make(ON)
    names = tuple(t["name"] for t in hub.tools())
    assert names == app_mod.TOOL_NAMES
    assert all(n.startswith("fan_") for n in names)
    for tool in hub.tools():
        assert tool["parameters"].get("additionalProperties") is False


def test_set_and_status_round_trip() -> None:
    hub = app_mod.make(ON)

    async def go() -> None:
        assert (await hub.handle("fan_status", {}))["on"] is False
        assert (await hub.handle("fan_set", {"on": True}))["ok"] is True
        assert (await hub.handle("fan_status", {}))["on"] is True
        assert (await hub.handle("fan_set", {"on": "yes"}))["ok"] is False

    asyncio.run(go())


def test_an_unknown_tool_is_refused() -> None:
    assert asyncio.run(app_mod.make(ON).handle("fan_explode", {}))["ok"] is False


def test_the_instructions_name_only_its_own_tools() -> None:
    named = set(re.findall(r"\bfan_[a-z_]+", app_mod.make(ON).instructions()))
    assert named and named <= set(app_mod.TOOL_NAMES)


def test_the_phone_sheet_meets_the_contract() -> None:
    hub = app_mod.make(ON)

    async def go() -> None:
        first = await hub.panel({"action": "status"})
        assert app_registry.check_panel(first) == [] and first["status"] == "Off"
        turned = await hub.panel({"action": "power", "on": True})
        assert app_registry.check_panel(turned) == [] and turned["status"] == "On"
        assert (await hub.handle("fan_status", {}))["on"] is True
        assert (await hub.panel({"action": "nonsense"}))["ok"] is False

    asyncio.run(go())


def test_it_is_registered() -> None:
    app = app_registry.get("fan")
    assert app is not None and app_registry.check(app) == []
    assert "fan_set" in app.acting and app.switch == "EVE_FAN" and not app.default_on
