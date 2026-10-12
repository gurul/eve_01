"""Live end-to-end check (docs/vault-gates.md V11): a real Codex computer task signs in to a local page with a
vault item, and the page's server receives exactly the saved password, which Codex never saw.

The vault is a throwaway index and keychain file (never the owner's). The page posts its form to this script's
server, which compares the password and answers "Signed in" or "Wrong password"; the page itself never holds
the value. Codex drives the owner's Chrome through cua_repl as any computer task does, so a tab opens there.
Prints event kinds, the final answer and the server's verdict, and checks the canary appears in none of them.

    bridge/.venv/bin/python bridge/tools/vault_codex_e2e.py      → VAULT_E2E_OK
"""

from __future__ import annotations

import asyncio
import http.server
import os
import subprocess
import sys
import tempfile
import threading
from pathlib import Path
from urllib.parse import parse_qs

CANARY = "e2e-Canary-pw-7731!"
USER = "eve-test-user"
PAGE = """<!doctype html><html><head><title>Local sign in</title></head><body style="font:18px sans-serif;margin:40px">
<h1>Sign in to Local Test</h1><form method="post" action="/signin">
<p><label for="u">Username</label><br><input id="u" name="username" autocomplete="username" style="width:300px;height:32px"></p>
<p><label for="p">Password</label><br><input id="p" name="password" type="password" autocomplete="current-password" style="width:300px;height:32px"></p>
<p><button type="submit" style="font-size:18px">Sign in</button></p></form></body></html>"""
received: dict[str, object] = {}


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_: object) -> None:
        pass

    def _send(self, body: str) -> None:
        data = body.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:
        self._send(PAGE)

    def do_POST(self) -> None:
        form = parse_qs(self.rfile.read(int(self.headers.get("Content-Length") or 0)).decode())
        ok = form.get("password", [""])[0] == CANARY and form.get("username", [""])[0] == USER
        received["posts"] = int(received.get("posts", 0)) + 1
        received["ok"] = bool(received.get("ok")) or ok
        self._send("<h1>Signed in. Welcome back.</h1>" if ok else "<h1>Wrong password.</h1>")


async def main() -> int:
    tmp = Path(tempfile.mkdtemp(prefix="eve-vault-e2e-"))
    keychain = str(tmp / "e2e.keychain-db")
    sec = "/usr/bin/security"
    subprocess.run([sec, "create-keychain", "-p", "e2e", keychain], check=True, capture_output=True)
    subprocess.run([sec, "unlock-keychain", "-p", "e2e", keychain], check=True, capture_output=True)
    subprocess.run([sec, "set-keychain-settings", keychain], check=True, capture_output=True)   # no auto-lock
    os.environ["EVE_VAULT_INDEX"] = str(tmp / "vault.json")
    os.environ["EVE_VAULT_KEYCHAIN"] = keychain
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{srv.server_address[1]}"
    from eve_bridge.codex_computer import CodexComputerAgent
    from eve_bridge.vault import Vault

    item = Vault().add("login", "Local Test", {"username": USER, "password": CANARY}, site=base)
    events: list[tuple[str, str]] = []

    async def no(_: str) -> str:
        return "no"

    agent = CodexComputerAgent(on_event=lambda e: events.append((e.kind, e.text)), ask_user=no, max_secs=300)
    final = ""
    try:
        final = await agent.run(f"In Chrome, open {base}/ and sign in to Local Test with my saved login. "
                                "Then tell me what the page says.")
    finally:
        srv.shutdown()
        subprocess.run([sec, "delete-keychain", keychain], capture_output=True)
    kinds: dict[str, int] = {}
    for kind, _ in events:
        kinds[kind] = kinds.get(kind, 0) + 1
    print("item:", item.line())
    print("events:", kinds)
    print("final:", " ".join(str(final).split())[:300])
    print("server:", {"posts": received.get("posts", 0), "password_matched": bool(received.get("ok"))})
    leaked = CANARY in str(final) or any(CANARY in text for _, text in events)
    print("canary in any event or the answer:", leaked)
    if received.get("ok") and not leaked:
        print("VAULT_E2E_OK")
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
