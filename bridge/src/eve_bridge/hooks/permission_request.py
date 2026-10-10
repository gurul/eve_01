"""PermissionRequest hook: a permission dialog Claude Code is about to show, answered from the phone.

PreToolUse only sees the tools it is registered for (Bash, Read), so a dialog for Edit, Write, WebFetch,
an MCP tool … reached the phone as nothing but "Claude is waiting on you" (owner, 2026-09-23). This hook
fires for every dialog. With the Telegram relay on ("claude on") the daemon asks the owner yes/no in the
chat; any other state, no answer, or an unreachable daemon returns nothing, and the dialog on the Mac
runs exactly as before.

stdin: { session_id, tool_name, tool_input, tool_use_id, cwd, permission_mode, ... }
stdout (on decision): { "hookSpecificOutput": { "hookEventName": "PermissionRequest",
                                                 "decision": {"behavior": "allow"} } }
    or {"behavior": "deny", "message": "..."} — the shape Claude Code 2.1.280 validates.
"""

from __future__ import annotations

import json
import sys

from ._client import post, read_hook_input
from .pretooluse import BLOCK_TIMEOUT_SECS, _summarize

DENY_MESSAGE = ("The owner denied this from their phone. Do not retry it with alternative phrasings; "
                "the human has rejected this action.")


def main() -> int:
    payload = read_hook_input()
    resp = post({
        "evt": "permissionrequest",
        "session_id": payload.get("session_id", ""),
        "tool_use_id": payload.get("tool_use_id", ""),
        "tool_name": payload.get("tool_name", ""),
        "hint": _summarize(payload.get("tool_input")),
        "cwd": payload.get("cwd", ""),
        "permission_mode": payload.get("permission_mode", ""),
    }, timeout=BLOCK_TIMEOUT_SECS)
    if resp is None or not resp.get("ok"):
        return 0
    decision = resp.get("decision")
    if decision == "allow":
        behavior: dict = {"behavior": "allow"}
    elif decision == "deny":
        behavior = {"behavior": "deny", "message": DENY_MESSAGE}
    else:
        return 0
    sys.stdout.write(json.dumps({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
                                                        "decision": behavior}}) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
