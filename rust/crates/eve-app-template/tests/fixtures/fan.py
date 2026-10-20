"""Fan: an app for eve (docs/fan.md), written by `eve-bridge app new fan`.

It is off unless ``EVE_FAN=1``. On, it gives the chat and the voice two tools, ``fan_status`` (a read) and
``fan_set`` (an act: it changes something, so the tool gate treats it as one), and the phone a sheet with a
power toggle and a button (eve's generic sheet; no Swift needed). The state lives in memory: it runs with no device,
so its tests pass as written. Replace the marked parts with the real device code.
"""

from __future__ import annotations

import logging
import os
from typing import Any, Mapping, Optional

from . import app_registry

log = logging.getLogger(__name__)

SWITCH = "EVE_FAN"
OFF_REASON = "Fan is off on this Mac (EVE_FAN)."

TOOLS: list[dict[str, Any]] = [
    {
        "type": "function",
        "name": "fan_status",
        "description": "Whether the owner's fan is on.",
        "parameters": {"type": "object", "properties": {}, "additionalProperties": False},
    },
    {
        "type": "function",
        "name": "fan_set",
        "description": "Turn the owner's fan on or off.",
        "parameters": {
            "type": "object",
            "properties": {"on": {"type": "boolean", "description": "true turns it on, false turns it off"}},
            "required": ["on"],
            "additionalProperties": False,
        },
    },
]
TOOL_NAMES = tuple(t["name"] for t in TOOLS)

# What the model reads about this app. Name only this app's own tools (AGENTS.md rule 7).
INSTRUCTIONS = """The owner's fan. fan_status says whether it is on; fan_set turns it on or off."""


def enabled(environ: Optional[Mapping[str, str]] = None) -> bool:
    """Off unless EVE_FAN is 1, true, yes or on: a new app changes nothing the models read until it is turned on."""
    env = os.environ if environ is None else environ
    return str(env.get(SWITCH, "")).strip().lower() in ("1", "true", "yes", "on")


class Fan:
    """The app's hub: the tools, the phone's panel, and close. One is made per daemon (make)."""

    def __init__(self) -> None:
        # The real device goes here: its address, a client, a connection opened on first use. Learn how the
        # device's own app talks to it with REA (https://github.com/morluto/rea), and only for a device you own
        # and may inspect. Firmware on a board: log it with hwlog (https://github.com/gurul/hardware-logging).
        self.on = False

    def tools(self) -> list[dict[str, Any]]:
        return [dict(t) for t in TOOLS]

    def instructions(self) -> str:
        return INSTRUCTIONS

    def _said(self) -> str:
        return f"The fan is {'on' if self.on else 'off'}."

    async def _set(self, on: bool) -> None:
        # The real device call goes here (and its failure becomes {"ok": False, "reason": ...}).
        self.on = on

    async def handle(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        """One tool call from the chat or the voice."""
        if name == "fan_status":
            return {"ok": True, "on": self.on, "line": self._said()}
        if name == "fan_set":
            on = args.get("on")
            if not isinstance(on, bool):
                return {"ok": False, "reason": "on must be true or false"}
            await self._set(on)
            return {"ok": True, "on": self.on, "line": self._said()}
        return {"ok": False, "reason": f"unknown tool {name}"}

    async def panel(self, body: dict[str, Any]) -> dict[str, Any]:
        """The phone's generic sheet (POST /api/link/controls/fan): status, a power toggle and a button."""
        action = str(body.get("action") or "status")
        line = ""
        if action == "power":
            on = body.get("on")
            if not isinstance(on, bool):
                return app_registry.panel(ok=False, line="Power needs on or off.")
            await self._set(on)
            line = self._said()
        elif action == "hello":
            line = "Hello from Fan."
        elif action != "status":
            return app_registry.panel(ok=False, line="That isn't something Fan does.")
        return app_registry.panel(
            status="On" if self.on else "Off",
            line=line,
            controls=[
                app_registry.toggle("power", "Power", self.on, "power"),
                app_registry.button("hello", "Say hello", "hello", symbol="hand.wave"),
            ],
        )

    async def close(self) -> None:
        """The daemon stops: let go of the device."""


def make(environ: Optional[Mapping[str, str]] = None) -> Optional[Fan]:
    """The app, or None when EVE_FAN is off (then no tool is lent and the phone shows no row)."""
    if not enabled(environ):
        log.info("fan: off (EVE_FAN)")
        return None
    return Fan()
