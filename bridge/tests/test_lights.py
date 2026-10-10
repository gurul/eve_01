"""lights.py: colours, each protocol's bytes, which lights a request means, the code path, the tools."""

from __future__ import annotations

import asyncio
import json
import stat
from typing import Any

import pytest

from eve_bridge import lights as L


class FakeDriver:
    def __init__(self, light: L.Light, log: list, fail: set) -> None:
        self.light, self.log, self.fail = light, log, fail

    async def apply(self, change: L.Change) -> None:
        if self.light.name in self.fail:
            raise ConnectionError("unreachable")
        self.log.append((self.light.name, change))

    async def state(self) -> L.State:
        if self.light.name in self.fail:
            raise ConnectionError("unreachable")
        return L.State(on=True, brightness=40, rgb=(0, 0, 255))

    async def close(self) -> None:
        return None


def hub(fail: set = frozenset()) -> tuple[L.Lights, list]:
    calls: list = []
    lights = [L.Light("floor lamp", "govee", room="bedroom", ip="192.0.2.10", device="AA:BB"),
              L.Light("strip", "triones", room="bedroom", address="uuid-1", aliases=("led strip",)),
              L.Light("desk bulb", "tuya", room="office", id="abc", key="k")]
    make = {k: (lambda lt, _k=k: FakeDriver(lt, calls, set(fail))) for k in L.KINDS}
    return L.Lights(lights, drivers=make), calls


# ---- colour ----

def test_parse_color_names_whites_hex_kelvin() -> None:
    assert L.parse_color("Blue").rgb == (0, 0, 255)
    assert L.parse_color("warm white").kelvin == 2700
    assert L.parse_color("daylight").kelvin == 6500
    assert L.parse_color("#FF8800").rgb == (255, 136, 0)
    assert L.parse_color("ff8800").rgb == (255, 136, 0)
    assert L.parse_color("2700K").kelvin == 2700
    assert L.parse_color("12000k").kelvin == L.KELVIN_MAX
    assert L.parse_color("red light").rgb == (255, 0, 0)
    assert L.parse_color("plaid") is None


def test_kelvin_to_rgb_is_warm_to_cool() -> None:
    warm, cool = L.kelvin_to_rgb(2700), L.kelvin_to_rgb(6500)
    assert warm[0] == 255 and warm[2] < warm[1] < 255
    assert cool[2] >= 250 and abs(cool[0] - cool[2]) < 20


def test_scale_rgb_keeps_hue_and_sets_level() -> None:
    assert L.scale_rgb((0, 0, 128), 100) == (0, 0, 255)
    assert L.scale_rgb((255, 128, 0), 50) == (128, 64, 0)
    assert L.scale_rgb((0, 0, 0), 50) == (128, 128, 128)
    assert L.rgb_percent((0, 0, 128)) == 50


def test_describe_names_a_picked_colour() -> None:
    assert L.describe(L.Change(color=L.parse_color("#19ffff"), brightness=50)) == "cyan (#19ffff) at 50%"
    assert L.describe(L.Change(color=L.parse_color("blue"))) == "blue"


def test_color_word() -> None:
    assert L.color_word((224, 0, 0)) == "red"
    assert L.color_word((0, 0, 200)) == "blue"
    assert L.color_word((250, 250, 245)) == "white"


# ---- Govee ----

def test_govee_off_is_one_turn() -> None:
    assert L.govee_commands(L.Change(power=False)) == [{"msg": {"cmd": "turn", "data": {"value": 0}}}]


def test_govee_colour_and_level_turn_on_first() -> None:
    msgs = L.govee_commands(L.Change(brightness=40, color=L.parse_color("blue")))
    assert [m["msg"]["cmd"] for m in msgs] == ["turn", "colorwc", "brightness"]
    assert msgs[0]["msg"]["data"] == {"value": 1}
    assert msgs[1]["msg"]["data"] == {"color": {"r": 0, "g": 0, "b": 255}, "colorTemInKelvin": 0}
    assert msgs[2]["msg"]["data"] == {"value": 40}


def test_govee_white_uses_kelvin_clamped() -> None:
    msgs = L.govee_commands(L.Change(color=L.Color(kelvin=1500, word="x")))
    assert msgs[1]["msg"]["data"]["colorTemInKelvin"] == L.GOVEE_KELVIN[0]


def test_govee_brightness_zero_means_off() -> None:
    assert L.govee_commands(L.Change(brightness=0)) == [{"msg": {"cmd": "turn", "data": {"value": 0}}}]


# ---- WiZ ----

def test_wiz_off_is_setstate_false() -> None:
    assert L.wiz_params(L.Change(power=False)) == {"state": False}
    assert L.wiz_params(L.Change(brightness=0)) == {"state": False}


def test_wiz_colour_and_level_in_one_pilot() -> None:
    assert L.wiz_params(L.Change(brightness=40, color=L.parse_color("blue"))) == \
        {"state": True, "r": 0, "g": 0, "b": 255, "dimming": 40}


def test_wiz_white_is_temp_clamped_and_dimming_floored() -> None:
    assert L.wiz_params(L.Change(color=L.Color(kelvin=1500, word="x"), brightness=3)) == \
        {"state": True, "temp": L.WIZ_KELVIN[0], "dimming": L.WIZ_MIN_DIMMING}


def test_wiz_parse_pilot() -> None:
    s = L.wiz_parse_pilot({"mac": "x", "state": True, "r": 255, "g": 0, "b": 0, "dimming": 60})
    assert (s.on, s.brightness, s.rgb, s.kelvin) == (True, 60, (255, 0, 0), None)
    w = L.wiz_parse_pilot({"state": False, "temp": 2700, "dimming": 100})
    assert (w.on, w.kelvin, w.rgb) == (False, 2700, None)
    scene = L.wiz_parse_pilot({"state": True, "sceneId": 4, "dimming": 50})
    assert scene.rgb is None and scene.kelvin is None


def _fake_wiz(reply: dict) -> tuple[Any, list]:
    """A UDP 'bulb' on localhost that records each request and answers with reply."""
    import socket as so
    import threading

    srv = so.socket(so.AF_INET, so.SOCK_DGRAM)
    srv.bind(("127.0.0.1", 0))
    srv.settimeout(3)
    got: list = []

    def run() -> None:
        try:
            data, addr = srv.recvfrom(4096)
        except OSError:
            return
        req = json.loads(data)
        got.append(req)
        srv.sendto(json.dumps({"method": req["method"], **reply}).encode(), addr)
        srv.close()
    threading.Thread(target=run, daemon=True).start()
    return srv, got


def test_wiz_driver_sends_setpilot_and_reads_reply(monkeypatch: Any) -> None:
    srv, got = _fake_wiz({"result": {"success": True}})
    monkeypatch.setattr(L, "WIZ_PORT", srv.getsockname()[1])
    d = L.WizDriver(L.Light("bulb", "wiz", ip="127.0.0.1", device="aabbccddeeff"))
    asyncio.run(d.apply(L.Change(color=L.parse_color("red"))))
    assert got == [{"id": 1, "method": "setPilot", "params": {"state": True, "r": 255, "g": 0, "b": 0}}]


def test_wiz_error_reply_raises(monkeypatch: Any) -> None:
    srv, _ = _fake_wiz({"error": {"code": -32600, "message": "Invalid Request"}})
    monkeypatch.setattr(L, "WIZ_PORT", srv.getsockname()[1])
    with pytest.raises(ConnectionError):
        L._wiz_call("127.0.0.1", "getPilot", {}, tries=1)


KEY = "00112233445566778899aabbccddeeff"               # a made-up key, not a real home's


def _signing_bulb(bulb_ts: int, clock_ok: bool = True) -> tuple[Any, list]:
    """A UDP 'bulb' that answers reads, and accepts a change only with a valid hmac over its params (and, when
    clock_ok is False, only with sigTs == bulb_ts + 1): firmware 1.38's behaviour."""
    import base64
    import hashlib
    import hmac
    import socket as so
    import threading

    srv = so.socket(so.AF_INET, so.SOCK_DGRAM)
    srv.bind(("127.0.0.1", 0))
    srv.settimeout(3)
    got: list = []

    def run() -> None:
        while True:
            try:
                data, addr = srv.recvfrom(4096)
            except OSError:
                return
            req = json.loads(data)
            got.append(req)
            if req["method"] == "getPilot":
                out = {"method": "getPilot", "result": {"state": True, "dimming": 50, "sigTs": bulb_ts}}
            else:
                body = json.dumps(req["params"], separators=(",", ":")).encode()
                good = base64.b64encode(hmac.new(bytes.fromhex(KEY), body, hashlib.sha256).digest()).decode()
                ok = req.get("hmac") == good and (clock_ok or req["params"].get("sigTs") == bulb_ts + 1)
                out = {"method": req["method"], **({"result": {"success": True}} if ok else
                                                   {"error": {"code": -32602, "message": "Invalid params"}})}
            srv.sendto(json.dumps(out).encode(), addr)
    threading.Thread(target=run, daemon=True).start()
    return srv, got


def test_wiz_sign_is_hmac_sha256_of_compact_params() -> None:
    import base64
    import hashlib
    import hmac
    params = {"state": True, "r": 0, "g": 255, "b": 0, "sigTs": 5}
    want = hmac.new(bytes.fromhex(KEY), b'{"state":true,"r":0,"g":255,"b":0,"sigTs":5}', hashlib.sha256).digest()
    assert base64.b64decode(L.wiz_sign(KEY, params)) == want
    msg = json.loads(L.wiz_message("setPilot", {"state": False}, KEY, sig_ts=7))
    assert msg["params"] == {"state": False, "sigTs": 7} and msg["hmac"] == L.wiz_sign(KEY, {"state": False, "sigTs": 7})
    assert "hmac" not in json.loads(L.wiz_message("getPilot", {}, KEY))       # reads go unsigned
    assert "hmac" not in json.loads(L.wiz_message("setPilot", {"state": True}))  # no key: unsigned


def test_a_signed_change_is_accepted_and_an_unsigned_one_names_the_fix(monkeypatch: Any) -> None:
    srv, got = _signing_bulb(bulb_ts=1000)
    monkeypatch.setattr(L, "WIZ_PORT", srv.getsockname()[1])
    d = L.WizDriver(L.Light("wiz", "wiz", ip="127.0.0.1", device="aabb", key=KEY))
    asyncio.run(d.apply(L.Change(color=L.parse_color("green"))))
    assert got[-1]["method"] == "setPilot" and "hmac" in got[-1]
    bare = L.WizDriver(L.Light("wiz", "wiz", ip="127.0.0.1", device="aabb"))
    with pytest.raises(L.WizRefused, match="wiz-import"):
        asyncio.run(bare.apply(L.Change(power=False)))
    srv.close()


def test_a_clock_the_bulb_disagrees_with_is_retried_on_the_bulbs_clock(monkeypatch: Any) -> None:
    srv, got = _signing_bulb(bulb_ts=1000, clock_ok=False)
    monkeypatch.setattr(L, "WIZ_PORT", srv.getsockname()[1])
    L.wiz_call_signed("127.0.0.1", "setPilot", {"state": True}, KEY)
    assert [r["method"] for r in got] == ["setPilot", "getPilot", "setPilot"]
    assert got[-1]["params"]["sigTs"] == 1001
    srv.close()


def test_wiz_import_takes_the_key_by_mac(tmp_path: Any) -> None:
    p = tmp_path / "lights.json"
    L.save([L.Light("wiz", "wiz", ip="192.0.2.5", device="aabbccddeeff"), L.Light("lamp", "govee", ip="192.0.2.6")], p)
    export = tmp_path / "export.json"
    export.write_text(json.dumps({"udp_signing_key": KEY, "devices": [{"mac_address": "AABBCCDDEEFF"}]}))
    said: list = []
    assert L.cli(["wiz-import", str(export)], path=p, out=said.append) == 0
    got = {lt.name: lt for lt in L.load(p)}
    assert got["wiz"].key == KEY and got["lamp"].key == "" and "wiz" in said[-1]
    assert stat.S_IMODE(p.stat().st_mode) == 0o600
    export.write_text(json.dumps({"udp_signing_key": "not hex", "devices": []}))
    assert L.cli(["wiz-import", str(export)], path=p, out=said.append) == 1


def test_wiz_light_roundtrips_and_merges() -> None:
    lt = L.Light.from_dict({"name": "bulb", "kind": "wiz", "ip": "192.0.2.5", "device": "aabb", "sku": "ESP"})
    assert lt.to_dict() == {"name": "bulb", "kind": "wiz", "ip": "192.0.2.5", "device": "aabb", "sku": "ESP"}
    merged, new = L.merge([lt], [L.Light("wiz aabb", "wiz", ip="192.0.2.9", device="aabb")])
    assert new == [] and merged[0].name == "bulb" and merged[0].ip == "192.0.2.9"


# ---- Triones ----

def test_triones_frames_bytes() -> None:
    assert L.TRIONES_ON == bytes.fromhex("cc2333") and L.TRIONES_OFF == bytes.fromhex("cc2433")
    assert L.triones_color((1, 2, 3)) == bytes.fromhex("56010203 00f0aa".replace(" ", ""))
    assert L.triones_frames(L.Change(power=False), None) == [L.TRIONES_OFF]


def test_triones_status_parse() -> None:
    st = L.triones_parse_status(bytes.fromhex("66e3234120017b0000000399"))
    assert st is not None and st.on is True and st.rgb == (123, 0, 0) and st.brightness == 48
    off = L.triones_parse_status(bytes.fromhex("66e3244120017b0000000399"))
    assert off is not None and off.on is False
    assert L.triones_parse_status(b"\x66\x01") is None


def test_triones_colour_keeps_current_level() -> None:
    cur = L.State(on=True, rgb=(128, 0, 0), brightness=50)
    frames = L.triones_frames(L.Change(color=L.parse_color("blue")), cur)
    assert frames == [L.TRIONES_ON, L.triones_color((0, 0, 128))]


def test_triones_level_keeps_current_colour() -> None:
    cur = L.State(on=True, rgb=(255, 0, 0), brightness=100)
    assert L.triones_frames(L.Change(brightness=20), cur)[-1] == L.triones_color((51, 0, 0))


def test_triones_colour_on_an_off_light_is_full() -> None:
    cur = L.State(on=False, rgb=(10, 0, 0))
    assert L.triones_frames(L.Change(color=L.parse_color("green")), cur)[-1] == L.triones_color((0, 255, 0))


# ---- Tuya ----

def test_tuya_temp_percent() -> None:
    assert L.tuya_temp_percent(2700) == 0 and L.tuya_temp_percent(6500) == 100
    assert L.tuya_temp_percent(1000) == 0 and L.tuya_temp_percent(4000) == 34


def test_tuya_without_key_says_so() -> None:
    d = L.TuyaDriver(L.Light("b", "tuya", id="x"))
    with pytest.raises(PermissionError):
        asyncio.run(d.apply(L.Change(power=True)))


# ---- the file ----

def test_load_save_roundtrip_is_private(tmp_path: Any) -> None:
    p = tmp_path / "lights.json"
    h, _ = hub()
    L.save(h.lights, p)
    assert stat.S_IMODE(p.stat().st_mode) == 0o600
    back = L.load(p)
    assert [lt.to_dict() for lt in back] == [lt.to_dict() for lt in h.lights]
    assert "address" not in back[0].to_dict() and "key" not in back[0].to_dict()


def test_load_skips_bad_entries(tmp_path: Any) -> None:
    p = tmp_path / "lights.json"
    p.write_text(json.dumps({"lights": [{"name": "x", "kind": "hue"}, {"kind": "govee"}, {"name": "ok", "kind": "govee"}]}))
    assert [lt.name for lt in L.load(p)] == ["ok"]
    assert L.load(tmp_path / "missing.json") == []


def test_merge_keeps_names_and_takes_new_address() -> None:
    known = [L.Light("floor lamp", "govee", room="bedroom", ip="192.0.2.10", device="AA:BB")]
    found = [L.Light("govee h6076", "govee", ip="192.0.2.99", device="AA:BB"),
             L.Light("floor lamp", "triones", address="u2")]
    merged, new = L.merge(known, found)
    assert merged[0].name == "floor lamp" and merged[0].ip == "192.0.2.99" and merged[0].room == "bedroom"
    assert [n.name for n in new] == ["floor lamp 2"]


def test_make_lights_off_switch_and_empty(tmp_path: Any) -> None:
    p = tmp_path / "lights.json"
    assert L.make_lights(p, {}) is None
    L.save([L.Light("a", "govee")], p)
    assert L.make_lights(p, {"EVE_LIGHTS": "0"}) is None
    assert L.make_lights(p, {}) is not None


# ---- which lights ----

def test_resolve() -> None:
    h, _ = hub()
    names = lambda t: [lt.name for lt in h.resolve(t)[0]]  # noqa: E731
    assert names("all") == names("the lights") == names("") == ["floor lamp", "strip", "desk bulb"]
    assert names("Floor Lamp") == ["floor lamp"]
    assert names("bedroom") == names("bedroom lights") == ["floor lamp", "strip"]
    assert names("lamp") == ["floor lamp"]
    assert names("led strip") == ["strip"]
    assert names("govee") == ["floor lamp"] and names("sylvania") == ["desk bulb"]
    got, why = h.resolve("garage")
    assert got == [] and "floor lamp" in why


# ---- the code path ----

@pytest.mark.parametrize("text,names,change", [
    ("lights off", "all", L.Change(power=False)),
    ("Turn off the lights", "all", L.Change(power=False)),
    ("turn the lights on please", "all", L.Change(power=True)),
    ("lights out", "all", L.Change(power=False)),
    ("lights blue", "all", L.Change(color=L.parse_color("blue"))),
    ("set all the lights to purple", "all", L.Change(color=L.parse_color("purple"))),
    ("make the lights warm white", "all", L.Change(color=L.parse_color("warm white"))),
    ("lights to 2700K", "all", L.Change(color=L.parse_color("2700k"))),
    ("lights #ff8800", "all", L.Change(color=L.parse_color("#ff8800"))),
    ("floor lamp red", ("floor lamp",), L.Change(color=L.parse_color("red"))),
    ("dim the bedroom lights", ("floor lamp", "strip"), L.Change(brightness=L.DIM_PERCENT)),
    ("bedroom lights to 30%", ("floor lamp", "strip"), L.Change(brightness=30)),
    ("set the lamp to blue at 40%", ("floor lamp",), L.Change(brightness=40, color=L.parse_color("blue"))),
    ("turn off the lamp and the desk bulb", ("floor lamp", "desk bulb"), L.Change(power=False)),
    ("led strip pink", ("strip",), L.Change(color=L.parse_color("pink"))),
])
def test_match(text: str, names: Any, change: L.Change) -> None:
    h, _ = hub()
    cmd = h.match(text)
    assert cmd is not None, text
    assert cmd.change == change
    want = tuple(lt.name for lt in h.lights) if names == "all" else names
    assert cmd.names == want


@pytest.mark.parametrize("text", [
    "turn off the lights in ten minutes", "are the lights on", "why are the lights blue", "blue",
    "make it cozy", "lights", "turn on the tv", "what's the weather", "lights red and blue",
    "lights on off", "open spotify", "",
])
def test_match_leaves_the_rest_to_the_model(text: str) -> None:
    h, _ = hub()
    assert h.match(text) is None, text


def test_match_without_lights_is_none() -> None:
    assert L.Lights([]).match("lights off") is None


def test_run_by_code_reports_each() -> None:
    h, calls = hub()
    line = asyncio.run(h.run(h.match("lights off")))
    assert line == "Lights off." and len(calls) == 3
    h2, _ = hub(fail={"strip"})
    line2 = asyncio.run(h2.run(h2.match("lights blue at 50%")))
    assert line2 == "Floor lamp and desk bulb blue at 50%. strip: unreachable."


# ---- the tools ----

def test_tools_are_strict_and_complete() -> None:
    h, _ = hub()
    for t in h.tools():
        params = t["parameters"]
        assert t["strict"] is True and params["additionalProperties"] is False
        assert sorted(params["required"]) == sorted(params["properties"])
    assert set(L.TOOL_NAMES) == {"lights_set", "lights_match", "lights_status"}
    assert "floor lamp (room: bedroom)" in h.instructions() and "led strip" in h.instructions()


def test_handle_set_and_status() -> None:
    h, calls = hub(fail={"desk bulb"})
    out = asyncio.run(h.handle("lights_set", {"target": "all", "power": None, "brightness": 30, "color": "warm white"}))
    assert out["ok"] and out["done"] == ["floor lamp", "strip"] and out["failed"][0]["name"] == "desk bulb"
    assert calls[0][1] == L.Change(brightness=30, color=L.parse_color("warm white"))
    bad = asyncio.run(h.handle("lights_set", {"target": "all", "power": None, "brightness": None, "color": "plaid"}))
    assert bad["ok"] is False and "plaid" in bad["reason"]
    none = asyncio.run(h.handle("lights_set", {"target": "all", "power": None, "brightness": None, "color": None}))
    assert none["ok"] is False
    st = asyncio.run(h.handle("lights_status", {"target": "bedroom"}))
    assert [x["name"] for x in st["lights"]] == ["floor lamp", "strip"]
    assert st["lights"][0] == {"name": "floor lamp", "room": "bedroom", "reachable": True, "on": True,
                               "brightness": 40, "color": "blue", "rgb": "#0000ff"}
    st2 = asyncio.run(h.handle("lights_status", {"target": "office"}))
    assert st2["lights"][0]["reachable"] is False


# ---- matching one light to the others ----

def test_change_of_state() -> None:
    assert L.change_of(L.State(on=False, brightness=50, rgb=(255, 0, 0))) == L.Change(power=False)
    c = L.change_of(L.State(on=True, brightness=40, rgb=(0, 0, 102)))
    assert c.power is True and c.brightness == 40 and c.color.rgb == (0, 0, 255) and c.color.word == "blue"
    w = L.change_of(L.State(on=True, brightness=80, kelvin=2700, rgb=(0, 0, 0)))
    assert w.color == L.Color(kelvin=2700, word="2700K")


@pytest.mark.parametrize("text,names,source", [
    ("match the lights", "all", ""),
    ("sync lights", "all", ""),
    ("desk bulb same as the others", ("desk bulb",), "the others"),
    ("make the desk bulb the same light as the other lights rn", ("desk bulb",), "the other lights"),
    ("make the strip match the lamp", ("strip",), "the lamp"),
    ("match the desk bulb to the floor lamp please", ("desk bulb",), "the floor lamp"),
    ("match the desk bulb", ("desk bulb",), ""),
])
def test_match_copy(text: str, names: Any, source: str) -> None:
    h, _ = hub()
    cmd = h.match(text)
    assert cmd is not None and cmd.source == source, text
    assert cmd.names == (tuple(lt.name for lt in h.lights) if names == "all" else names)


@pytest.mark.parametrize("text", ["make the lights like a sunset", "match the garage to the lamp",
                                  "the strip is the same as yesterday"])
def test_match_copy_needs_real_lights(text: str) -> None:
    h, _ = hub()
    cmd = h.match(text)
    assert cmd is None or cmd.source is None


def test_copy_reads_a_wifi_light_first_and_sets_the_rest() -> None:
    h, calls = hub()
    out = asyncio.run(h.copy("all"))
    assert out["ok"] and out["source"] == "floor lamp" and out["done"] == ["strip", "desk bulb"]
    assert calls[0][1] == L.Change(power=True, brightness=40, color=L.Color(rgb=(0, 0, 255), word="blue"))
    assert out["line"] == "Strip and desk bulb matched to floor lamp: blue at 40%."


def test_copy_skips_a_source_that_does_not_answer() -> None:
    h, calls = hub(fail={"floor lamp"})
    out = asyncio.run(h.copy("strip"))
    assert out["source"] == "desk bulb" and [c[0] for c in calls] == ["strip"]


def test_copy_by_code_and_by_tool() -> None:
    h, calls = hub()
    line = asyncio.run(h.run(h.match("make the strip match the desk bulb")))
    assert line == "Strip matched to desk bulb: blue at 40%." and [c[0] for c in calls] == ["strip"]
    out = asyncio.run(h.handle("lights_match", {"target": "desk bulb", "source": None}))
    assert out["ok"] and out["source"] == "floor lamp" and "line" not in out
    bad = asyncio.run(h.handle("lights_match", {"target": "garage", "source": None}))
    assert bad["ok"] is False


def test_a_slow_light_times_out_alone() -> None:
    class Slow(FakeDriver):
        async def apply(self, change: L.Change) -> None:
            await asyncio.sleep(5)
    calls: list = []
    lights = [L.Light("a", "govee"), L.Light("b", "triones")]
    h = L.Lights(lights, drivers={"govee": lambda lt: FakeDriver(lt, calls, set()),
                                  "triones": lambda lt: Slow(lt, calls, set()), "tuya": lambda lt: None},
                 timeout=0.05)
    out = asyncio.run(h.set("all", L.Change(power=True)))
    assert out["done"] == ["a"] and out["failed"] == [{"name": "b", "reason": "it did not answer in time"}]


def test_cli_name_sets_room_and_key(tmp_path: Any) -> None:
    p = tmp_path / "lights.json"
    L.save([L.Light("tuya abcd", "tuya", id="x", ip="192.0.2.5")], p)
    lines: list[str] = []
    assert L.cli(["name", "tuya abcd", "desk bulb", "--room", "office", "--key", "secret"], p, lines.append) == 0
    lt = L.load(p)[0]
    assert (lt.name, lt.room, lt.key) == ("desk bulb", "office", "secret")
    assert "secret" not in "\n".join(lines)
