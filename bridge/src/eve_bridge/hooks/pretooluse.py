"""PreToolUse hook — blocks Claude Code's tool call until the stick's button decides.

If the daemon is unreachable or BLE is not connected, emits no decision so that
Claude Code's normal approval flow runs.

stdin: { session_id, tool_name, tool_input, tool_use_id, ... }
stdout (on decision): { "hookSpecificOutput": { "hookEventName": "PreToolUse",
                                                 "permissionDecision": "allow"|"deny"|"ask",
                                                 "permissionDecisionReason": "..." } }
"""

from __future__ import annotations

import json
import sys

from ._client import post, read_hook_input

# Hard upper bound for how long this hook blocks. Must be < the `timeout` we
# set in settings.json (330 s) and > the daemon's permission wait
# (telegram.DEFAULT_PERMISSION_TIMEOUT_SECS, 240 s). 5 minutes is plenty
# of human reaction time and still leaves headroom.
BLOCK_TIMEOUT_SECS = 320.0


def _question(tool_input: dict) -> str:
    """An AskUserQuestion call as one line the owner can answer from the phone: each question with its
    options numbered in parentheses, so a reply of "2" picks the second (the relay types it into the
    dialog). Empty when the input is not that shape."""
    questions = tool_input.get("questions")
    if not isinstance(questions, list):
        return ""
    lines = []
    for q in questions:
        if not isinstance(q, dict):
            continue
        text = str(q.get("question") or "").strip()
        labels = [str(o.get("label") or "").strip() for o in (q.get("options") or []) if isinstance(o, dict)]
        labels = [x for x in labels if x]
        if text:
            lines.append(text + (" (" + " / ".join(f"{i}. {x}" for i, x in enumerate(labels, 1)) + ")"
                                 if labels else ""))
    return " | ".join(lines)


def _summarize(tool_input: object) -> str:
    """Short human-readable hint from a tool_input dict."""
    if isinstance(tool_input, dict):
        asked = _question(tool_input)
        if asked:
            return asked
        # Bash: command; Edit/Write: file_path; fallback: first string value.
        for key in ("command", "file_path", "path", "url"):
            v = tool_input.get(key)
            if isinstance(v, str) and v:
                return v
        for v in tool_input.values():
            if isinstance(v, str) and v:
                return v
    if isinstance(tool_input, str):
        return tool_input
    return ""


def main() -> int:
    payload = read_hook_input()
    event = {
        "evt": "pretooluse",
        "session_id": payload.get("session_id", ""),
        "tool_use_id": payload.get("tool_use_id", ""),
        "tool_name": payload.get("tool_name", ""),
        "hint": _summarize(payload.get("tool_input")),
        "cwd": payload.get("cwd", ""),
        # Claude Code's permission mode for this session ("default", "acceptEdits", "plan",
        # "bypassPermissions"): a phone that is asked in bypass mode is asking for nothing.
        "permission_mode": payload.get("permission_mode", ""),
    }
    resp = post(event, timeout=BLOCK_TIMEOUT_SECS)
    if resp is None or not resp.get("ok"):
        # Daemon unreachable or errored — defer to Claude Code's default behavior.
        return 0
    decision = resp.get("decision")
    if decision not in ("allow", "deny", "ask"):
        return 0
    # Always surface a permissionDecisionReason so the model knows the
    # decision came from a human at the Hardware Eve device. Without
    # this, Claude Code treats a bare `permissionDecision: "deny"` as a
    # generic hook block and may attempt alternative phrasings to work
    # around it (e.g. dropping `-rf`, splitting an `rm -rf foo/` into
    # per-file `rm`s). A clear reason short-circuits that.
    reason = _decision_reason(decision)
    out = {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision,
            "permissionDecisionReason": reason,
        }
    }
    sys.stdout.write(json.dumps(out) + "\n")
    return 0


def _decision_reason(decision: str) -> str:
    """Human-readable reason surfaced to the model alongside the decision."""
    if decision == "allow":
        return "User approved on Hardware Eve device."
    if decision == "deny":
        return (
            "User denied on Hardware Eve device. "
            "Do not retry with alternative phrasings; the human has rejected this action."
        )
    # "ask" — daemon explicitly bounced this to Claude Code's normal flow.
    return "Hardware Eve deferred to Claude Code's normal approval flow."


if __name__ == "__main__":
    raise SystemExit(main())
