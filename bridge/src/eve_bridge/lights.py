"""The owner's lights: Govee and WiZ over the LAN, HappyLighting over Bluetooth, Tuya — one controller.

The owner asked on 2026-09-27 to control their lights from eve (WiZ added 2026-09-28). Each speaks its own protocol, and none
needs a cloud or a hub once set up:

==============  ==========================================================  =========================================
Brand           How eve talks to it                                       What setup needs
==============  ==========================================================  =========================================
Govee           Govee's LAN API: JSON over UDP (scan on multicast           "LAN Control" on for the light in the
                239.255.255.250:4001, replies to :4002, commands to :4003)  Govee app. No key.
WiZ            WiZ's local API: JSON over UDP port 38899 (``getPilot``,      Newer firmware refuses unsigned changes:
                ``setPilot``); found by a broadcast ``getPilot``            the home's signing key, from the WiZ
                                                                            app's local-integration export
                                                                            (``lights wiz-import``, docs/lights.md).
HappyLighting   Bluetooth LE, the "Triones" protocol QH-tek's controllers   Within Bluetooth range of the Mac. No key.
                speak (``QHM-…`` names): write ``ffd9``, status on ``ffd4``
Sylvania Smart+ Tuya's local protocol through tinytuya (the ``lights``      The device's local key, once, from Tuya's
(Wi-Fi)         extra), on the LAN                                          developer platform (docs/lights.md).
==============  ==========================================================  =========================================

The lights live in ``~/.config/eve-bridge/lights.json`` (mode 600: a Tuya key is a secret), which
``eve-bridge lights scan`` fills and ``lights name`` edits. Names, rooms and addresses stay on the Mac, never
in the repo.

Three doors use one ``Lights``:

* **Telegram** (and everything that rides its brain: the chat window, Mini App calls, the Voice PE's hold to
  talk). A message that is **only** a light command ("lights off", "floor lamp blue", "dim the bedroom to 30%")
  is done by code, no model turn (``Lights.match``), like quick_answers.py. Anything else ("make it cozy in
  here", "turn the lights off in ten minutes") goes to the model, which has ``lights_set`` and ``lights_status``.
* **The robot's live voice** (voice_agent.py) gets the same two tools.

``EVE_LIGHTS=0`` turns it all off. With no lights file, or an empty one, it is off too, and no tool is lent.

HappyLighting over Bluetooth from the daemon: macOS grants Bluetooth to the launchd job's Python itself. A
terminal that has no Bluetooth permission is killed by macOS the moment it scans (exit 134), so ``lights scan``
from such a terminal takes ``--no-ble``, and the daemon (or a terminal with the permission) finds the controller.
"""

from __future__ import annotations

import asyncio
import base64
import colorsys
import hashlib
import hmac
import json
import logging
import math
import os
import re
import socket
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Iterable, Iterator, Optional, Sequence

log = logging.getLogger(__name__)

CONFIG_PATH = Path("~/.config/eve-bridge/lights.json")
OFF_REASON = "eve has no lights on this computer (EVE_LIGHTS, ~/.config/eve-bridge/lights.json)"
KINDS = ("govee", "wiz", "triones", "tuya")
LIGHT_TIMEOUT_SECS = 12.0          # one light's whole change; a Bluetooth connect alone can take several seconds

# ---- colour ---------------------------------------------------------------------------------------------------

COLORS: dict[str, tuple[int, int, int]] = {
    "red": (255, 0, 0), "orange": (255, 100, 0), "amber": (255, 150, 0), "yellow": (255, 210, 0),
    "gold": (255, 180, 0), "lime": (160, 255, 0), "green": (0, 255, 0), "mint": (60, 255, 140),
    "teal": (0, 200, 160), "cyan": (0, 255, 255), "turquoise": (0, 220, 200), "sky blue": (80, 180, 255),
    "blue": (0, 0, 255), "navy": (0, 0, 140), "indigo": (75, 0, 255), "purple": (140, 0, 255),
    "violet": (170, 60, 255), "lavender": (190, 150, 255), "magenta": (255, 0, 255), "pink": (255, 80, 160),
    "hot pink": (255, 20, 120), "rose": (255, 60, 110), "coral": (255, 110, 80), "salmon": (255, 120, 100),
    "peach": (255, 170, 110),
}
# White, by colour temperature. "white" alone is a neutral white.
WHITES: dict[str, int] = {
    "candle": 2000, "candlelight": 2000, "warm": 2700, "warm white": 2700, "soft white": 3000, "white": 4000,
    "neutral": 4000, "neutral white": 4000, "cool": 5500, "cool white": 5500, "daylight": 6500,
}
KELVIN_MIN, KELVIN_MAX = 1500, 9000
_HEX = re.compile(r"^#?([0-9a-f]{6})$")
_KELVIN = re.compile(r"^(\d{4,5})\s*k$")


@dataclass(frozen=True)
class Color:
    """What the owner asked for: an RGB colour, or a white at a colour temperature."""
    rgb: Optional[tuple[int, int, int]] = None
    kelvin: Optional[int] = None
    word: str = ""

    def as_rgb(self) -> tuple[int, int, int]:
        return self.rgb if self.rgb is not None else kelvin_to_rgb(self.kelvin or 4000)


def parse_color(text: str) -> Optional[Color]:
    """A colour name, a ``#rrggbb``, a white ("warm white", "daylight") or a temperature ("2700K"). None otherwise."""
    t = re.sub(r"\s+", " ", (text or "").strip().lower())
    if t.endswith(" light"):
        t = t[: -len(" light")]
    if t in COLORS:
        return Color(rgb=COLORS[t], word=t)
    if t in WHITES:
        return Color(kelvin=WHITES[t], word=t)
    m = _HEX.match(t)
    if m:
        h = m.group(1)
        return Color(rgb=(int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16)), word="#" + h)
    m = _KELVIN.match(t)
    if m:
        return Color(kelvin=max(KELVIN_MIN, min(KELVIN_MAX, int(m.group(1)))), word=f"{m.group(1)}K")
    return None


def kelvin_to_rgb(kelvin: int) -> tuple[int, int, int]:
    """An RGB that looks like a white of this temperature (Tanner Helland's fit), for lights with no white LED."""
    t = max(KELVIN_MIN, min(KELVIN_MAX, kelvin)) / 100.0
    if t <= 66:
        r = 255.0
        g = 99.4708025861 * math.log(t) - 161.1195681661
        b = 0.0 if t <= 19 else 138.5177312231 * math.log(t - 10) - 305.0447927307
    else:
        r = 329.698727446 * (t - 60) ** -0.1332047592
        g = 288.1221695283 * (t - 60) ** -0.0755148492
        b = 255.0
    return tuple(int(max(0, min(255, round(v)))) for v in (r, g, b))  # type: ignore[return-value]


def scale_rgb(rgb: Sequence[int], percent: int) -> tuple[int, int, int]:
    """The colour's hue at ``percent`` brightness: the brightest channel becomes 255 * percent / 100. Black stays
    black-free: a colour with no light in it is treated as white."""
    top = max(rgb) if rgb else 0
    base = (255, 255, 255) if top <= 0 else tuple(c * 255.0 / top for c in rgb)
    k = max(0, min(100, percent)) / 100.0
    return tuple(int(round(c * k)) for c in base)  # type: ignore[return-value]


def rgb_percent(rgb: Sequence[int]) -> int:
    return int(round(max(rgb) * 100 / 255)) if rgb else 0


def hex_of(rgb: Sequence[int]) -> str:
    return "#" + "".join(f"{int(c):02x}" for c in rgb)


def color_word(rgb: Sequence[int]) -> str:
    """The nearest colour name, by hue, for a status line."""
    r, g, b = (c / 255.0 for c in rgb)
    h, s, v = colorsys.rgb_to_hsv(r, g, b)
    if v < 0.02:
        return "off"
    if s < 0.18:
        return "white"
    best, dist = "", 9.0
    for name, c in COLORS.items():
        h2, s2, _ = colorsys.rgb_to_hsv(*(x / 255.0 for x in c))
        d = min(abs(h - h2), 1 - abs(h - h2)) + 0.2 * abs(s - s2)
        if d < dist:
            best, dist = name, d
    return best


# ---- the change asked for, and a light's state ------------------------------------------------------------------

@dataclass(frozen=True)
class Change:
    power: Optional[bool] = None        # None: leave it (a colour or a brightness turns an off light on)
    brightness: Optional[int] = None    # 1-100
    color: Optional[Color] = None

    @property
    def empty(self) -> bool:
        return self.power is None and self.brightness is None and self.color is None

    def normalised(self) -> "Change":
        """brightness 0 means off; anything above 100 is 100."""
        if self.brightness is not None and self.brightness <= 0:
            return Change(power=False)
        b = None if self.brightness is None else min(100, self.brightness)
        return Change(power=self.power, brightness=b, color=self.color)


@dataclass
class State:
    on: Optional[bool] = None
    brightness: Optional[int] = None
    rgb: Optional[tuple[int, int, int]] = None
    kelvin: Optional[int] = None

    def public(self) -> dict[str, Any]:
        out: dict[str, Any] = {"on": self.on}
        if self.brightness is not None:
            out["brightness"] = self.brightness
        if self.kelvin:
            out["white_kelvin"] = self.kelvin
        elif self.rgb is not None:
            out["color"] = color_word(self.rgb)
            out["rgb"] = hex_of(self.rgb)
        return out


def change_of(state: State) -> Change:
    """The change that puts another light in this state: "match the lights" copies one light onto the rest."""
    if state.on is False:
        return Change(power=False)
    color = None
    if state.kelvin:
        color = Color(kelvin=state.kelvin, word=f"{state.kelvin}K")
    elif state.rgb is not None and any(state.rgb):
        rgb = scale_rgb(state.rgb, 100)     # a Bluetooth controller's level is in its colour: take the hue at full
        color = Color(rgb=rgb, word=color_word(rgb))
    return Change(power=True, brightness=state.brightness or None, color=color)


@dataclass
class Light:
    name: str
    kind: str
    room: str = ""
    aliases: tuple[str, ...] = ()
    # govee and wiz (a WiZ light's device is its MAC, its sku the module name)
    ip: str = ""
    device: str = ""
    sku: str = ""
    # triones (a CoreBluetooth UUID on macOS, a MAC elsewhere)
    address: str = ""
    # tuya (and a WiZ home's UDP signing key, hex)
    id: str = ""
    key: str = ""
    version: str = "3.3"

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "Light":
        kind = str(d.get("kind") or "").strip().lower()
        if kind not in KINDS:
            raise ValueError(f"unknown light kind {kind!r}")
        name = str(d.get("name") or "").strip()
        if not name:
            raise ValueError("a light needs a name")
        return cls(name=name, kind=kind, room=str(d.get("room") or "").strip(),
                   aliases=tuple(str(a).strip() for a in (d.get("aliases") or []) if str(a).strip()),
                   ip=str(d.get("ip") or ""), device=str(d.get("device") or ""), sku=str(d.get("sku") or ""),
                   address=str(d.get("address") or ""), id=str(d.get("id") or ""), key=str(d.get("key") or ""),
                   version=str(d.get("version") or "3.3"))

    def to_dict(self) -> dict[str, Any]:
        out: dict[str, Any] = {"name": self.name, "kind": self.kind}
        if self.room:
            out["room"] = self.room
        if self.aliases:
            out["aliases"] = list(self.aliases)
        fields = {"govee": ("ip", "device", "sku"), "wiz": ("ip", "device", "sku", "key"), "triones": ("address",),
                  "tuya": ("id", "ip", "key", "version")}
        for f in fields[self.kind]:
            if getattr(self, f):
                out[f] = getattr(self, f)
        return out


def load(path: Optional[Path] = None) -> list[Light]:
    """The lights file, or [] when it is missing. A bad entry is skipped with a warning, never fatal."""
    p = (path or CONFIG_PATH).expanduser()
    try:
        raw = json.loads(p.read_text())
    except FileNotFoundError:
        return []
    except (OSError, ValueError) as e:
        log.warning("lights: %s is unreadable (%s)", p, type(e).__name__)
        return []
    out: list[Light] = []
    for d in (raw.get("lights") if isinstance(raw, dict) else None) or []:
        try:
            out.append(Light.from_dict(d))
        except (ValueError, AttributeError, TypeError) as e:
            log.warning("lights: skipping an entry in %s (%s)", p, e)
    return out


def save(lights: Iterable[Light], path: Optional[Path] = None) -> Path:
    p = (path or CONFIG_PATH).expanduser()
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(".tmp")
    tmp.write_text(json.dumps({"lights": [lt.to_dict() for lt in lights]}, indent=2) + "\n")
    os.chmod(tmp, 0o600)
    tmp.replace(p)
    return p


# ---- Govee: the LAN API ----------------------------------------------------------------------------------------

GOVEE_GROUP = "239.255.255.250"
GOVEE_SCAN_PORT, GOVEE_LISTEN_PORT, GOVEE_CMD_PORT = 4001, 4002, 4003
GOVEE_KELVIN = (2000, 9000)


# Every Govee light answers on the one reply port, and with SO_REUSEPORT the kernel hands a unicast reply to
# just one of the sockets bound there: with status("all") reading lights in parallel, one light's reply
# landed in another read's socket and was thrown away as the wrong sender. So one listen at a time.
_GOVEE_LISTEN_LOCK = threading.Lock()


@contextmanager
def _govee_listening(timeout: float) -> Iterator[socket.socket]:
    """The reply-port socket, held alone for as long as the caller listens."""
    with _GOVEE_LISTEN_LOCK, _govee_listener(timeout) as r:
        yield r


def _govee_listener(timeout: float) -> socket.socket:
    r = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    r.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    if hasattr(socket, "SO_REUSEPORT"):
        r.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
    r.bind(("", GOVEE_LISTEN_PORT))
    r.settimeout(timeout)
    return r


def govee_scan(seconds: float = 3.0) -> list[dict[str, str]]:
    """Every Govee light with LAN Control on: ``{"device", "sku", "ip"}``. Blocking."""
    msg = json.dumps({"msg": {"cmd": "scan", "data": {"account_topic": "reserve"}}}).encode()
    found: dict[str, dict[str, str]] = {}
    with _govee_listening(0.3) as r, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 2)
        end, again = time.monotonic() + seconds, 0.0
        while time.monotonic() < end:
            if time.monotonic() >= again:
                s.sendto(msg, (GOVEE_GROUP, GOVEE_SCAN_PORT))
                again = time.monotonic() + 1.0
            try:
                data, addr = r.recvfrom(4096)
            except socket.timeout:
                continue
            try:
                d = json.loads(data)["msg"]["data"]
            except (ValueError, KeyError, TypeError):
                continue
            if d.get("device"):
                # the light's reply carries its address on most models; the packet's sender always does
                found[d["device"]] = {"device": d["device"], "sku": str(d.get("sku") or ""),
                                      "ip": str(d.get("ip") or addr[0])}
    return list(found.values())


def govee_commands(change: Change, current: Optional[State] = None) -> list[dict[str, Any]]:
    """The LAN API messages for a change, in order."""
    c = change.normalised()
    if c.power is False:
        return [{"msg": {"cmd": "turn", "data": {"value": 0}}}]
    out: list[dict[str, Any]] = []
    if c.power is True or not c.empty:
        out.append({"msg": {"cmd": "turn", "data": {"value": 1}}})
    if c.color is not None:
        if c.color.kelvin is not None:
            k = max(GOVEE_KELVIN[0], min(GOVEE_KELVIN[1], c.color.kelvin))
            data = {"color": {"r": 0, "g": 0, "b": 0}, "colorTemInKelvin": k}
        else:
            r, g, b = c.color.rgb or (255, 255, 255)
            data = {"color": {"r": r, "g": g, "b": b}, "colorTemInKelvin": 0}
        out.append({"msg": {"cmd": "colorwc", "data": data}})
    if c.brightness is not None:
        out.append({"msg": {"cmd": "brightness", "data": {"value": max(1, c.brightness)}}})
    return out


class GoveeDriver:
    def __init__(self, light: Light) -> None:
        self.light = light

    def _status_blocking(self) -> State:
        with _govee_listening(1.5) as r, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
            s.sendto(json.dumps({"msg": {"cmd": "devStatus", "data": {}}}).encode(), (self.light.ip, GOVEE_CMD_PORT))
            end = time.monotonic() + 1.5
            while time.monotonic() < end:
                data, addr = r.recvfrom(4096)
                if addr[0] != self.light.ip:
                    continue
                d = json.loads(data)["msg"]
                if d.get("cmd") != "devStatus":
                    continue
                d = d["data"]
                col = d.get("color") or {}
                k = int(d.get("colorTemInKelvin") or 0)
                return State(on=bool(d.get("onOff")), brightness=int(d.get("brightness") or 0),
                             rgb=(int(col.get("r", 0)), int(col.get("g", 0)), int(col.get("b", 0))),
                             kelvin=k or None)
        raise TimeoutError("the Govee light did not answer")

    def _rediscover(self) -> bool:
        """DHCP moves a light: find it again by its device id."""
        if not self.light.device:
            return False
        for d in govee_scan(2.0):
            if d["device"] == self.light.device and d["ip"] != self.light.ip:
                log.info("lights: %s moved to a new address", self.light.name)
                self.light.ip = d["ip"]
                return True
        return False

    async def state(self) -> State:
        try:
            return await asyncio.to_thread(self._status_blocking)
        except (OSError, TimeoutError):
            if await asyncio.to_thread(self._rediscover):
                return await asyncio.to_thread(self._status_blocking)
            raise

    async def apply(self, change: Change) -> None:
        if not self.light.ip and not await asyncio.to_thread(self._rediscover):
            raise ConnectionError("no address for this Govee light; run `eve-bridge lights scan`")
        msgs = govee_commands(change)

        def send() -> None:
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
                for m in msgs:
                    s.sendto(json.dumps(m).encode(), (self.light.ip, GOVEE_CMD_PORT))
                    time.sleep(0.05)        # the lamp drops a burst; a gap keeps every message
        await asyncio.to_thread(send)

    async def close(self) -> None:
        return None


# ---- WiZ: the local UDP API --------------------------------------------------------------------------------------

WIZ_PORT = 38899
WIZ_KELVIN = (2200, 6500)           # WiZ colour bulbs; tunable-white ones span 2700-6500 and clamp the rest
WIZ_MIN_DIMMING = 10                # the bulb refuses a dimming below this


def wiz_params(change: Change) -> dict[str, Any]:
    """The one ``setPilot`` (or ``setState``) params dict for a change. WiZ takes power, colour and level together."""
    c = change.normalised()
    if c.power is False:
        return {"state": False}
    out: dict[str, Any] = {"state": True}
    if c.color is not None:
        if c.color.kelvin is not None:
            out["temp"] = max(WIZ_KELVIN[0], min(WIZ_KELVIN[1], c.color.kelvin))
        else:
            r, g, b = c.color.rgb or (255, 255, 255)
            out.update(r=r, g=g, b=b)
    if c.brightness is not None:
        out["dimming"] = max(WIZ_MIN_DIMMING, c.brightness)
    return out


def wiz_parse_pilot(result: dict[str, Any]) -> State:
    """A ``getPilot`` result as a State. A scene (sceneId, no r/g/b or temp) leaves the colour unknown."""
    k = int(result.get("temp") or 0)
    rgb = None
    if not k and all(isinstance(result.get(x), int) for x in ("r", "g", "b")):
        rgb = (int(result["r"]), int(result["g"]), int(result["b"]))
    dim = result.get("dimming")
    return State(on=bool(result.get("state")), brightness=int(dim) if isinstance(dim, (int, float)) else None,
                 rgb=rgb, kelvin=k or None)


def wiz_sign(key_hex: str, params: dict[str, Any]) -> str:
    """The ``hmac`` a signed WiZ request carries: HMAC-SHA256 of the compact params JSON (``sigTs`` included),
    keyed by the home's hex ``udp_signing_key``, base64. Found 2026-09-28 by reproducing the bulb's own reply
    signatures (it signs its ``result`` the same way), then confirmed by a signed setPilot it accepted."""
    body = json.dumps(params, separators=(",", ":")).encode()
    return base64.b64encode(hmac.new(bytes.fromhex(key_hex), body, hashlib.sha256).digest()).decode()


def wiz_message(method: str, params: dict[str, Any], key_hex: str = "", sig_ts: Optional[int] = None) -> bytes:
    """A request, signed when there is a key (a read needs none, but a signed one is accepted too)."""
    msg: dict[str, Any] = {"id": 1, "method": method, "params": params}
    if key_hex and method.startswith("set"):
        signed = {**params, "sigTs": int(time.time()) if sig_ts is None else sig_ts}
        msg = {"id": 1, "method": method, "params": signed, "hmac": wiz_sign(key_hex, signed)}
    return json.dumps(msg, separators=(",", ":")).encode()


class WizRefused(PermissionError):
    """The bulb answered but refused a change: unsigned, or a stale or wrong signature."""


def _wiz_call(ip: str, method: str, params: dict[str, Any], tries: int = 3, wait: float = 0.7, key: str = "",
              sig_ts: Optional[int] = None) -> dict[str, Any]:
    """One request and its reply. UDP drops, so it resends; the reply's ``error`` becomes an exception."""
    msg = wiz_message(method, params, key, sig_ts)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.settimeout(wait)
        for _ in range(tries):
            s.sendto(msg, (ip, WIZ_PORT))
            end = time.monotonic() + wait
            while time.monotonic() < end:
                try:
                    data, addr = s.recvfrom(4096)
                except socket.timeout:
                    break
                if addr[0] != ip:
                    continue
                try:
                    reply = json.loads(data)
                except ValueError:
                    continue
                if reply.get("method") != method:
                    continue
                if "error" in reply:
                    err = reply["error"] if isinstance(reply["error"], dict) else {}
                    if err.get("code") == -32602 and method.startswith("set"):
                        # firmware 1.38 refuses an unsigned change (reads still answer): "Invalid params"
                        raise WizRefused("the WiZ light refused the change: it needs the home's signing key "
                                         "(eve-bridge lights wiz-import, docs/lights.md)" if not key else
                                         "the WiZ light refused the signed change (a new key? run lights "
                                         "wiz-import again)")
                    raise ConnectionError(f"the WiZ light refused {method}: {err.get('message') or reply['error']}")
                return reply.get("result") or {}
    raise TimeoutError("the WiZ light did not answer; is it on the same Wi-Fi?")


def wiz_call_signed(ip: str, method: str, params: dict[str, Any], key: str) -> dict[str, Any]:
    """A change, signed with the Mac's clock; refused once, signed again with the bulb's own clock (its reply
    to a read carries ``sigTs``), in case the two disagree."""
    try:
        return _wiz_call(ip, method, params, key=key)
    except WizRefused:
        if not key:
            raise
        bulb_ts = _wiz_call(ip, "getPilot", {}, tries=2).get("sigTs")
        if not isinstance(bulb_ts, int):
            raise
        return _wiz_call(ip, method, params, key=key, sig_ts=bulb_ts + 1)


def wiz_import(export: dict[str, Any], lights: Sequence[Light]) -> list[str]:
    """Take the signing key from the WiZ app's local-integration export (``udp_signing_key``, and ``devices``
    by ``mac_address``) into every WiZ light of that home. Returns the names given the key."""
    key = str(export.get("udp_signing_key") or "")
    bytes.fromhex(key)                                    # ValueError when it is not a hex key
    macs = {str(d.get("mac_address") or "").lower() for d in export.get("devices") or []}
    done = []
    for lt in lights:
        if lt.kind == "wiz" and (not macs or lt.device.lower() in macs):
            lt.key = key
            done.append(lt.name)
    return done


def _broadcast_addresses() -> list[str]:
    """255.255.255.255 plus this Mac's /24 broadcast: some routers drop the all-ones one."""
    out = ["255.255.255.255"]
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
            s.connect(("10.255.255.255", 1))           # no packet is sent; it only picks the LAN interface
            a, b, c, _ = s.getsockname()[0].split(".")
            out.append(f"{a}.{b}.{c}.255")
    except OSError:
        pass
    return out


def wiz_scan(seconds: float = 3.0) -> list[dict[str, str]]:
    """Every WiZ light with local communication on: ``{"device" (MAC), "sku" (module), "ip"}``. Blocking."""
    msg = json.dumps({"method": "getPilot", "params": {}}).encode()
    found: dict[str, dict[str, str]] = {}
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        s.settimeout(0.3)
        end, again = time.monotonic() + seconds, 0.0
        while time.monotonic() < end:
            if time.monotonic() >= again:
                for addr in _broadcast_addresses():
                    try:
                        s.sendto(msg, (addr, WIZ_PORT))
                    except OSError:
                        pass
                again = time.monotonic() + 1.0
            try:
                data, (ip, _port) = s.recvfrom(4096)
            except socket.timeout:
                continue
            try:
                mac = str(json.loads(data)["result"]["mac"]).lower()
            except (ValueError, KeyError, TypeError):
                continue
            found[mac] = {"device": mac, "sku": "", "ip": ip}
    for d in found.values():
        try:
            d["sku"] = str(_wiz_call(d["ip"], "getSystemConfig", {}, tries=1).get("moduleName") or "")
        except (OSError, TimeoutError):
            pass
    return list(found.values())


class WizDriver:
    def __init__(self, light: Light) -> None:
        self.light = light

    def _rediscover(self) -> bool:
        """DHCP moves a light: find it again by its MAC."""
        if not self.light.device:
            return False
        for d in wiz_scan(2.0):
            if d["device"] == self.light.device.lower() and d["ip"] != self.light.ip:
                log.info("lights: %s moved to a new address", self.light.name)
                self.light.ip = d["ip"]
                return True
        return False

    async def _call(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        if not self.light.ip and not await asyncio.to_thread(self._rediscover):
            raise ConnectionError("no address for this WiZ light; run `eve-bridge lights scan`")
        key = self.light.key

        def call() -> dict[str, Any]:
            return wiz_call_signed(self.light.ip, method, params, key) if method.startswith("set") \
                else _wiz_call(self.light.ip, method, params)
        try:
            return await asyncio.to_thread(call)
        except WizRefused:
            raise
        except (OSError, TimeoutError):
            if await asyncio.to_thread(self._rediscover):
                return await asyncio.to_thread(call)
            raise

    async def state(self) -> State:
        return wiz_parse_pilot(await self._call("getPilot", {}))

    async def apply(self, change: Change) -> None:
        params = wiz_params(change)
        await self._call("setState" if params == {"state": False} else "setPilot", params)

    async def close(self) -> None:
        return None


# ---- HappyLighting: Triones over Bluetooth LE ------------------------------------------------------------------

TRIONES_WRITE = "0000ffd9-0000-1000-8000-00805f9b34fb"
TRIONES_NOTIFY = "0000ffd4-0000-1000-8000-00805f9b34fb"
TRIONES_ON = bytes([0xCC, 0x23, 0x33])
TRIONES_OFF = bytes([0xCC, 0x24, 0x33])
TRIONES_STATUS = bytes([0xEF, 0x01, 0x77])
TRIONES_NAMES = ("QHM-", "Triones", "LEDBlue", "LEDBLE")
TRIONES_IDLE_SECS = 20.0            # hold the connection this long for the next command; the phone app needs it back
TRIONES_CONNECT_SECS = 10.0


def triones_color(rgb: Sequence[int]) -> bytes:
    r, g, b = (max(0, min(255, int(c))) for c in rgb)
    return bytes([0x56, r, g, b, 0x00, 0xF0, 0xAA])


def triones_parse_status(data: bytes) -> Optional[State]:
    """``66 tt PP mm .. ss RR GG BB WW vv 99``: PP 0x23 on, 0x24 off; mode 0x41 is a steady colour."""
    if len(data) < 12 or data[0] != 0x66 or data[-1] != 0x99:
        return None
    rgb = (data[6], data[7], data[8])
    return State(on=data[2] == 0x23, rgb=rgb, brightness=rgb_percent(rgb))


def triones_frames(change: Change, current: Optional[State]) -> list[bytes]:
    """The writes for a change. The controller has no brightness of its own: brightness is the colour's level,
    so a colour keeps the current level and a level keeps the current colour."""
    c = change.normalised()
    if c.power is False:
        return [TRIONES_OFF]
    out: list[bytes] = []
    was_on = bool(current and current.on)
    if c.power is True or not c.empty:
        out.append(TRIONES_ON)
    if c.color is None and c.brightness is None:
        return out
    base = c.color.as_rgb() if c.color is not None else (current.rgb if current and current.rgb else (255, 255, 255))
    if c.brightness is not None:
        level = c.brightness
    elif current is not None and current.rgb is not None and was_on and rgb_percent(current.rgb) > 0:
        level = rgb_percent(current.rgb)
    else:
        level = 100
    out.append(triones_color(scale_rgb(base, level)))
    return out


class TrionesDriver:
    """One Bluetooth connection, opened on demand and closed after TRIONES_IDLE_SECS of quiet."""

    def __init__(self, light: Light) -> None:
        self.light = light
        self._client: Any = None
        self._lock = asyncio.Lock()
        self._closer: Optional[asyncio.TimerHandle] = None
        self._status: Optional[asyncio.Future[bytes]] = None

    async def _connect(self) -> Any:
        if self._client is not None and self._client.is_connected:
            return self._client
        from bleak import BleakClient, BleakScanner

        dev = await BleakScanner.find_device_by_address(self.light.address, timeout=TRIONES_CONNECT_SECS)
        if dev is None:
            raise ConnectionError("the Bluetooth light is out of range or taken by the phone app")
        client = BleakClient(dev, timeout=TRIONES_CONNECT_SECS)
        await client.connect()
        await client.start_notify(TRIONES_NOTIFY, self._on_notify)
        self._client = client
        return client

    def _on_notify(self, _handle: Any, data: bytearray) -> None:
        if self._status is not None and not self._status.done() and data[:1] == b"\x66":
            self._status.set_result(bytes(data))

    def _touch(self) -> None:
        if self._closer is not None:
            self._closer.cancel()
        loop = asyncio.get_running_loop()
        self._closer = loop.call_later(TRIONES_IDLE_SECS, lambda: loop.create_task(self.close()))

    async def _read(self, client: Any) -> Optional[State]:
        self._status = asyncio.get_running_loop().create_future()
        await client.write_gatt_char(TRIONES_WRITE, TRIONES_STATUS, response=False)
        try:
            return triones_parse_status(await asyncio.wait_for(self._status, 2.0))
        except asyncio.TimeoutError:
            return None
        finally:
            self._status = None

    async def state(self) -> State:
        async with self._lock:
            client = await self._connect()
            self._touch()
            st = await self._read(client)
            if st is None:
                raise TimeoutError("the Bluetooth light did not report its state")
            return st

    async def apply(self, change: Change) -> None:
        async with self._lock:
            client = await self._connect()
            self._touch()
            c = change.normalised()
            needs_state = c.power is not False and (c.color is not None or c.brightness is not None)
            current = await self._read(client) if needs_state else None
            for frame in triones_frames(change, current):
                await client.write_gatt_char(TRIONES_WRITE, frame, response=False)
                await asyncio.sleep(0.05)

    async def close(self) -> None:
        if self._closer is not None:
            self._closer.cancel()
            self._closer = None
        client, self._client = self._client, None
        if client is not None:
            try:
                await client.disconnect()
            except Exception:  # noqa: BLE001 — a connection that is already gone is closed
                pass


async def triones_scan(seconds: float = 8.0) -> list[dict[str, str]]:
    """Nearby HappyLighting controllers: ``{"address", "name"}``. Needs Bluetooth permission (see the module doc)."""
    from bleak import BleakScanner

    found = await BleakScanner.discover(timeout=seconds, return_adv=True)
    out = []
    for addr, (dev, adv) in found.items():
        name = adv.local_name or dev.name or ""
        if name.startswith(TRIONES_NAMES):
            out.append({"address": addr, "name": name})
    return out


# ---- Sylvania Smart+ Wi-Fi: Tuya's local protocol ------------------------------------------------------------

TUYA_KELVIN = (2700, 6500)          # a Tuya bulb's colour-temperature percent spans about this


def tuya_temp_percent(kelvin: int) -> int:
    lo, hi = TUYA_KELVIN
    return int(round((max(lo, min(hi, kelvin)) - lo) * 100 / (hi - lo)))


class TuyaDriver:
    def __init__(self, light: Light) -> None:
        self.light = light
        self._bulb: Any = None

    def _dev(self) -> Any:
        if self._bulb is None:
            if not self.light.key:
                raise PermissionError("this Tuya light has no local key yet (docs/lights.md, Sylvania)")
            try:
                import tinytuya  # type: ignore[import-not-found]
            except ImportError as e:
                raise RuntimeError("tinytuya is not installed: pip install -e \".[lights]\"") from e
            bulb = tinytuya.BulbDevice(self.light.id, self.light.ip or "Auto", self.light.key,
                                       version=float(self.light.version or 3.3))
            bulb.set_socketTimeout(3)
            bulb.set_socketRetryLimit(1)
            self._bulb = bulb
        return self._bulb

    def _state_blocking(self) -> State:
        dev = self._dev()
        s = dev.state()
        if not isinstance(s, dict) or "Error" in s:
            raise ConnectionError(str((s or {}).get("Error") or "the Tuya light did not answer"))
        pct = dev.get_brightness_percentage()
        rgb = None
        if s.get("mode") == "colour":
            got = dev.colour_rgb()
            rgb = tuple(int(c) for c in got) if isinstance(got, (tuple, list)) and len(got) == 3 else None
        return State(on=bool(s.get("is_on")), brightness=int(pct) if isinstance(pct, (int, float)) else None,
                     rgb=rgb)  # type: ignore[arg-type]

    def _apply_blocking(self, change: Change) -> None:
        c = change.normalised()
        dev = self._dev()
        if c.power is False:
            _tuya_ok(dev.turn_off())
            return
        if c.power is True or not c.empty:
            _tuya_ok(dev.turn_on())
        if c.color is not None and c.color.kelvin is not None:
            _tuya_ok(dev.set_white_percentage(c.brightness or 100, tuya_temp_percent(c.color.kelvin)))
            return
        if c.color is not None:
            _tuya_ok(dev.set_colour(*(c.color.rgb or (255, 255, 255))))
        if c.brightness is not None:
            _tuya_ok(dev.set_brightness_percentage(c.brightness))

    async def state(self) -> State:
        return await asyncio.to_thread(self._state_blocking)

    async def apply(self, change: Change) -> None:
        await asyncio.to_thread(self._apply_blocking, change)

    async def close(self) -> None:
        bulb, self._bulb = self._bulb, None
        if bulb is not None:
            try:
                bulb.close()
            except Exception:  # noqa: BLE001
                pass


def _tuya_ok(reply: Any) -> None:
    if isinstance(reply, dict) and "Error" in reply:
        raise ConnectionError(str(reply["Error"]))


def tuya_scan(seconds: int = 8) -> list[dict[str, str]]:
    """Tuya devices broadcasting on the LAN: ``{"id", "ip", "version"}``. No key needed to find them."""
    try:
        import tinytuya  # type: ignore[import-not-found]
    except ImportError:
        return []
    found = tinytuya.deviceScan(verbose=False, maxretry=seconds, color=False, poll=False)
    return [{"id": str(d.get("gwId") or d.get("id") or ""), "ip": str(d.get("ip") or ip),
             "version": str(d.get("version") or "3.3")} for ip, d in (found or {}).items()]


DRIVERS: dict[str, Callable[[Light], Any]] = {"govee": GoveeDriver, "wiz": WizDriver, "triones": TrionesDriver,
                                              "tuya": TuyaDriver}


# ---- which lights a request means --------------------------------------------------------------------------------

ALL_WORDS = frozenset({"", "all", "lights", "light", "the lights", "all lights", "all the lights", "every light",
                       "all of them", "everything", "them", "it"})


def _norm(text: str) -> str:
    return re.sub(r"\s+", " ", re.sub(r"[^\w#%\s-]", " ", (text or "").lower().replace("’", "'"))).strip()


# ---- the tools ------------------------------------------------------------------------------------------------

TOOLS: list[dict[str, Any]] = [
    {"type": "function", "name": "lights_set", "strict": True,
     "description": "Change the owner's lights: on or off, brightness, colour or a white. Only what you pass "
                    "changes; a colour or a brightness turns an off light on.",
     "parameters": {"type": "object", "additionalProperties": False,
                    "required": ["target", "power", "brightness", "color"],
                    "properties": {
                        "target": {"type": "string",
                                   "description": "\"all\", a light's name, or a room, as listed in your "
                                                  "instructions."},
                        "power": {"type": ["boolean", "null"], "description": "true on, false off, null to leave."},
                        "brightness": {"type": ["integer", "null"], "description": "1-100 percent; null to leave."},
                        "color": {"type": ["string", "null"],
                                  "description": "A colour name (red, blue, purple, pink, orange...), a #rrggbb, "
                                                 "a white (\"warm white\", \"white\", \"cool white\", "
                                                 "\"daylight\") or a temperature like \"2700K\"; null to leave."}}}},
    {"type": "function", "name": "lights_match", "strict": True,
     "description": "Make lights match another light: the same on or off, colour or white, and brightness. "
                    "Use it for \"make X the same as the others\", \"match the lights\", \"sync the lights\".",
     "parameters": {"type": "object", "additionalProperties": False, "required": ["target", "source"],
                    "properties": {
                        "target": {"type": "string", "description": "The lights to change: \"all\", a light's "
                                                                    "name, or a room."},
                        "source": {"type": ["string", "null"],
                                   "description": "The light to copy, by name; null to copy the other lights."}}}},
    {"type": "function", "name": "lights_status", "strict": True,
     "description": "Whether the owner's lights are on, and their brightness and colour.",
     "parameters": {"type": "object", "additionalProperties": False, "required": ["target"],
                    "properties": {"target": {"type": "string", "description": "\"all\", a light's name, or a room."}}}},
]
TOOL_NAMES = tuple(t["name"] for t in TOOLS)

INSTRUCTIONS_HEAD = """You can control the owner's lights with lights_set (on, off, brightness, colour, white),
make lights match another light with lights_match, and read them with lights_status. For a mood, pick the settings yourself: cozy is warm white around 30%, reading is
white at 100%, a movie is dim, a party is a colour. Say what you did in a few words."""


@dataclass
class Command:
    """A light command the code understood: which lights, and what to change."""
    target: str
    change: Change
    names: tuple[str, ...] = field(default_factory=tuple)
    source: Optional[str] = None        # set: copy this light ("" for the other lights) instead of change


# The words a command may carry besides a target, an on/off, a colour and a level. Anything else — "in ten
# minutes", "are", "why" — and the message is not only a command: the model gets it.
_FILLER = frozenset({"please", "pls", "eve", "hey", "ok", "okay", "can", "could", "would", "will", "you", "u",
                     "the", "my", "to", "at", "in", "light", "lights", "lamp", "lamps", "turn", "switch", "set",
                     "make", "put", "change", "brightness", "level", "now", "and", "a", "bit", "all", "of", "them",
                     "it", "thanks", "thank", "for", "me", "go", "up", "percent"})
_VERB_DIM = frozenset({"dim", "darken", "lower"})
_VERB_BRIGHT = frozenset({"brighten", "brighter", "raise", "full", "max"})
_POWER = {"on": True, "off": False, "out": False}
DIM_PERCENT = 20
_NAME_STOPWORDS = frozenset({"the", "my", "a", "of", "and", "in", "on", "off", "to", "at"})
_PCT = re.compile(r"^(\d{1,3})%?$")
# "match the lights", "sync the lights", "make wiz match the lamp", "wiz same as the others", "match wiz to the lamp"
_COPY_ALL = re.compile(r"^(?:match|sync)(?: up)?(?: all)?(?: the| my)? lights?(?: up)?$")
_COPY_AS = re.compile(r"^(?:make |set |turn |put |change )?(?P<target>.+?) (?:to )?(?:match(?:es)?|"
                      r"(?:the )?same(?: (?:colou?rs?|lights?|settings?|way))? as|like) (?P<source>.+)$")
_COPY_TO = re.compile(r"^(?:match|sync) (?P<target>.+?)(?: (?:to|with) (?P<source>.+))?$")
_COPY_LEAD = re.compile(r"^(?:(?:hey|ok|okay|eve|please|pls|can you|could you|would you) )+")
_COPY_TAIL = re.compile(r"(?: (?:please|pls|now|rn|right now|thanks|thank you|too|as well))+$")
_OTHERS = frozenset({"others", "the others", "other lights", "the other lights", "the other ones", "the rest",
                     "rest of the lights", "the rest of the lights", "everything else", "everyone else",
                     "the other light"})
# a source is read before it is copied: a LAN light answers in milliseconds, Bluetooth has to connect
_SOURCE_ORDER = ("govee", "wiz", "tuya", "triones")


class Lights:
    """The owner's lights, their drivers, the tools and the code path."""

    def __init__(self, lights: Sequence[Light], *, drivers: Optional[dict[str, Callable[[Light], Any]]] = None,
                 timeout: float = LIGHT_TIMEOUT_SECS) -> None:
        self.lights = list(lights)
        self._make = drivers or DRIVERS
        self._drivers: dict[str, Any] = {}
        self.timeout = timeout

    def driver(self, light: Light) -> Any:
        d = self._drivers.get(light.name)
        if d is None:
            d = self._drivers[light.name] = self._make[light.kind](light)
        return d

    # -- targets --
    def _words_of(self, light: Light) -> list[str]:
        return [w for w in (_norm(light.name), *(_norm(a) for a in light.aliases)) if w]

    def resolve(self, target: str) -> tuple[list[Light], str]:
        """(lights, "") or ([], why not). "all"/"the lights" is every light; then a name or alias; then a room;
        then a single word that is in exactly one name ("lamp" for "floor lamp") or names a brand."""
        t = _norm(target)
        t = re.sub(r"^(the|my)\s+", "", t)
        t = re.sub(r"\s+(lights?|lamps?)$", "", t) if t not in ("lights", "light") else t
        if t in ALL_WORDS:
            return list(self.lights), ""
        by_name = [lt for lt in self.lights if t in self._words_of(lt)]
        if by_name:
            return by_name, ""
        by_room = [lt for lt in self.lights if lt.room and _norm(lt.room) == t]
        if by_room:
            return by_room, ""
        by_kind = [lt for lt in self.lights if t in (lt.kind, {"triones": "happylighting", "tuya": "sylvania"}.get(lt.kind))]
        if by_kind:
            return by_kind, ""
        partial = [lt for lt in self.lights if any(re.search(rf"\b{re.escape(t)}\b", w) for w in self._words_of(lt))]
        if partial:
            return partial, ""
        return [], f"no light called {target!r}; the lights are: " + self.names_line()

    def names_line(self) -> str:
        return ", ".join(lt.name + (f" ({lt.room})" if lt.room else "") for lt in self.lights) or "none"

    # -- doing it --
    async def _one(self, light: Light, change: Change) -> dict[str, Any]:
        try:
            await asyncio.wait_for(self.driver(light).apply(change), self.timeout)
            return {"name": light.name, "ok": True}
        except asyncio.CancelledError:
            raise
        except asyncio.TimeoutError:
            return {"name": light.name, "ok": False, "reason": "it did not answer in time"}
        except Exception as e:  # noqa: BLE001 — one light failing never stops the others
            log.warning("lights: %s failed: %s: %s", light.name, type(e).__name__, e)
            return {"name": light.name, "ok": False, "reason": str(e) or type(e).__name__}

    async def set(self, target: str, change: Change) -> dict[str, Any]:
        lights, why = self.resolve(target)
        if not lights:
            return {"ok": False, "reason": why}
        if change.empty:
            return {"ok": False, "reason": "nothing to change: give power, brightness or a colour"}
        results = await asyncio.gather(*(self._one(lt, change) for lt in lights))
        done = [r["name"] for r in results if r["ok"]]
        failed = [{"name": r["name"], "reason": r["reason"]} for r in results if not r["ok"]]
        log.info("lights: %s -> %d done, %d failed", describe(change), len(done), len(failed))
        return {"ok": bool(done), "done": done, "failed": failed, "change": describe(change)}

    async def status(self, target: str = "all") -> dict[str, Any]:
        lights, why = self.resolve(target)
        if not lights:
            return {"ok": False, "reason": why}

        async def one(lt: Light) -> dict[str, Any]:
            base = {"name": lt.name, **({"room": lt.room} if lt.room else {})}
            try:
                st = await asyncio.wait_for(self.driver(lt).state(), self.timeout)
                return {**base, "reachable": True, **st.public()}
            except asyncio.CancelledError:
                raise
            except Exception as e:  # noqa: BLE001
                return {**base, "reachable": False, "reason": str(e) or type(e).__name__}
        return {"ok": True, "lights": list(await asyncio.gather(*(one(lt) for lt in lights)))}

    async def copy(self, target: str, source: Optional[str] = None) -> dict[str, Any]:
        """Put the target lights in the source light's state. No source: the first other light that answers,
        a Wi-Fi light before a Bluetooth one. With every light as the target, one light is copied onto the rest."""
        targets, why = self.resolve(target)
        if not targets:
            return {"ok": False, "reason": why}
        if source and _norm(source) not in _OTHERS:
            sources, why = self.resolve(source)
            if not sources:
                return {"ok": False, "reason": why}
        else:
            sources = [lt for lt in self.lights if lt not in targets] or list(targets)
        sources = sorted(sources, key=lambda lt: _SOURCE_ORDER.index(lt.kind) if lt.kind in _SOURCE_ORDER else 9)
        picked: Optional[Light] = None
        state: Optional[State] = None
        tried: list[str] = []
        for lt in sources:
            try:
                state = await asyncio.wait_for(self.driver(lt).state(), self.timeout)
                picked = lt
                break
            except asyncio.CancelledError:
                raise
            except Exception as e:  # noqa: BLE001 — try the next light
                tried.append(f"{lt.name}: {str(e) or type(e).__name__}")
        if picked is None or state is None:
            return {"ok": False, "reason": "no light to copy answered (" + "; ".join(tried) + ")"}
        change = change_of(state)
        to_set = [lt for lt in targets if lt is not picked]
        if not to_set:
            return {"ok": False, "reason": f"{picked.name} is the only light: nothing to match it to"}
        results = await asyncio.gather(*(self._one(lt, change) for lt in to_set))
        log.info("lights: match %s -> %d done, %d failed", describe(change),
                 sum(r["ok"] for r in results), sum(not r["ok"] for r in results))
        return {"ok": any(r["ok"] for r in results), "source": picked.name, "change": describe(change),
                "done": [r["name"] for r in results if r["ok"]],
                "failed": [{"name": r["name"], "reason": r["reason"]} for r in results if not r["ok"]],
                "line": self.copy_line(picked, to_set, results, change)}

    def copy_line(self, source: Light, lights: Sequence[Light], results: Sequence[dict[str, Any]],
                  change: Change) -> str:
        """"Wiz matched to floor lamp: red at 100%." and each light that did not."""
        done = [r["name"] for r in results if r["ok"]]
        parts = [f"{_join(done)} matched to {source.name}: {describe(change)}."] if done else []
        parts += [f"{r['name']}: {r['reason']}." for r in results if not r["ok"]]
        return " ".join(parts) or "No lights changed."

    async def close(self) -> None:
        for d in list(self._drivers.values()):
            try:
                await d.close()
            except Exception:  # noqa: BLE001
                pass
        self._drivers.clear()

    # -- the model's tools --
    def tools(self) -> list[dict[str, Any]]:
        return json.loads(json.dumps(TOOLS))

    def instructions(self) -> str:
        rows = "\n".join(f"- {lt.name}" + (f" (room: {lt.room})" if lt.room else "")
                         + (f", also called {', '.join(lt.aliases)}" if lt.aliases else "") for lt in self.lights)
        return INSTRUCTIONS_HEAD + "\nThe lights:\n" + rows

    async def handle(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        if name == "lights_status":
            return await self.status(str(args.get("target") or "all"))
        if name == "lights_match":
            out = await self.copy(str(args.get("target") or "all"), args.get("source") or None)
            out.pop("line", None)
            return out
        if name == "lights_set":
            color = None
            raw = args.get("color")
            if raw not in (None, ""):
                color = parse_color(str(raw))
                if color is None:
                    return {"ok": False, "reason": f"unknown colour {raw!r}: use a colour name, #rrggbb, a white "
                                                   "or a temperature like 2700K"}
            bright = args.get("brightness")
            change = Change(power=args.get("power") if isinstance(args.get("power"), bool) else None,
                            brightness=int(bright) if isinstance(bright, (int, float)) else None, color=color)
            return await self.set(str(args.get("target") or "all"), change)
        return {"ok": False, "reason": f"unknown tool {name}"}

    # -- the code path: a message that is only a light command --
    def match(self, text: str) -> Optional[Command]:
        """A Command when the whole message is a light command, else None (the model answers).

        Every word must be accounted for: a light, a room or "lights"; on/off; a colour or white; a level ("40%",
        "dim"); or a filler word. A message must name a light ("lights", "lamp", or a configured name or room),
        and must ask for something. "Turn off the lights in ten minutes" leaves "ten minutes" over: model."""
        if not self.lights:
            return None
        t = _norm(text)
        if not t or len(t) > 80:
            return None
        copy = self._match_copy(t)
        if copy is not None:
            return copy
        rest = f" {t} "
        # the lights' own words first, longest first, so "bedroom lamp" wins over "lamp"
        phrases = sorted({(w, lt.name) for lt in self.lights for w in self._words_of(lt)}
                         | {(_norm(lt.room), "@" + lt.room) for lt in self.lights if lt.room},
                         key=lambda p: -len(p[0]))
        # then a single word of exactly one light's name: "the lamp" is the floor lamp when only one is a lamp
        seen: dict[str, set[str]] = {}
        for lt in self.lights:
            for w in {x for words in self._words_of(lt) for x in words.split()} - _NAME_STOPWORDS:
                seen.setdefault(w, set()).add(lt.name)
        phrases += [(w, next(iter(who))) for w, who in seen.items() if len(who) == 1]
        targets: list[str] = []
        for phrase, who in phrases:
            if phrase and f" {phrase} " in rest:
                rest = rest.replace(f" {phrase} ", " ", 1)
                targets.append(who)
        # colours next, two words before one ("warm white" before "white")
        color: Optional[Color] = None
        for phrase in sorted([*COLORS, *WHITES], key=lambda p: -len(p)):
            if f" {phrase} " in rest:
                if color is not None:
                    return None                 # two colours: not a simple command
                color = parse_color(phrase)
                rest = rest.replace(f" {phrase} ", " ", 1)
        words = rest.split()
        power: Optional[bool] = None
        bright: Optional[int] = None
        light_word = bool(targets)
        leftover: list[str] = []
        i = 0
        while i < len(words):
            w = words[i]
            m = _PCT.match(w)
            if w in _POWER:
                if power is not None and power != _POWER[w]:
                    return None
                power = _POWER[w]
            elif m and int(m.group(1)) <= 100:
                bright = int(m.group(1))
            elif _KELVIN.match(w):
                if color is not None:
                    return None
                color = parse_color(w)
            elif w in _VERB_DIM:
                bright = bright if bright is not None else DIM_PERCENT
            elif w in _VERB_BRIGHT:
                bright = bright if bright is not None else 100
            elif w.startswith("#") and parse_color(w):
                if color is not None:
                    return None
                color = parse_color(w)
            elif w in ("light", "lights", "lamp", "lamps"):
                light_word = True
            elif w in _FILLER:
                pass
            else:
                leftover.append(w)
            i += 1
        if leftover or not light_word:
            return None
        change = Change(power=power, brightness=bright, color=color)
        if change.empty:
            return None
        if not targets:
            return Command("all", change, tuple(lt.name for lt in self.lights))
        chosen: list[Light] = []
        for who in targets:
            picked = [lt for lt in self.lights if lt.room == who[1:]] if who.startswith("@") else \
                [lt for lt in self.lights if lt.name == who]
            chosen += [lt for lt in picked if lt not in chosen]
        return Command(",".join(targets), change, tuple(lt.name for lt in self.lights if lt in chosen))

    def _match_copy(self, t: str) -> Optional[Command]:
        """"match the lights", "make wiz match the lamp", "wiz same as the others": a Command with a source.
        Target and source must both name lights, or it is not this command."""
        t = _COPY_TAIL.sub("", _COPY_LEAD.sub("", t)).strip()
        if _COPY_ALL.match(t):
            return Command("all", Change(), tuple(lt.name for lt in self.lights), source="")
        m = _COPY_AS.match(t) or _COPY_TO.match(t)
        if m is None:
            return None
        target = m.group("target")
        source = _COPY_TAIL.sub("", m.group("source") or "").strip()
        targets, _ = self.resolve(target)
        if not targets:
            return None
        if source and source not in _OTHERS and not self.resolve(source)[0]:
            return None
        return Command(target, Change(), tuple(lt.name for lt in targets), source=source)

    async def run(self, cmd: Command) -> str:
        """Do a matched command; the line to say back."""
        if cmd.source is not None:
            out = await self.copy(cmd.target, cmd.source or None)
            return str(out.get("line") or out.get("reason") or "No lights changed.")
        lights = [lt for lt in self.lights if lt.name in cmd.names]
        results = await asyncio.gather(*(self._one(lt, cmd.change) for lt in lights))
        log.info("lights: by code, %s -> %d done, %d failed", describe(cmd.change),
                 sum(r["ok"] for r in results), sum(not r["ok"] for r in results))
        return self.line(lights, results, cmd.change)

    def line(self, lights: Sequence[Light], results: Sequence[dict[str, Any]], change: Change) -> str:
        """"Lights blue." / "Floor lamp off. strip: out of range.": what changed, and each light that did not."""
        done = [r["name"] for r in results if r["ok"]]
        failed = [r for r in results if not r["ok"]]
        everyone = len(lights) == len(self.lights) and len(lights) > 1
        parts = []
        if done:
            who = "Lights" if everyone and not failed else _join(done)
            parts.append(f"{who} {describe(change)}.")
        for f in failed:
            parts.append(f"{f['name']}: {f['reason']}.")
        return " ".join(parts) or "No lights changed."

    async def panel(self, body: dict[str, Any]) -> dict[str, Any]:
        """The Mini App's lights card (miniapp.py /api/lights): ``{"action": "status"}`` reads every light;
        ``{"action": "set", "target", "power", "brightness", "color"}`` changes them and says what happened."""
        if body.get("action") == "status":
            return await self.status("all")
        target = str(body.get("target") or "all")
        lights, why = self.resolve(target)
        if not lights:
            return {"ok": False, "line": why}
        color = None
        if body.get("color"):
            color = parse_color(str(body["color"]))
            if color is None:
                return {"ok": False, "line": f"Unknown colour {body['color']!r}."}
        bright = body.get("brightness")
        power = body.get("power")
        change = Change(power=power if isinstance(power, bool) else None,
                        brightness=int(bright) if isinstance(bright, (int, float)) and not isinstance(bright, bool)
                        else None, color=color)
        if change.empty:
            return {"ok": False, "line": "Nothing to change."}
        results = await asyncio.gather(*(self._one(lt, change) for lt in lights))
        log.info("lights: from the Mini App, %s -> %d done, %d failed", describe(change),
                 sum(r["ok"] for r in results), sum(not r["ok"] for r in results))
        return {"ok": any(r["ok"] for r in results), "line": self.line(lights, results, change),
                "failed": [r["name"] for r in results if not r["ok"]]}

    def listing(self) -> list[dict[str, str]]:
        """The lights' names and rooms, for the Mini App's card (no addresses, no keys)."""
        return [{"name": lt.name, **({"room": lt.room} if lt.room else {})} for lt in self.lights]


def describe(change: Change) -> str:
    c = change.normalised()
    if c.power is False:
        return "off"
    bits = []
    if c.color is not None:
        word = c.color.word or "that colour"
        if word.startswith("#") and c.color.rgb is not None:
            word = f"{color_word(c.color.rgb)} ({word})"     # a picked colour: "cyan (#19ffff)" says what it is
        bits.append(word)
    if c.brightness is not None:
        bits.append(f"at {c.brightness}%")
    return " ".join(bits) if bits else "on"


def _join(names: Sequence[str]) -> str:
    names = list(names)
    if len(names) <= 1:
        return (names[0][:1].upper() + names[0][1:]) if names else ""
    s = ", ".join(names[:-1]) + " and " + names[-1]
    return s[:1].upper() + s[1:]


# ---- construction ----------------------------------------------------------------------------------------------

def enabled(environ: Any = None) -> bool:
    env = os.environ if environ is None else environ
    return str(env.get("EVE_LIGHTS", "")).strip().lower() not in ("0", "false", "no", "off")


def make_lights(path: Optional[Path] = None, environ: Any = None) -> Optional[Lights]:
    """The owner's Lights, or None when switched off or there are none (then no tool is lent)."""
    if not enabled(environ):
        log.info("lights: off (EVE_LIGHTS)")
        return None
    found = load(path)
    if not found:
        log.info("lights: none configured (eve-bridge lights scan)")
        return None
    log.info("lights: %d (%s)", len(found), ", ".join(sorted({lt.kind for lt in found})))
    return Lights(found)


# ---- the CLI: scan, list, name, set ---------------------------------------------------------------------------

async def scan(ble: bool = True, tuya: bool = True) -> list[Light]:
    """Everything found, as Lights with default names. Never raises for one brand failing."""
    out: list[Light] = []
    try:
        for d in await asyncio.to_thread(govee_scan, 3.0):
            out.append(Light(name=f"govee {d['sku'] or d['device'][-5:]}".lower(), kind="govee", ip=d["ip"],
                             device=d["device"], sku=d["sku"]))
    except OSError as e:
        log.warning("lights: Govee scan failed (%s)", e)
    try:
        for d in await asyncio.to_thread(wiz_scan, 3.0):
            out.append(Light(name=f"wiz {d['device'][-4:]}", kind="wiz", ip=d["ip"], device=d["device"], sku=d["sku"]))
    except OSError as e:
        log.warning("lights: WiZ scan failed (%s)", e)
    if ble:
        try:
            for d in await triones_scan():
                out.append(Light(name=f"happylighting {d['name']}".lower(), kind="triones", address=d["address"]))
        except Exception as e:  # noqa: BLE001
            log.warning("lights: Bluetooth scan failed (%s)", e)
    if tuya:
        try:
            for d in await asyncio.to_thread(tuya_scan):
                out.append(Light(name=f"tuya {d['id'][-4:]}", kind="tuya", id=d["id"], ip=d["ip"],
                                 version=d["version"]))
        except Exception as e:  # noqa: BLE001
            log.warning("lights: Tuya scan failed (%s)", e)
    return out


def merge(existing: Sequence[Light], found: Sequence[Light]) -> tuple[list[Light], list[Light]]:
    """Known lights keep their names, rooms and keys (a Govee light's new address is taken); new ones are added.
    Returns (all, new)."""
    out = list(existing)
    new: list[Light] = []

    def same(a: Light, b: Light) -> bool:
        return a.kind == b.kind and ((a.device and a.device == b.device) or (a.address and a.address == b.address)
                                     or (a.id and a.id == b.id))
    for f in found:
        known = next((e for e in out if same(e, f)), None)
        if known is None:
            taken = {e.name for e in out}
            n, i = f.name, 2
            while n in taken:
                n, i = f"{f.name} {i}", i + 1
            f.name = n
            out.append(f)
            new.append(f)
        elif f.ip and f.kind in ("govee", "wiz", "tuya"):
            known.ip = f.ip
    return out, new


def cli(argv: Sequence[str], path: Optional[Path] = None, out: Callable[[str], Any] = print) -> int:
    """``eve-bridge lights scan [--no-ble] [--no-tuya] | list | name OLD NEW [--room R] [--key K] | wiz-import FILE |
    set TARGET [on|off] [--brightness N] [--color C]``."""
    import argparse

    p = argparse.ArgumentParser(prog="eve-bridge lights")
    sub = p.add_subparsers(dest="act", required=True)
    s = sub.add_parser("scan", help="Find Govee, WiZ, HappyLighting and Tuya lights and add new ones to the lights file")
    s.add_argument("--no-ble", action="store_true", help="skip Bluetooth (a terminal without Bluetooth permission)")
    s.add_argument("--no-tuya", action="store_true")
    sub.add_parser("list", help="The lights and their state")
    n = sub.add_parser("name", help="Rename a light, and set its room or Tuya key")
    n.add_argument("old")
    n.add_argument("new")
    n.add_argument("--room")
    n.add_argument("--key", help="a Tuya light's local key")
    wz = sub.add_parser("wiz-import", help="Give WiZ lights their home's signing key, from the WiZ app's "
                                           "local-integration export (a JSON file)")
    wz.add_argument("file")
    st = sub.add_parser("set", help="Change lights now")
    st.add_argument("target")
    st.add_argument("power", nargs="?", choices=("on", "off"))
    st.add_argument("--brightness", type=int)
    st.add_argument("--color")
    a = p.parse_args(list(argv))
    cfg = (path or CONFIG_PATH).expanduser()

    if a.act == "scan":
        found = asyncio.run(scan(ble=not a.no_ble, tuya=not a.no_tuya))
        merged, new = merge(load(cfg), found)
        save(merged, cfg)
        for lt in merged:
            out(f"{'new  ' if lt in new else '     '}{lt.name}  [{lt.kind}]" + (f"  room: {lt.room}" if lt.room else "")
                + ("  (needs --key)" if lt.kind == "tuya" and not lt.key else ""))
        out(f"{len(new)} new, {len(merged)} in {cfg}. Rename with: eve-bridge lights name OLD NEW --room ROOM")
        return 0
    lights = load(cfg)
    if not lights:
        out(f"No lights in {cfg}. Run: eve-bridge lights scan")
        return 1
    if a.act == "wiz-import":
        try:
            done = wiz_import(json.loads(Path(a.file).expanduser().read_text()), lights)
        except (OSError, ValueError, AttributeError) as e:
            out(f"Could not read a WiZ export from {a.file}: {type(e).__name__}")
            return 1
        if not done:
            out("No WiZ light in the lights file matches that export. Run: eve-bridge lights scan")
            return 1
        save(lights, cfg)
        out(f"Signing key set for: {', '.join(done)}. Restart the daemon to use it.")
        return 0
    if a.act == "name":
        hub = Lights(lights)
        match, why = hub.resolve(a.old)
        if len(match) != 1:
            out(why or f"{a.old!r} is more than one light: {', '.join(lt.name for lt in match)}")
            return 1
        lt = match[0]
        lt.name = a.new.strip()
        if a.room is not None:
            lt.room = a.room.strip()
        if a.key is not None:
            lt.key = a.key.strip()
        save(lights, cfg)
        out(f"{lt.name}  [{lt.kind}]" + (f"  room: {lt.room}" if lt.room else ""))
        return 0

    async def go() -> dict[str, Any]:
        hub = Lights(lights)
        try:
            if a.act == "list":
                return await hub.status("all")
            color = parse_color(a.color) if a.color else None
            if a.color and color is None:
                return {"ok": False, "reason": f"unknown colour {a.color!r}"}
            power = None if a.power is None else a.power == "on"
            return await hub.set(a.target, Change(power=power, brightness=a.brightness, color=color))
        finally:
            await hub.close()
    result = asyncio.run(go())
    out(json.dumps(result, indent=2))
    return 0 if result.get("ok") else 1
