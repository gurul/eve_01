"""vault_mcp.py: the vault's two tools for Codex tasks, which focused field a value may go into, and the Codex
wiring (codex_computer: the thread's eve_vault server, its paragraph, its approvals).

The live test opens a headed Chromium with another app in front (as a Codex task leaves the browser) and fills
its fields through the Accessibility API, so it runs only with EVE_LIVE=1.
"""

from __future__ import annotations

import base64
import io
import json
import time
from pathlib import Path
from typing import Any, Optional

import pytest

from eve_bridge import codex_computer, vault_mcp
from eve_bridge.vault import Keychain, Vault, totp
from eve_bridge.vault_mcp import Server
from eve_bridge.vault_native import Field


class MemKeychain(Keychain):
    def __init__(self) -> None:
        super().__init__(keychain="")
        self.items: dict[str, dict[str, Any]] = {}

    def put(self, handle: str, value: dict[str, Any]) -> None:
        self.items[handle] = dict(value)

    def get(self, handle: str) -> Optional[dict[str, Any]]:
        return dict(self.items[handle]) if handle in self.items else None

    def delete(self, handle: str) -> bool:
        return self.items.pop(handle, None) is not None


class FakeMac:
    """Pages and their fields; page_fields walks only the pages ``want`` accepts, as the real one does."""

    def __init__(self, *fields: Field, takes: bool = True, count: Optional[int] = None) -> None:
        self.fields, self.takes, self.count = list(fields), takes, count
        self.set: list[tuple[str, str]] = []                   # (the field's words, the value)
        self.walked: set[str] = set()

    def trusted(self) -> bool:
        return True

    def page_fields(self, want: Any, frames: bool = False) -> list[Field]:
        out = [f for f in self.fields if want(f.url) or (frames and f.page and want(f.page))]
        self.walked |= {f.url for f in out}
        return out

    def set_value(self, field: Field, text: str) -> bool:
        if self.takes:
            self.set.append((field.words, text))
        return self.takes

    def char_count(self, field: Field) -> Optional[int]:
        return self.count if self.count is not None else (len(self.set[-1][1]) if self.set else 0)


CANARY = {"password": "mcp-canary-pw!", "number": "4000056655665556", "cvc": "713"}
SECRET = base64.b32encode(b"12345678901234567890").decode()
LOGIN = "https://secure.chase.com/login"


def setup(tmp_path: Path) -> tuple[Vault, dict[str, str]]:
    v = Vault(tmp_path / "vault.json", MemKeychain(), clock=lambda: 59.0)
    ids = {"login": v.add("login", "Chase", {"username": "me@example.com", "password": CANARY["password"]}, site="chase.com").handle,
           "card": v.add("card", "Visa", {"number": CANARY["number"], "exp_month": "7", "exp_year": "2031", "cvc": CANARY["cvc"]}).handle,
           "totp": v.add("totp", "Chase 2FA", {"secret": SECRET}, site="chase.com").handle}
    return v, ids


def f(words: str = "", url: str = LOGIN, secure: bool = False, order: int = 1, focused: bool = False,
      role: str = "AXTextField") -> Field:
    return Field(pid=1, app="Google Chrome", role=role, subrole="AXSecureTextField" if secure else "", url=url,
                 words=words, focused=focused, order=order)


def test_the_server_speaks_mcp_and_lists_no_value(tmp_path: Path) -> None:
    v, ids = setup(tmp_path)
    out = io.StringIO()
    server = Server(vault=v, native=FakeMac(), out=out)
    for msg in ({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}},
                {"jsonrpc": "2.0", "method": "notifications/initialized"},
                {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
                {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "vault_list", "arguments": {}}},
                {"jsonrpc": "2.0", "id": 4, "method": "nope"}):
        server.handle_line(json.dumps(msg))
    server.handle_line("not json")
    replies = [json.loads(line) for line in out.getvalue().splitlines()]
    assert [r.get("id") for r in replies] == [1, 2, 3, 4, None]
    assert replies[0]["result"]["serverInfo"]["name"] == "eve_vault" and replies[0]["result"]["protocolVersion"] == "2025-06-18"
    assert [t["name"] for t in replies[1]["result"]["tools"]] == ["vault_list", "vault_fill"]
    listing = replies[2]["result"]["content"][0]["text"]
    assert f'{ids["login"]} login "Chase" for chase.com' in listing and f'{ids["card"]} card "Visa"' in listing
    assert replies[3]["error"]["code"] == -32601 and replies[4]["error"]["code"] == -32700
    for value in (*CANARY.values(), "me@example.com", SECRET):
        assert value not in out.getvalue()


def test_a_fill_finds_the_field_by_what_it_is_on_the_items_own_page(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(vault_mcp, "FIELD_WAITS", (0.0,))
    monkeypatch.setattr(vault_mcp, "COUNT_WAIT", 0.0)
    v, ids = setup(tmp_path)
    answers: list[str] = []

    def fill(mac: FakeMac, which: str, part: str, page: str = "", label: str = "") -> tuple[bool, list[tuple[str, str]]]:
        result = Server(vault=v, native=mac).call("vault_fill", {"id": ids[which], "part": part, "page": page, "field": label})
        answers.append(result["content"][0]["text"])
        return not result["isError"], mac.set

    signin = [f("", order=1), f("Password", secure=True, order=2)]
    other_site = f("Password", url="https://chase.com.evil.example/login", secure=True, order=1)
    mac = FakeMac(*signin, other_site)
    assert fill(mac, "login", "password")[1] == [("Password", CANARY["password"])]
    assert "into the password field on secure.chase.com" in answers[-1]
    assert mac.walked == {LOGIN}                                    # the other site's page was never read
    assert fill(FakeMac(*signin), "login", "username")[1] == [("", "me@example.com")]   # the field before the password
    named = [f("Search", order=1), f("Email address", order=2), f("Password", secure=True, order=3)]
    assert fill(FakeMac(*named), "login", "username")[1] == [("Email address", "me@example.com")]
    focus = [f("Search", order=1), f("Email address", order=2, focused=True), f("Password", secure=True, order=3)]
    assert fill(FakeMac(*focus), "login", "username")[1] == [("Email address", "me@example.com")]
    pay = "https://shop.example/checkout"
    card = [f("Card number", url=pay, order=1), f("Expiry (MM/YY)", url=pay, order=2), f("CVC", url=pay, order=3)]
    assert fill(FakeMac(*card), "card", "number", page=pay)[1] == [("Card number", CANARY["number"])]
    assert fill(FakeMac(*card), "card", "exp", page=pay + "#pay")[1] == [("Expiry (MM/YY)", "07/31")]
    assert fill(FakeMac(*card), "card", "cvc", page=pay)[1] == [("CVC", CANARY["cvc"])]
    framed = [f("Email", url=pay, order=1), Field(pid=1, app="Google Chrome", role="AXTextField", subrole="",
              url="https://js.payments.example/frame", page=pay, words="Card number", order=2)]
    assert fill(FakeMac(*framed), "card", "number", page=pay)[1] == [("Card number", CANARY["number"])]
    login_framed = [Field(pid=1, app="Google Chrome", role="AXTextField", subrole="AXSecureTextField",
                          url="https://evil.example/frame", page=LOGIN, words="Password", order=1)]
    assert not fill(FakeMac(*login_framed), "login", "password")[0]          # a login never fills another site's frame
    assert fill(FakeMac(f("Enter the code", order=1)), "totp", "code")[1] == [("Enter the code", totp(SECRET, at=59.0))]
    two_codes = [f("Code from SMS", order=1), f("Backup code", order=2)]
    assert fill(FakeMac(*two_codes), "totp", "code", label="SMS")[1] == [("Code from SMS", totp(SECRET, at=59.0))]
    two_pages = [f("Password", secure=True), f("Password", url="https://chase.com/other", secure=True)]
    assert fill(FakeMac(*two_pages), "login", "password", page="https://chase.com/other")[0]
    refusals = [
        (FakeMac(other_site), "login", "password", "", "", "No page on chase.com with a field is in front"),
        (FakeMac(f("Code", url="https://example.com/")), "totp", "code", "", "", "No page on chase.com"),
        (FakeMac(f("Email")), "login", "password", "", "", "only into a password field"),
        (FakeMac(*signin), "login", "password", "", "Email", "No field labelled 'Email'"),
        (FakeMac(*card), "card", "number", "", "", "Give the URL of the page"),
        (FakeMac(*two_pages), "login", "password", "", "", "2 pages on chase.com are open"),
        (FakeMac(*two_codes), "totp", "code", "", "", "several fields the code could go into ('Code from SMS', 'Backup code')"),
        (FakeMac(f("Password", secure=True), takes=False), "login", "password", "", "", "would not take a value"),
        (FakeMac(*signin), "login", "cvc", "", "", "parts username, password"),
    ]
    for mac, which, part, page, label, why in refusals:
        ok, done = fill(mac, which, part, page, label)
        assert not ok and done == [] and why in answers[-1], (why, answers[-1])
    unknown = Server(vault=v, native=FakeMac(*signin)).call("vault_fill", {"id": "v_zzzzzzzz", "part": "password"})
    assert unknown["isError"] and "No vault item" in unknown["content"][0]["text"]
    odd = FakeMac(f("Card number", url=pay), count=19)
    assert fill(odd, "card", "number", page=pay)[0] and "shows 19 characters where 16 were set" in answers[-1]
    for text in answers:
        for value in (*CANARY.values(), "me@example.com", totp(SECRET, at=59.0)):
            assert value not in text, (value, text)


# ---- the Codex wiring ------------------------------------------------------------------------------------------

async def _no(_: str) -> str:
    return "no"


def test_codex_threads_get_the_vault_server_only_when_the_vault_has_items(tmp_path: Path) -> None:
    agent = codex_computer.CodexComputerAgent(on_event=lambda e: None, ask_user=_no)
    agent.vault = Vault(tmp_path / "vault.json", MemKeychain())
    empty = agent._thread_params()
    assert "config" not in empty and empty["developerInstructions"] == codex_computer.INSTRUCTIONS
    agent.vault.add("card", "Visa", {"number": CANARY["number"], "exp_month": "7", "exp_year": "2031", "cvc": "713"})
    full = agent._thread_params()
    server = full["config"]["mcp_servers"]["eve_vault"]
    assert server["args"] == ["-I", "-m", "eve_bridge.vault_mcp"] and server["command"]
    assert full["developerInstructions"].endswith(codex_computer.VAULT_INSTRUCTIONS)
    assert full["sandbox"] == "read-only" and full["approvalPolicy"] == "on-request"
    assert CANARY["number"] not in json.dumps(full)


def test_codex_a_warm_thread_is_stale_once_the_vault_gains_or_loses_its_items(tmp_path: Path) -> None:
    agent = codex_computer.CodexComputerAgent(on_event=lambda e: None, ask_user=_no)
    agent.vault = Vault(tmp_path / "vault.json", MemKeychain())
    agent._thread_params()                                     # warmed with an empty vault
    assert not agent._vault_changed()
    item = agent.vault.add("card", "Visa", {"number": CANARY["number"], "exp_month": "7", "exp_year": "2031", "cvc": "713"})
    assert agent._vault_changed()                              # run() opens a thread with the vault
    agent._thread_params()
    assert not agent._vault_changed()
    agent.vault.remove(item.handle)
    assert agent._vault_changed()


def test_codex_accepts_only_the_vault_servers_own_tool_approvals() -> None:
    def form(server: str = "eve_vault", tool: str = "vault_fill", **meta: Any) -> dict[str, Any]:
        return {"serverName": server, "mode": "form", "requestedSchema": {"type": "object", "properties": {}},
                "_meta": {"codex_approval_kind": "mcp_tool_call", "tool_name": tool, **meta}}

    assert codex_computer.vault_tool_call(form()) and codex_computer.vault_tool_call(form(tool="vault_list"))
    assert not codex_computer.vault_tool_call(form(server="cua_repl"))
    assert not codex_computer.vault_tool_call(form(tool="something_else"))
    assert not codex_computer.vault_tool_call(form(codex_approval_kind="other"))
    live = {"serverName": "eve_vault", "mode": "form", "requestedSchema": {"type": "object", "properties": {}},
            "message": 'Allow the eve_vault MCP server to run tool "vault_fill"?',
            "_meta": {"codex_approval_kind": "mcp_tool_call", "persist": ["session", "always"], "tool_params": {}}}
    assert codex_computer.vault_tool_call(live)                  # Codex 0.159's real form (2026-10-12)
    assert not codex_computer.vault_tool_call({**live, "message": 'Allow the eve_vault MCP server to run tool "other"?'})
    assert not codex_computer.vault_tool_call({**live, "serverName": "cua_repl"})
    asks_for_input = form()
    asks_for_input["requestedSchema"] = {"type": "object", "properties": {"x": {"type": "string"}}}
    assert not codex_computer.vault_tool_call(asks_for_input)


def test_codex_vault_server_config_carries_the_vault_settings(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVE_VAULT_INDEX", "/tmp/x/vault.json")
    monkeypatch.delenv("EVE_VAULT_KEYCHAIN", raising=False)
    assert vault_mcp.server_config("/py")["env"] == {"EVE_VAULT_INDEX": "/tmp/x/vault.json"}


# ---- live: the real Mac ----------------------------------------------------------------------------------------

PAGE = """<!doctype html><html><head><title>Sign in</title></head><body style="margin:40px">
<form><input id=user placeholder="Email" style="width:300px;height:30px"><br><br>
<input id=pw type=password placeholder="Password" style="width:300px;height:30px"></form>
<script>window.ev = []; for (const id of ['user', 'pw']) for (const t of ['input', 'change'])
  document.getElementById(id).addEventListener(t, () => ev.push(id + ':' + t));</script></body></html>"""


@pytest.mark.live
def test_live_native_fill_sets_the_right_fields_with_the_browser_behind_and_unfocused(tmp_path: Path) -> None:
    import functools
    import http.server
    import subprocess
    import threading

    from AppKit import NSWorkspace
    from playwright.sync_api import sync_playwright

    from eve_bridge.vault_native import NativeMac

    (tmp_path / "login.html").write_text(PAGE)
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(tmp_path))
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    srv.RequestHandlerClass.log_message = lambda *a, **k: None
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    port = srv.server_address[1]
    here_url, there_url = f"http://127.0.0.1:{port}/login.html", f"http://localhost:{port}/login.html"
    v = Vault(tmp_path / "vault.json", MemKeychain())
    here = v.add("login", "Local", {"username": "me@example.com", "password": CANARY["password"]}, site=f"http://127.0.0.1:{port}").handle
    other = v.add("login", "Elsewhere", {"username": "x", "password": "elsewhere-pw"}, site="other.example").handle
    server = Server(vault=v, native=NativeMac())
    fill = lambda h, part, label="": server.call("vault_fill", {"id": h, "part": part, "page": here_url, "field": label})  # noqa: E731
    value = lambda page, id_: page.evaluate(f"() => document.getElementById('{id_}').value")  # noqa: E731
    try:
        with sync_playwright() as p:
            # Chrome for Testing builds its web Accessibility tree only when forced; the owner's Chrome had it on
            # (vault_native's docstring). Two windows: the item's site, and the same page on another host.
            browser = p.chromium.launch(headless=False, args=["--force-renderer-accessibility"])
            here_page, there_page = browser.new_page(), browser.new_page()
            here_page.goto(here_url)
            there_page.goto(there_url)
            # nothing focused, and another app in front, as a Codex task leaves the browser
            for pg in (here_page, there_page):
                pg.evaluate("() => document.activeElement && document.activeElement.blur()")
            subprocess.run(["open", "-a", "Finder"], check=True)      # activation from here is ignored; LaunchServices is not
            time.sleep(1.0)
            front = NSWorkspace.sharedWorkspace().frontmostApplication().bundleIdentifier()
            first = fill(other, "password")                            # another site's login: refused
            second = fill(here, "password", label="Email")             # a password into the Email field: refused
            third = fill(here, "username")
            fourth = fill(here, "password")
            got = {"here_user": value(here_page, "user"), "here_pw_ok": here_page.evaluate(
                       "w => document.getElementById('pw').value === w", CANARY["password"]),
                   "there": [value(there_page, "user"), value(there_page, "pw")], "events": here_page.evaluate("() => ev")}
            still = NSWorkspace.sharedWorkspace().frontmostApplication().bundleIdentifier()
            browser.close()
    finally:
        srv.shutdown()
    assert first["isError"] and "No page on other.example" in first["content"][0]["text"], first
    assert second["isError"] and "only into a password field" in second["content"][0]["text"], second
    assert not third["isError"] and not fourth["isError"], (third, fourth)
    assert got["here_user"] == "me@example.com" and got["here_pw_ok"] is True
    assert got["there"] == ["", ""]                                   # the same form on another host: untouched
    assert got["events"] == ["user:input", "user:change", "pw:input", "pw:change"]
    assert front == "com.apple.finder" and still == "com.apple.finder"      # the browser never came to the front
    for r in (first, second, third, fourth):
        assert CANARY["password"] not in json.dumps(r) and "elsewhere-pw" not in json.dumps(r)


CHECKOUT = """<!doctype html><html><head><title>Checkout</title></head><body style="margin:40px">
<label for=email>Email</label><input id=email style="width:300px;height:30px">
<iframe id=pay src="FRAME" style="width:420px;height:120px;border:0"></iframe></body></html>"""
CARD_FRAME = """<!doctype html><html><body><label for=cc>Card number</label>
<input id=cc style="width:300px;height:30px"></body></html>"""


@pytest.mark.live
def test_live_a_card_fills_a_payment_frame_from_another_host_inside_the_named_page(tmp_path: Path) -> None:
    import functools
    import http.server
    import threading

    from playwright.sync_api import sync_playwright

    from eve_bridge.vault_native import NativeMac

    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(tmp_path))
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    srv.RequestHandlerClass.log_message = lambda *a, **k: None
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    port = srv.server_address[1]
    (tmp_path / "frame.html").write_text(CARD_FRAME)
    (tmp_path / "checkout.html").write_text(CHECKOUT.replace("FRAME", f"http://localhost:{port}/frame.html"))
    checkout = f"http://127.0.0.1:{port}/checkout.html"
    v = Vault(tmp_path / "vault.json", MemKeychain())
    card = v.add("card", "Visa", {"number": CANARY["number"], "exp_month": "7", "exp_year": "2031", "cvc": "713"}).handle
    server = Server(vault=v, native=NativeMac())
    try:
        with sync_playwright() as p:
            browser = p.chromium.launch(headless=False, args=["--force-renderer-accessibility"])
            page = browser.new_page()
            page.goto(checkout)
            page.frame_locator("#pay").locator("#cc").wait_for()
            result = server.call("vault_fill", {"id": card, "part": "number", "page": checkout})
            in_frame = page.frame_locator("#pay").locator("#cc").evaluate("(e, w) => e.value === w", CANARY["number"])
            email = page.evaluate("() => document.getElementById('email').value")
            browser.close()
    finally:
        srv.shutdown()
    assert not result["isError"] and "shows" not in result["content"][0]["text"], result
    assert in_frame is True and email == ""
    assert CANARY["number"] not in json.dumps(result)
