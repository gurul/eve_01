"""Sound: the owner can mute eve. Muted means silent, not still — the head
and the LEDs keep moving; only sound stops.

Every sound eve makes comes from the board's chirp synth, and the firmware
already gates all of it on its persisted ``sound`` setting (NVS key
``s_snd``). The host sets that setting with ``{"cmd":"sound","on":bool}`` and
re-sends it on every connect, so a reflashed or replaced board comes back
with the owner's choice. The choice itself is kept here, in
``~/.config/eve-bridge/sound.json``, so it survives a daemon restart.

Two host-side belts on top of the firmware switch: caption pages go out with
``"chirp": false`` while muted (older firmware without the command stays
quiet during a conversation), and the voice never plays audio on the Mac
speaker in ``EVE_VOICE_OUTPUT=audio`` mode.
"""

from __future__ import annotations

import json
import logging
import os
from pathlib import Path
from typing import Any, Optional

log = logging.getLogger(__name__)


def default_path(environ: Any = None, name: str = "sound") -> Path:
    """``~/.config/eve-bridge/<name>.json``, or ``EVE_<NAME>_FILE``."""
    env = os.environ if environ is None else environ
    raw = (env.get(f"EVE_{name.upper()}_FILE") or "").strip()
    if raw:
        return Path(raw).expanduser()
    return Path.home() / ".config" / "eve-bridge" / f"{name}.json"


def build_sound_cmd(on: bool) -> dict[str, Any]:
    return {"cmd": "sound", "on": bool(on)}


def quiet_caption(msg: dict[str, Any], muted: bool) -> dict[str, Any]:
    """A caption page without its talk chirp while muted; unchanged otherwise."""
    if muted and msg.get("chirp"):
        return {**msg, "chirp": False}
    return msg


class SoundSetting:
    """On or off, persisted. A missing or unreadable file means on.

    ``name`` is the switch's name in log lines and its default file: the
    daemon keeps two, ``sound`` (mute) and ``mic`` (the owner's microphone
    switch, see ears.py)."""

    def __init__(self, path: Optional[Path] = None, name: str = "sound") -> None:
        self.name = name
        self.path = path or default_path(name=name)
        self.on = True

    @property
    def muted(self) -> bool:
        return not self.on

    def load(self) -> bool:
        try:
            obj = json.loads(self.path.read_text(encoding="utf-8"))
            if not isinstance(obj, dict):
                raise ValueError("expected a JSON object")
            self.on = not bool(obj.get("muted", False))
        except FileNotFoundError:
            self.on = True
        except (OSError, ValueError) as e:
            log.warning("%s: cannot read %s (%s) — %s stays on", self.name, self.path, e, self.name)
            self.on = True
        return self.on

    def set(self, on: bool) -> bool:
        """Change and persist. Returns True when the value changed."""
        changed = bool(on) != self.on
        self.on = bool(on)
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            tmp = self.path.with_suffix(".tmp")
            tmp.write_text(json.dumps({"muted": not self.on}) + "\n", encoding="utf-8")
            os.replace(tmp, self.path)
        except OSError as e:
            log.warning("%s: cannot save %s (%s) — the choice lasts until the daemon restarts", self.name, self.path, e)
        return changed
