"""The vault for Codex tasks: a stdio MCP server with two tools, ``vault_list`` and ``vault_fill``.

A Codex computer task (codex_computer.py) gets this server in its thread's config whenever the vault holds an
item. Codex calls ``vault_fill`` with an item's handle, the part it wants (``password``, ``number``, ``code``...)
and the page's URL; this server finds the field in that page through the Accessibility API and sets its value
(vault_native.py: no key is pressed, no focus is needed, the browser need not be in front). The value goes from
the Keychain to that one field; the tool's answer says what was filled, never what. ``vault_list`` gives the
lines the browser lane's planner sees (vault.Item.line).

Which page: for a login or 2FA item, a page on the item's site (vault.site_allows, against the URL the
Accessibility tree gives, never Codex's word); for a card, the page Codex names. Pages on other sites are never
walked into. Several such pages: the one Codex names, else the one with a focused field, else refused.

Which field, by what it is (vault_native's docstring says why focus cannot decide it):
  password   the page's password field (AXSecureTextField); a password never goes anywhere else
  username   the field the page has focused, else the one called a username or email, else the last text
             field before the password field, else the page's only text field
  card parts, code   the focused field, else the one field whose label fits the part, else the only one
Codex may name the field's label (``field``) to narrow it. Anything still ambiguous is refused with the
candidates' labels, never guessed. Afterwards the field's character count is compared with the value's length
(a site that reformats a card number answers "it shows N characters", which is said, not hidden).

Hand-written JSON-RPC 2.0 over newline-delimited stdio, the shape of eve_mcp.py. Nothing is logged: stdout is
the protocol, and nothing else is printed.
"""

from __future__ import annotations

import json
import re
import sys
import threading
import time
from typing import Any, Callable, Optional, TextIO

from .vault import ALL_PARTS, PARTS, Vault, VaultError, site_allows, site_of
from .vault_native import Field

PROTOCOL_VERSIONS = ("2025-06-18", "2025-03-26", "2024-11-05")
SERVER_NAME = "eve_vault"
SERVER_INFO = {"name": SERVER_NAME, "version": "1.0.0"}
TOOL_NAMES = ("vault_list", "vault_fill")
FIELD_WAITS = (0.0, 0.3, 0.5, 0.7, 1.0)     # seconds before each look for the page's fields
COUNT_WAIT = 0.3                            # a frame's field reports its new length a moment after the set
# What a field is called, per part, to tell a page's fields apart (vault_native.Field.words).
PART_WORDS = {"number": re.compile(r"card.?num|card number|cc.?num|\bpan\b", re.I),
              "cvc": re.compile(r"cvc|cvv|csc|security code|card code", re.I),
              "exp": re.compile(r"expir|exp.?date|mm.?/.?yy", re.I),
              "name": re.compile(r"name on card|card.?holder", re.I),
              "code": re.compile(r"code|otp|one.?time|verification|2fa|passcode", re.I),
              "username": re.compile(r"user|e-?mail|login|account|phone", re.I)}
INSTRUCTIONS = ("eve's vault holds the owner's saved sign-ins, cards and 2FA secrets. vault_list shows them by id. "
                "To enter one, call vault_fill with the id, the part and the page's URL. You never see a value, "
                "and never type one yourself.")
TOOLS = [
    {"name": "vault_list",
     "description": "The owner's saved sign-ins, cards and 2FA secrets: one line each with its id, kind, label "
                    "and the site it is for. Never a value.",
     "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False}},
    {"name": "vault_fill",
     "description": "Put one part of a saved item into a field of the page, without you seeing it. Give the "
                    "page's URL, and the field's label when the page has several fields of that kind. Parts: a "
                    "login has username and password; a card has number, exp (MM/YY), exp_month, exp_year, cvc and "
                    "name; a 2FA item has code (generated now). A login or 2FA item fills only on its own site; a "
                    "password fills only into a password field.",
     "inputSchema": {"type": "object", "additionalProperties": False, "required": ["id", "part"],
                     "properties": {"id": {"type": "string", "description": "The item's id from vault_list"},
                                    "part": {"type": "string", "enum": list(ALL_PARTS)},
                                    "page": {"type": "string", "description": "The URL of the page the field is on"},
                                    "field": {"type": "string", "description": "The field's label, when needed"}}}},
]


class Server:
    def __init__(self, vault: Optional[Vault] = None, native: Any = None, out: Optional[TextIO] = None) -> None:
        self.vault = vault if vault is not None else Vault()
        self._native = native
        self._out = out if out is not None else sys.stdout
        self._lock = threading.Lock()

    @property
    def native(self) -> Any:
        if self._native is None:
            from .vault_native import NativeMac
            self._native = NativeMac()
        return self._native

    # -- the tools --
    def vault_list(self) -> tuple[bool, str]:
        lines = self.vault.lines()
        return True, "\n".join(lines) if lines else "The vault is empty."

    def vault_fill(self, handle: Any, part: Any, page: Any = "", label: Any = "") -> tuple[bool, str]:
        handle, part = str(handle or "").strip(), str(part or "").strip()
        page, label = str(page or "").strip(), " ".join(str(label or "").split())
        item = self.vault.get(handle)
        if item is None:
            return False, f"No vault item {handle[:20]!r}. vault_list shows the ids."
        if part not in PARTS[item.kind]:
            return False, f"A {item.kind} has the parts {', '.join(PARTS[item.kind])}."
        if not item.origin and not page:
            return False, f"Give the URL of the page the {part} goes into."
        native = self.native
        if hasattr(native, "trusted") and not native.trusted():
            return False, "eve has no Accessibility permission here, so it cannot reach the page."

        def want(url: str) -> bool:
            return site_allows(item.origin, url) if item.origin else _same_page(url, page)

        fields: list[Field] = []
        for wait in FIELD_WAITS:
            # Chrome builds a page's Accessibility tree after a client first asks (live, 2026-10-12: a page
            # just loaded showed no fields on the first walk and all of them a second later)
            time.sleep(wait)
            fields = native.page_fields(want, frames=item.kind == "card")
            if fields:
                break
        where_item = site_of(item.origin) if item.origin else page
        if not fields:
            return False, (f"No page on {where_item} with a field is in front in its window, so the vault cannot see "
                           "it (it reads each window's active tab). Show that tab, then call vault_fill again.")
        top = lambda f: (f.pid, f.page or f.url)  # noqa: E731 — a frame's fields belong to its page
        pages = {top(f) for f in fields}
        if len(pages) > 1 and page:
            named = [f for f in fields if _same_page(f.page or f.url, page)]
            fields = named or fields
            pages = {top(f) for f in fields}
        if len(pages) > 1:
            hot = {top(f) for f in fields if f.focused}
            if len(hot) != 1:
                return False, f"{len(pages)} pages on {where_item} are open. Give the page's exact URL."
            fields = [f for f in fields if top(f) in hot]
        field, why = pick_field(fields, part, label)
        if field is None:
            return False, why
        try:
            item, values = self.vault.fill_values(handle, field.url, part)
        except VaultError as e:
            return False, str(e)
        value = values.pop(part)
        ok = native.set_value(field, value)
        want_n = len(value)
        n = native.char_count(field)
        if n is not None and n != want_n:
            time.sleep(COUNT_WAIT)              # live 2026-10-12: a payment frame's field said 0 right after the set
            n = native.char_count(field)
        del value
        where = site_of(field.url) if "://" in field.url else field.url
        if not ok:
            return False, f"The {part} field on {where} would not take a value. Nothing was filled."
        if n is not None and n != want_n:
            return True, (f"Put the {part} of {item.label!r} into the field on {where}, but it shows {n} characters "
                          f"where {want_n} were set (the site may format it). Check the page.")
        kind = "password" if field.secure else f"{field.words[:40]!r}" if field.words else "text"
        return True, f"Filled the {part} of {item.label!r} into the {kind} field on {where}."

    def call(self, name: Any, args: Any) -> dict[str, Any]:
        args = args if isinstance(args, dict) else {}
        try:
            if name == "vault_list":
                ok, text = self.vault_list()
            elif name == "vault_fill":
                ok, text = self.vault_fill(args.get("id"), args.get("part"), args.get("page"), args.get("field"))
            else:
                ok, text = False, f"No tool {str(name)[:40]!r}."
        except Exception as e:  # noqa: BLE001 — Codex gets a sentence, never a dead server or a traceback
            ok, text = False, f"The vault could not do that ({type(e).__name__})."
        return {"content": [{"type": "text", "text": text}], "isError": not ok}

    # -- the wire --
    def write(self, msg: dict[str, Any]) -> None:
        with self._lock:
            self._out.write(json.dumps(msg, ensure_ascii=False) + "\n")
            self._out.flush()

    def handle(self, msg: Any) -> None:
        if not isinstance(msg, dict) or msg.get("jsonrpc") != "2.0" or not isinstance(msg.get("method"), str):
            if isinstance(msg, dict) and "id" in msg and "method" not in msg:
                return
            self.write({"jsonrpc": "2.0", "id": msg.get("id") if isinstance(msg, dict) else None,
                        "error": {"code": -32600, "message": "invalid request"}})
            return
        if "id" not in msg:
            return                                       # a notification: nothing to answer
        rid, method = msg["id"], msg["method"]
        params = msg.get("params") if isinstance(msg.get("params"), dict) else {}
        if method == "initialize":
            asked = params.get("protocolVersion")
            result: dict[str, Any] = {"protocolVersion": asked if asked in PROTOCOL_VERSIONS else PROTOCOL_VERSIONS[0],
                                      "capabilities": {"tools": {"listChanged": False}},
                                      "serverInfo": SERVER_INFO, "instructions": INSTRUCTIONS}
        elif method == "ping":
            result = {}
        elif method == "tools/list":
            result = {"tools": TOOLS}
        elif method == "tools/call":
            result = self.call(params.get("name"), params.get("arguments"))
        else:
            self.write({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": f"method not found: {method}"}})
            return
        self.write({"jsonrpc": "2.0", "id": rid, "result": result})

    def handle_line(self, raw: str) -> None:
        raw = raw.strip()
        if not raw:
            return
        try:
            msg = json.loads(raw)
        except ValueError:
            self.write({"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "parse error"}})
            return
        for one in msg if isinstance(msg, list) else [msg]:
            self.handle(one)

    def serve(self, stdin: Optional[TextIO] = None) -> int:
        for raw in stdin if stdin is not None else sys.stdin:
            self.handle_line(raw)
        return 0


def pick_field(fields: list[Field], part: str, label: str = "") -> tuple[Optional[Field], str]:
    """The one field of one page that ``part`` goes into, or (None, why). See the module docstring."""
    fields = sorted(fields, key=lambda f: f.order)
    if label:
        named = [f for f in fields if label.casefold() in f.words.casefold()]
        if not named:
            return None, f"No field labelled {label!r} on the page."
        fields = named
    secure = [f for f in fields if f.secure]
    if part == "password":
        if not secure:
            return None, "A password goes only into a password field, and the page has none" + (" with that label." if label else ".")
        return next((f for f in secure if f.focused), secure[0]), ""
    plain = [f for f in fields if not f.secure]
    if not plain:
        return None, f"The page has no field for the {part}."
    focused = [f for f in plain if f.focused]
    if len(focused) == 1:
        return focused[0], ""
    words = PART_WORDS.get(part)
    named = [f for f in plain if words is not None and words.search(f.words)]
    if len(named) == 1:
        return named[0], ""
    if part == "username" and secure:
        before = [f for f in plain if f.order < secure[0].order]
        if before:
            return before[-1], ""
    if len(plain) == 1:
        return plain[0], ""
    labels = ", ".join(repr(f.words[:30] or "unlabelled") for f in (named or plain)[:6])
    return None, f"The page has several fields the {part} could go into ({labels}). Give the field's label."


def _same_page(url: str, page: str) -> bool:
    """The field's page is the one Codex named: the same origin and path (a fragment or query aside)."""
    from urllib.parse import urlsplit
    a, b = urlsplit(url), urlsplit(page)
    return (a.scheme, a.netloc.lower(), a.path.rstrip("/")) == (b.scheme, b.netloc.lower(), b.path.rstrip("/"))


def server_config(python: str = "") -> dict[str, Any]:
    """The ``mcp_servers`` entry a Codex thread is started with (codex_computer.py). The vault's own two
    settings travel with it, because Codex starts a server with a trimmed environment."""
    import os
    env = {k: os.environ[k] for k in ("EVE_VAULT_INDEX", "EVE_VAULT_KEYCHAIN") if os.environ.get(k)}
    return {"command": python or sys.executable, "args": ["-I", "-m", "eve_bridge.vault_mcp"], **({"env": env} if env else {})}


def main(argv: Optional[list[str]] = None, *, make: Callable[[], Server] = Server) -> int:
    return make().serve()


if __name__ == "__main__":
    raise SystemExit(main())
