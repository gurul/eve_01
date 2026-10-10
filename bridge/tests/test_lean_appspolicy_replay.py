"""Replays of verification/Eve/AppsPolicy.lean's ``current_violates`` against the real apps_mcp.py and
composio_tools.py.

1. (P1) ``tool_reads`` lets a tool with no readOnlyHint read by its name alone. A server listing
   ``find_and_replace`` with no annotations has it offered and run under Gmail's read policy: "find" is a reading
   verb and "replace" is in no list. The model's fix: no hint is not a read.
2. (P3) ``_allowed_toolkits`` compares the raw alias with "personal", while ``decide`` strips and lowercases the
   account. The owner's personal Gmail aliased "Personal" joins the session.
3. (P3) Gmail named in EVE_COMPOSIO_TOOLKITS joins with only the personal account connected, and the session
   is not made multi-account (no explicit account selection).
4. (P3) EVE_COMPOSIO_TOOLKITS=all makes a session with no toolkit limit: GitHub and Drive join.

Each test FAILS on a35b043 and passes once the model's ``fixReads`` / ``fixSession`` is the code. The session tests
read what ``sessions.create`` was given, so they hold for any way of keeping a toolkit out ("enable" without it, or
"disable" with it). Controls at the end pass on both.
"""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from typing import Any

from eve_bridge import apps_mcp, composio_tools
from eve_bridge.apps_mcp import AppsBridge, AppsConfig

# ---- the direct apps -------------------------------------------------------------------------------------

class _Server:
    def __init__(self, tools: list[dict[str, Any]], calls: list[Any]) -> None:
        self.tools, self.calls = tools, calls

    def list_tools(self) -> list[dict[str, Any]]:
        return [dict(t) for t in self.tools]

    def call_tool(self, name: str, args: Any) -> dict[str, Any]:
        self.calls.append(name)
        return {"structuredContent": {"ok": name}}


def _gmail_read_only(tmp_path: Path, tools: list[dict[str, Any]]) -> tuple[AppsBridge, list[Any]]:
    env = {"EVE_APPS_POLICY": "gmail=read"}
    config = AppsConfig(enabled=True, config_dir=tmp_path, policy=apps_mcp.policy(env), environ=env)
    calls: list[Any] = []
    b = AppsBridge(config, rest_factory=lambda app, tok: _Server(tools if app == "gmail" else [], calls),
                   client_factory=lambda s, tok: _Server([], calls),
                   tokens={"google": lambda: "g"})
    b.start()
    return b, calls


def test_an_unhinted_tool_named_like_a_read_does_not_run_under_a_read_policy(tmp_path: Path) -> None:
    b, calls = _gmail_read_only(tmp_path, [{"name": "find_and_replace"}])
    name = "gmail__find_and_replace"
    assert name not in b.names                                  # not offered: Gmail is read only
    assert b.decide(name).action == "refuse"
    if b.decide(name).action == "run":                          # what _direct_tool does on run
        b.execute(name, {})
    assert calls == []


# ---- Composio's session --------------------------------------------------------------------------------

class _Sessions:
    def __init__(self) -> None:
        self.kwargs: list[dict[str, Any]] = []

    def create(self, **kwargs: Any) -> Any:
        self.kwargs.append(kwargs)
        return SimpleNamespace(session_id="trs_1", tools=lambda: [])

    def use(self, session_id: str) -> Any:
        raise RuntimeError("no stored session in these tests")


def _session(tmp_path: Path, toolkits: Any, accounts: list[dict[str, Any]]) -> tuple[composio_tools.ComposioBridge,
                                                                                       dict[str, Any]]:
    sessions = _Sessions()
    client = SimpleNamespace(sessions=sessions, connected_accounts=SimpleNamespace(
        list=lambda **k: SimpleNamespace(items=accounts)))
    config = composio_tools.ComposioConfig(enabled=True, user_id="telegram-7", state_path=tmp_path / "c.json",
                                           toolkits=toolkits)
    b = composio_tools.ComposioBridge(config, client_factory=lambda: client)
    b.start()
    return b, sessions.kwargs[-1]


def _joins(kwargs: dict[str, Any], kit: str) -> bool:
    """Whether a session made with these arguments holds this toolkit."""
    tk = kwargs.get("toolkits")
    if tk is None:
        return True                                            # no toolkit limit
    if isinstance(tk, dict) and "enable" in tk:
        return kit in tk["enable"]
    if isinstance(tk, dict) and "disable" in tk:
        return kit not in tk["disable"]
    return kit in tk


def _multi(kwargs: dict[str, Any]) -> bool:
    ma = kwargs.get("multi_account") or {}
    return bool(ma.get("enable")) and bool(ma.get("require_explicit_selection"))


def _acct(kit: str, alias: Any, i: int = 1) -> dict[str, Any]:
    return {"toolkit": {"slug": kit}, "status": "ACTIVE", "alias": alias, "id": f"ca_{kit}_{i}"}


def test_the_personal_gmail_aliased_with_a_capital_stays_out_of_the_session(tmp_path: Path) -> None:
    # decide() already treats "Personal" as the personal account (test_composio_tools.py); the session must too.
    assert composio_tools.decide("COMPOSIO_MULTI_EXECUTE_TOOL", {"tools": [
        {"tool_slug": "GMAIL_FETCH_EMAILS", "account": "Personal"}]}).action == "refuse"
    for alias in ("Personal", " personal", "PERSONAL"):
        _b, kwargs = _session(tmp_path, ("slack",), [_acct("gmail", alias)])
        assert not _joins(kwargs, "gmail"), alias


def test_a_configured_gmail_with_only_the_personal_account_stays_out(tmp_path: Path) -> None:
    b, kwargs = _session(tmp_path, ("gmail", "slack"), [_acct("gmail", "personal")])
    # If Gmail is in the session at all, every call must name its account (explicit selection).
    assert not _joins(kwargs, "gmail") or _multi(kwargs)
    assert not _joins(kwargs, "gmail")


def test_all_toolkits_never_lets_drive_or_github_into_the_session(tmp_path: Path) -> None:
    _b, kwargs = _session(tmp_path, None, [_acct("github", "work"), _acct("googledrive", "work")])
    assert not _joins(kwargs, "github")
    assert not _joins(kwargs, "googledrive")


# ---- controls: pass before and after the fix -----------------------------------------------------------

def test_control_a_hinted_read_runs_and_a_hinted_trash_does_not(tmp_path: Path) -> None:
    b, calls = _gmail_read_only(tmp_path, [{"name": "search_threads", "annotations": {"readOnlyHint": True}},
                                           {"name": "trash_thread", "annotations": {"readOnlyHint": True}}])
    assert b.decide("gmail__search_threads").action == "run"
    assert b.decide("gmail__trash_thread").action == "refuse"


def test_control_the_work_gmail_joins_and_the_session_names_every_account(tmp_path: Path) -> None:
    _b, kwargs = _session(tmp_path, ("slack",), [_acct("gmail", "personal", 1), _acct("gmail", "work", 2)])
    assert _joins(kwargs, "gmail") and _multi(kwargs)
    assert not _joins(kwargs, "github") and not _joins(kwargs, "googledrive")


# ---- found by the adversarial check: a workbench script that hides its tool call -----------------------------
#
# 5. (P2, P3) ``workbench_slugs`` blocks names from a denylist, but not ``__builtins__`` (nor ``open`` plus
#    ``pd.read_pickle``). A script that reaches ``exec`` through ``__builtins__`` runs a string the AST walk never
#    sees, so ``workbench_slugs`` returns [] ("only computes") and ``decide`` says run: GMAIL_SEND_EMAIL (Gmail is
#    read only, and the owner's own account) or SLACK_SEND_MESSAGE (Slack asks) runs with no question at all.
#    Lean: ``current_violates_workbench``. Fix: ``workbench_slugs`` returns None for any name that starts with "_"
#    (as it already does for such attributes), and for ``open`` and pickle loaders, so the script asks.

_HIDDEN_EXEC = ("b = __builtins__\n"
                "x = b['exec'] if isinstance(b, dict) else b.exec\n"
                "x(\"run_composio_tool('GMAIL_SEND_EMAIL', {'to': 'a@example.com'})\")\n")
_HIDDEN_PICKLE = ("import base64\n"
                  "import pandas as pd\n"
                  "open('p.pkl', 'wb').write(base64.b64decode('gASVAAAAAAAAAAA='))\n"
                  "pd.read_pickle('p.pkl')\n")


def test_a_workbench_script_that_hides_its_tool_call_never_runs_unasked() -> None:
    policy = {"gmail": "read", "googlecalendar": "write", "slack": "ask"}
    for script in (_HIDDEN_EXEC, _HIDDEN_PICKLE):
        decision = composio_tools.decide(composio_tools.WORKBENCH, {"code_to_execute": script}, policy)
        assert decision.action in ("ask", "refuse"), script


def test_control_the_hidden_call_really_runs_in_a_python_namespace() -> None:
    # The escape is real Python, in both forms __builtins__ takes (a dict in exec'd code, the module in __main__).
    import builtins
    for ns in ({}, {"__builtins__": builtins}):
        calls: list[str] = []
        exec(_HIDDEN_EXEC, {**ns, "run_composio_tool": lambda slug, args, seen=calls: seen.append(slug)})  # noqa: S102
        assert calls == ["GMAIL_SEND_EMAIL"]
