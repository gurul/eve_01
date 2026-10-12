"""Live check (docs/vault-gates.md V7): a real Codex app-server thread, started exactly as a computer task starts
one (codex_computer.CodexComputerAgent._thread_params), lists eve's vault server with its two tools.

The vault here is a throwaway index with one item and no Keychain entry: listing needs only the index, and
nothing is filled. Prints server names and tool names only. Also says whether this Codex has cua_repl (the
browser a computer task needs), which decides whether the end-to-end check (V11) can run on this Mac.

    bridge/.venv/bin/python bridge/tools/vault_codex_check.py      → VAULT_CODEX_OK
"""

from __future__ import annotations

import asyncio
import json
import os
import sys
import tempfile
from pathlib import Path


async def main() -> int:
    tmp = Path(tempfile.mkdtemp(prefix="eve-vault-check-"))
    index = tmp / "vault.json"
    index.write_text(json.dumps({"items": [{"handle": "v_check000", "kind": "card", "label": "Check", "origin": ""}]}))
    os.environ["EVE_VAULT_INDEX"] = str(index)
    os.environ["EVE_VAULT_KEYCHAIN"] = str(tmp / "none.keychain-db")
    from eve_bridge.codex_computer import VAULT_INSTRUCTIONS, CodexComputerAgent

    async def no_answer(_: str) -> str:
        return "no"

    agent = CodexComputerAgent(on_event=lambda e: None, ask_user=no_answer)
    params = agent._thread_params()
    assert "config" in params and VAULT_INSTRUCTIONS in params["developerInstructions"], "the vault was not added"
    servers: dict[str, list[str]] = {}
    try:
        await agent._launch()
        result = await agent._rpc("thread/start", params)
        thread_id = result["thread"]["id"]
        cursor = None
        for _ in range(20):
            for _wait in range(10):                  # an MCP server starts in the background
                page = await agent._rpc("mcpServerStatus/list", {"threadId": thread_id, "limit": 100, **({"cursor": cursor} if cursor else {})})
                servers.update({s["name"]: sorted((s.get("tools") or {}).keys()) for s in page["data"]})
                if "eve_vault" in servers and servers["eve_vault"]:
                    break
                await asyncio.sleep(1)
            cursor = page.get("nextCursor")
            if not cursor:
                break
    finally:
        await agent._close()
    print("servers:", ", ".join(sorted(servers)))
    print("eve_vault tools:", servers.get("eve_vault"))
    print("cua_repl:", "present" if "js" in servers.get("cua_repl", []) else "absent")
    if servers.get("eve_vault") == ["vault_fill", "vault_list"]:
        print("VAULT_CODEX_OK")
        return 0
    print("eve_vault missing or incomplete")
    return 1


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
