"""The browser lane's `fill` step: plan_contract accepts it, plan_executor runs it, browser_lane's page fills
the form from the vault (vault.py), and no value reaches a planner request, a snapshot, a ledger or a picture.

The live tests launch a real headless Chromium against local pages (EVE_LIVE=1); the rest run on fakes.
"""

from __future__ import annotations

import asyncio
import base64
import functools
import http.server
import io
import json
import threading
from pathlib import Path
from typing import Any, Optional

import pytest

from eve_bridge import plan_contract as pc
from eve_bridge.browser_lane import MASK_COLOR, BrowserLane, BrowserLaneConfig, _Page
from eve_bridge.plan_executor import run_plan
from eve_bridge.vault import Keychain, Vault, totp


class MemKeychain(Keychain):
    """The Keychain wrapper over a dict: the vault's own logic, no /usr/bin/security."""

    def __init__(self) -> None:
        super().__init__(keychain="")
        self.items: dict[str, dict[str, Any]] = {}

    def put(self, handle: str, value: dict[str, Any]) -> None:
        self.items[handle] = dict(value)

    def get(self, handle: str) -> Optional[dict[str, Any]]:
        return dict(self.items[handle]) if handle in self.items else None

    def delete(self, handle: str) -> bool:
        return self.items.pop(handle, None) is not None


def mem_vault(tmp_path: Path) -> Vault:
    return Vault(tmp_path / "vault.json", MemKeychain())


# ---- the plan contract ----------------------------------------------------------------------------------------

def test_a_fill_step_names_a_vault_handle_and_nothing_else() -> None:
    step = pc.parse_step({"kind": "fill", "target": "the sign-in form", "text": "v_ab12cd34"}, 1, "sign me in")
    assert step.kind == "fill" and step.text == "v_ab12cd34"
    assert step.describe() == "fill the sign-in form from vault item v_ab12cd34"
    for bad in ({"kind": "fill", "target": "the sign-in form", "text": "hunter2"},
                {"kind": "fill", "target": "the form", "text": "v_AB12CD34"},
                {"kind": "fill", "target": "", "text": "v_ab12cd34"}):
        with pytest.raises(pc.PlanError):
            pc.parse_step(bad, 1, "sign me in")
    assert "fill" in pc.PLAN_SCHEMA["properties"]["steps"]["items"]["properties"]["kind"]["enum"]
    assert "- fill:" in pc.PLAN_INSTRUCTIONS_BROWSER and "- fill:" in pc.PLAN_INSTRUCTIONS_READ
    assert "- fill:" not in pc.PLAN_INSTRUCTIONS                # the Mac planner has no vault


def test_the_planner_is_told_the_vaults_lines_in_the_browser_only() -> None:
    lines = ['v_ab12cd34 login "Chase" for chase.com', 'v_ef56gh78 card "Visa"']
    text = pc.plan_request_text("sign in to chase", app="browser", outline=["button: Sign in"], apps=[],
                                site="chase.com", vault=lines)
    assert "Saved in the vault (id, kind, label, site):" in text
    assert '- v_ab12cd34 login "Chase" for chase.com' in text and '- v_ef56gh78 card "Visa"' in text
    empty = pc.plan_request_text("sign in", app="browser", outline=[], apps=[], vault=[])
    assert "Saved in the vault: nothing" in empty
    mac = pc.plan_request_text("open notes", app="Finder", outline=[], apps=["Notes"], vault=lines)
    assert "vault" not in mac
    req = pc.plan_request("m", "sign in", app="browser", outline=[], apps=[], effort="low", timeout=5,
                          browser=True, vault=lines)
    assert "v_ab12cd34" in req["input"][0]["content"][0]["text"]


# ---- the executor ---------------------------------------------------------------------------------------------

class MacEffectors:
    """The Mac lane's effectors: no vault."""

    def settle(self, secs: float) -> float:
        return secs


class Effectors(MacEffectors):
    def __init__(self, said: str) -> None:
        self.said, self.asked = said, []

    def fill_from_vault(self, handle: str) -> str:
        self.asked.append(handle)
        return self.said


def _run(effectors: Any) -> Any:
    plan = pc.parse_plan({"steps": [{"kind": "fill", "target": "the sign-in form", "text": "v_ab12cd34"}],
                          "final_say": "Filled.", "success": None}, "sign in")
    return run_plan(plan, senses=None, effectors=effectors, asker=None, open_app=str, open_url=str,
                    frontmost_app=lambda: "browser")


def test_the_executor_runs_a_fill_through_the_page_and_stops_on_a_refusal() -> None:
    ok = Effectors("filled username, password from 'Chase'")
    result = _run(ok)
    # No success check: every step ran, nothing confirms the sign-in, so partial (test_plan_success_guard).
    assert ok.asked == ["v_ab12cd34"] and result.status == "partial" and result.reason == "no_success_check"
    assert result.ledger[0].effect == "confirmed" and result.ledger[0].how == "code"
    half = _run(Effectors("filled username from 'Chase' but could not fill the password (TimeoutError)"))
    assert half.reason == "no_success_check" and half.ledger[0].effect == "unverifiable"
    refused = _run(Effectors('refused: "Chase" is saved for chase.com, not this page'))
    assert refused.status == "none" and refused.reason == "fill_refused"
    mac = _run(MacEffectors())
    assert mac.status == "none" and "only in the browser" in mac.ledger[0].note


class RecordingPage:
    url = "https://chase.com.evil.example/login"

    def __init__(self) -> None:
        self.calls: list[str] = []

    def __getattr__(self, name: str) -> Any:
        def record(*a: Any, **k: Any) -> Any:
            self.calls.append(name)
            return []
        return record


def test_the_page_refuses_before_touching_the_page_off_site_or_without_a_vault(tmp_path: Path) -> None:
    v = mem_vault(tmp_path)
    item = v.add("login", "Chase", {"username": "me", "password": "pw"}, site="chase.com")
    page = RecordingPage()
    said = _Page(page, vault=v).fill_from_vault(item.handle)
    assert said.startswith("refused:") and "saved for chase.com" in said and page.calls == []
    assert _Page(page, vault=None).fill_from_vault(item.handle) == "refused: there is no vault"
    assert _Page(page, vault=v).fill_from_vault("v_zzzzzzzz").startswith("refused: no vault item")


# ---- live: a real Chromium -------------------------------------------------------------------------------------

LOGIN = """<!doctype html><html><head><title>Sign in</title></head><body><form id=f onsubmit="event.preventDefault()">
<input id=user type=text><input id=pw type=password><button>Sign in</button></form></body></html>"""
CARD = """<!doctype html><html><head><title>Checkout</title></head><body><form>
<label for=n>Card number</label><input id=n autocomplete="cc-number">
<label for=e>Expiry</label><input id=e autocomplete="cc-exp">
<label for=c>CVC</label><input id=c autocomplete="cc-csc">
<label for=h>Name on card</label><input id=h autocomplete="cc-name"></form></body></html>"""
CARD_PLAIN = """<!doctype html><html><head><title>Pay</title></head><body><form>
<label for=n>Card number</label><input id=n name=cardnumber>
<label for=m>Expiry month</label><select id=m name=exp-month><option value="">MM</option>""" + "".join(
    f"<option value='{m:02d}'>{m:02d}</option>" for m in range(1, 13)) + """</select>
<label for=y>Expiry year</label><select id=y name=exp-year><option value="">YYYY</option>""" + "".join(
    f"<option>{y}</option>" for y in range(2026, 2036)) + """</select>
<label for=c>Security code</label><input id=c name=cvv></form></body></html>"""
OTP = """<!doctype html><html><head><title>Verify</title></head><body><form>
<label for=o>Enter the code from your app</label><input id=o autocomplete="one-time-code" inputmode=numeric></form></body></html>"""
CANARIES = {"username": "canary-user@example.com", "password": "canary-pw-9f3!", "number": "4000056655665556",
            "cvc": "713", "name": "Canary Holder"}
SECRET_B32 = base64.b32encode(b"12345678901234567890").decode()


def _serve(tmp_path: Path, pages: dict[str, str]) -> tuple[Any, str]:
    for name, html in pages.items():
        (tmp_path / name).write_text(html)
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(tmp_path))
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    srv.RequestHandlerClass.log_message = lambda *a, **k: None
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, f"http://127.0.0.1:{srv.server_address[1]}"


def _plan(handle: str, target: str) -> dict[str, Any]:
    return {"steps": [{"kind": "fill", "target": target, "label_hint": "", "text": handle, "key": "",
                       "expect": None, "consequential": False}], "final_say": "Filled.", "success": None}


def _pixel_mean(jpeg: bytes, box: dict[str, float]) -> tuple[float, float, float]:
    from PIL import Image
    img = Image.open(io.BytesIO(jpeg)).convert("RGB")
    x0, y0 = int(box["x"] + 3), int(box["y"] + 3)
    x1, y1 = int(box["x"] + box["width"] - 3), int(box["y"] + box["height"] - 3)
    px = [img.getpixel((x, y)) for x in range(x0, x1, 2) for y in range(y0, y1, 2)]
    return tuple(sum(p[i] for p in px) / len(px) for i in range(3))  # type: ignore[return-value]


@pytest.mark.live
def test_live_fill_fills_each_form_and_no_value_leaves_the_page(tmp_path: Path) -> None:
    srv, base = _serve(tmp_path, {"login.html": LOGIN, "card.html": CARD, "pay.html": CARD_PLAIN, "otp.html": OTP})
    v = Vault(tmp_path / "vault.json", MemKeychain(), clock=lambda: 1_234_567_890.0)
    login = v.add("login", "Local", {"username": CANARIES["username"], "password": CANARIES["password"]}, site=base)
    elsewhere = v.add("login", "Elsewhere", {"username": "x", "password": "elsewhere-pw"}, site="other.example")
    card = v.add("card", "Visa", {"number": CANARIES["number"], "exp_month": "7", "exp_year": "2031",
                                  "cvc": CANARIES["cvc"], "name": CANARIES["name"]})
    code = v.add("totp", "Local 2FA", {"secret": SECRET_B32}, site=base)
    want_code = totp(SECRET_B32, at=1_234_567_890.0)
    lane = BrowserLane(BrowserLaneConfig(profile=tmp_path / "profile", headless=True), vault=v)
    seen: list[Any] = []                                   # everything a model or a log could be shown

    def check(js: str, arg: Any) -> bool:
        return bool(lane._ensure().page.evaluate(js, arg))

    async def go() -> dict[str, Any]:
        out: dict[str, Any] = {}
        await lane.open_url(f"{base}/login.html")
        outline = await lane.outline()
        seen.append(outline)
        seen.append(pc.plan_request_text("sign me in", app="browser", outline=outline["lines"], apps=[],
                                         site=outline["site"], vault=outline["vault"]))
        refused = await lane.run_plan(_plan(elsewhere.handle, "the sign-in form"), "sign me in")
        seen.append(refused)
        out["refused"] = refused
        out["empty_after_refusal"] = await lane._run(check, "() => !document.getElementById('user').value && !document.getElementById('pw').value", None)
        result = await lane.run_plan(_plan(login.handle, "the sign-in form"), "sign me in")
        seen.append(result)
        out["login"] = result
        out["login_ok"] = await lane._run(check, "w => document.getElementById('user').value === w[0] && document.getElementById('pw').value === w[1]",
                                          [CANARIES["username"], CANARIES["password"]])
        snap = await lane._run(lambda: lane._ensure().snapshot())
        seen.append([c.to_dict() for c in snap.elements])
        seen.append(await lane.outline())
        box = await lane._run(lambda: lane._ensure().page.locator("#pw").bounding_box())
        masked = await lane.screenshot()
        plain = await lane._run(lambda: lane._ensure().page.screenshot(type="jpeg", quality=85))
        out["masked"], out["plain"] = _pixel_mean(masked, box), _pixel_mean(plain, box)
        ubox = await lane._run(lambda: lane._ensure().page.locator("#user").bounding_box())
        out["user_masked"] = _pixel_mean(masked, ubox)

        await lane.open_url(f"{base}/card.html")
        result = await lane.run_plan(_plan(card.handle, "the card form"), "pay with my visa")
        seen.append(result)
        out["card"] = result
        out["card_ok"] = await lane._run(check, "w => ['n','e','c','h'].every((id, i) => document.getElementById(id).value === w[i])",
                                         [CANARIES["number"], "07/31", CANARIES["cvc"], CANARIES["name"]])
        seen.append(await lane.outline())

        await lane.open_url(f"{base}/pay.html")
        result = await lane.run_plan(_plan(card.handle, "the payment form"), "pay with my visa")
        seen.append(result)
        out["pay"] = result
        out["pay_ok"] = await lane._run(check, "w => ['n','m','y','c'].every((id, i) => document.getElementById(id).value === w[i])",
                                        [CANARIES["number"], "07", "2031", CANARIES["cvc"]])
        seen.append(await lane.outline())

        await lane.open_url(f"{base}/otp.html")
        result = await lane.run_plan(_plan(code.handle, "the code field"), "enter my 2fa code")
        seen.append(result)
        out["otp_ok"] = await lane._run(check, "w => document.getElementById('o').value === w", want_code)
        seen.append(await lane.outline())
        await lane.close()
        return out

    try:
        out = asyncio.run(go())
    finally:
        srv.shutdown()
    assert out["refused"]["status"] == "none" and "saved for other.example" in json.dumps(out["refused"])
    assert out["empty_after_refusal"] is True
    assert out["login"]["reason"] == "no_success_check", out["login"]     # ran; no check: never "complete"
    assert out["login_ok"] is True and out["card_ok"] is True and out["pay_ok"] is True and out["otp_ok"] is True
    assert out["card"]["reason"] == out["pay"]["reason"] == "no_success_check", (out["card"], out["pay"])
    dump = json.dumps(seen)
    for name, value in [*CANARIES.items(), ("code", want_code), ("elsewhere", "elsewhere-pw")]:
        assert value not in dump, (name, dump[:2000])
    assert f'- {login.handle} login "Local" for 127.0.0.1' in seen[1]       # the planner did get the vault
    mask = tuple(int(MASK_COLOR[i:i + 2], 16) for i in (1, 3, 5))
    close = lambda a, b: all(abs(x - y) < 24 for x, y in zip(a, b, strict=True))           # noqa: E731 — JPEG noise
    assert close(out["masked"], mask) and close(out["user_masked"], mask), out["masked"]
    assert not close(out["plain"], mask), out["plain"]                          # positive control: unmasked differs
