"""Composio bridge: the switch, the owner's session, the ask-first gate, and execute that never raises.

No network: a fake client stands in for the SDK. The SDK is never imported here.
"""

from __future__ import annotations

import json
import logging
import stat
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from eve_bridge.composio_tools import (
    COMPOSIO_DEFAULT,
    PRIMARY_ALIAS,
    TOOLKITS,
    WEB_TOOLKITS,
    ComposioBridge,
    configured,
    consequential_slugs,
    describe_for_confirmation,
    is_read_only,
    web_toolkit_slugs,
)

META_TOOLS = [
    {"type": "function", "name": "COMPOSIO_SEARCH_TOOLS", "strict": True,
     "parameters": {"type": "object", "properties": {}}},
    {"type": "function", "name": "COMPOSIO_MULTI_EXECUTE_TOOL", "strict": True,
     "parameters": {"type": "object", "properties": {}}},
    {"type": "web_search"},  # not a function tool: filtered out
    {"name": "no_type"},  # no type: filtered out
]


class FakeResult:
    """Pydantic-ish, like SessionExecuteResponse."""

    def __init__(self, data: dict[str, Any], error: str | None = None) -> None:
        self.data, self.error, self.log_id = data, error, "log_123"

    def model_dump(self) -> dict[str, Any]:
        return {"data": self.data, "error": self.error, "log_id": self.log_id}


class FakeSession:
    def __init__(self, session_id: str, execute: Any = None) -> None:
        self.session_id = session_id
        self._execute = execute
        self.executed: list[tuple[str, dict[str, Any]]] = []

    def tools(self) -> list[dict[str, Any]]:
        return list(META_TOOLS)

    def execute(self, slug: str, *, arguments: dict[str, Any] | None = None, account: str | None = None) -> Any:
        self.executed.append((slug, dict(arguments or {})))
        if isinstance(self._execute, Exception):
            raise self._execute
        return self._execute if self._execute is not None else FakeResult({"items": [1, 2]})


class FakeSessions:
    def __init__(self, use_raises: bool = False, execute: Any = None) -> None:
        self.created: list[str] = []
        self.toolkits: list[Any] = []
        self.used: list[str] = []
        self.multi: list[Any] = []
        self.use_raises = use_raises
        self._execute = execute
        self.n = 0

    def create(self, *, user_id: str, toolkits: Any = None, multi_account: Any = None) -> FakeSession:
        self.created.append(user_id)
        self.toolkits.append(toolkits)
        self.multi.append(multi_account)
        self.n += 1
        return FakeSession(f"trs_new_{self.n}", self._execute)

    def use(self, session_id: str) -> FakeSession:
        self.used.append(session_id)
        if self.use_raises:
            raise RuntimeError("no such session")
        return FakeSession(session_id, self._execute)


class FakeAccounts:
    def __init__(self, connected: tuple[tuple[str, ...], ...] = (), raises: bool = False) -> None:
        self.connected, self.raises = connected, raises

    def list(self, *, user_ids: list[str]) -> Any:
        if self.raises:
            raise RuntimeError("down")
        # (slug, status) or (slug, status, alias): an alias of "" means the connection has none (named by id)
        return SimpleNamespace(items=[{"toolkit": {"slug": c[0]}, "status": c[1], "id": f"ca_{c[0]}_{i}",
                                       "alias": c[2] if len(c) > 2 else None} for i, c in enumerate(self.connected)])


class FakeClient:
    def __init__(self, connected: tuple[tuple[str, ...], ...] = (), accounts_raise: bool = False, **kw: Any) -> None:
        self.sessions = FakeSessions(**kw)
        self.connected_accounts = FakeAccounts(connected, accounts_raise)


def _cfg(tmp_path: Path, owners: frozenset[int] = frozenset({7})) -> Any:
    env = {"EVE_COMPOSIO": "1", "COMPOSIO_API_KEY": "ak_test", "EVE_COMPOSIO_STATE": str(tmp_path / "composio.json")}
    return configured(env, owners)


def test_it_ships_off_and_needs_the_switch_and_the_key(tmp_path: Path, caplog: Any) -> None:
    owners = frozenset({42, 7})
    assert COMPOSIO_DEFAULT is False
    assert configured({}, owners).enabled is False
    assert configured({"COMPOSIO_API_KEY": "ak_x"}, owners).enabled is False  # key without the switch
    with caplog.at_level(logging.WARNING, logger="eve_bridge.composio_tools"):
        assert configured({"EVE_COMPOSIO": "1"}, owners).enabled is False  # switch without the key
        assert configured({"EVE_COMPOSIO": "yes", "COMPOSIO_API_KEY": "ak_x"}, frozenset()).enabled is False
    assert "COMPOSIO_API_KEY" in caplog.text and "owner" in caplog.text
    assert "ak_x" not in caplog.text
    cfg = configured({"EVE_COMPOSIO": "on", "COMPOSIO_API_KEY": "ak_secret_value",
                      "EVE_COMPOSIO_STATE": str(tmp_path / "s.json"), "EVE_COMPOSIO_TIMEOUT_SECS": "12.5"},
                     owners)
    assert cfg.enabled is True
    assert cfg.user_id == "telegram-7-42"  # sorted, so the same owners always name the same session
    assert cfg.state_path == tmp_path / "s.json"
    assert cfg.timeout_secs == 12.5
    assert "ak_secret_value" not in repr(cfg)
    assert configured({"EVE_COMPOSIO": "0", "COMPOSIO_API_KEY": "ak_x"}, owners).enabled is False
    # Defaults hold when the overrides are absent or junk.
    d = configured({"EVE_COMPOSIO_TIMEOUT_SECS": "nope"}, owners)
    assert d.timeout_secs == 60.0 and d.state_path == Path("~/.config/eve-bridge/composio.json").expanduser()


def test_the_session_is_the_owners_and_is_reused(tmp_path: Path) -> None:
    cfg = _cfg(tmp_path)
    first = FakeClient()
    b1 = ComposioBridge(cfg, client_factory=lambda: first)
    b1.start()
    assert first.sessions.created == ["telegram-7"] and first.sessions.used == []
    assert b1.session_id == "trs_new_1"
    state = json.loads(cfg.state_path.read_text())
    assert state == {"user_id": "telegram-7", "session_id": "trs_new_1", "toolkits": sorted(TOOLKITS),
                     "multi_account": False}
    assert first.sessions.toolkits == [{"enable": sorted(TOOLKITS)}]          # an allow-list: no web toolkits
    assert not set(TOOLKITS) & WEB_TOOLKITS
    assert stat.S_IMODE(cfg.state_path.stat().st_mode) == 0o600
    # Only function tools with a name survive; names is derived from them.
    assert all(t["type"] == "function" and t["name"] for t in b1.tools())
    assert b1.names == frozenset({"COMPOSIO_SEARCH_TOOLS", "COMPOSIO_MULTI_EXECUTE_TOOL"})

    # Restart: the second bridge resumes the stored id instead of creating.
    second = FakeClient()
    b2 = ComposioBridge(cfg, client_factory=lambda: second)
    b2.start()
    assert second.sessions.used == ["trs_new_1"] and second.sessions.created == []
    assert b2.session_id == "trs_new_1"

    # A session stored before the allow-list (no "toolkits" in the state) has the web toolkits:
    # it is not resumed, and a fresh one is made with the list (2026-09-30).
    cfg.state_path.write_text(json.dumps({"user_id": "telegram-7", "session_id": "trs_old"}))
    older = FakeClient()
    ComposioBridge(cfg, client_factory=lambda: older).start()
    assert older.sessions.used == [] and older.sessions.created == ["telegram-7"]
    assert json.loads(cfg.state_path.read_text())["toolkits"] == sorted(TOOLKITS)

    # A stored id for someone else is not ours: create, and overwrite the state.
    cfg.state_path.write_text(json.dumps({"user_id": "telegram-999", "session_id": "trs_theirs"}))
    third = FakeClient()
    b3 = ComposioBridge(cfg, client_factory=lambda: third)
    b3.start()
    assert third.sessions.used == [] and third.sessions.created == ["telegram-7"]
    assert json.loads(cfg.state_path.read_text())["session_id"] == "trs_new_1"

    # A stored id the server no longer knows: use raises, create takes over, the state heals.
    fourth = FakeClient(use_raises=True)
    b4 = ComposioBridge(cfg, client_factory=lambda: fourth)
    b4.start()
    assert fourth.sessions.used == ["trs_new_1"] and fourth.sessions.created == ["telegram-7"]
    assert b4.session_id == "trs_new_1" and json.loads(cfg.state_path.read_text())["session_id"] == "trs_new_1"


def test_read_only_slugs_run_and_consequential_slugs_ask_first() -> None:
    assert is_read_only("GMAIL_FETCH_EMAILS") and is_read_only("GITHUB_LIST_REPOSITORY_ISSUES")
    assert is_read_only("googlecalendar_find_event") and is_read_only("NOTION_SEARCH")
    assert is_read_only("GOOGLECALENDAR_EVENTS_LIST_ALL_CALENDARS")      # the verb is not always the first word
    assert not is_read_only("GMAIL_SEND_EMAIL") and not is_read_only("GITHUB_CREATE_ISSUE")
    assert not is_read_only("GETTY_UPLOAD")  # a verb-like toolkit name does not make the call read-only
    assert not is_read_only("GMAIL_GET_AND_DELETE_MESSAGE")               # a read word beside a write word writes
    assert not is_read_only("SLACK_DELETE_MESSAGE") and not is_read_only("") and not is_read_only("GMAIL")


def test_gmail_is_read_only_and_the_calendar_may_write() -> None:
    from eve_bridge.composio_tools import Decision, decide, toolkit_policy

    def multi(*slugs: str) -> dict:
        # Gmail and the calendar run through Composio only for the owner's other account, named (2026-10-05)
        return {"tools": [{"tool_slug": s, "arguments": {}, "account": "work"} for s in slugs]}

    assert decide("COMPOSIO_MULTI_EXECUTE_TOOL", multi("GMAIL_FETCH_EMAILS", "GOOGLECALENDAR_FIND_EVENT")).action == "run"
    d = decide("COMPOSIO_MULTI_EXECUTE_TOOL", multi("GMAIL_SEND_EMAIL"))
    assert d.action == "refuse" and d.slugs == ["GMAIL_SEND_EMAIL"] and "gmail is read only here" in d.why
    d = decide("COMPOSIO_MULTI_EXECUTE_TOOL", multi("GOOGLECALENDAR_CREATE_EVENT", "GOOGLECALENDAR_FIND_EVENT"))
    assert d.action == "run" and d.why == "allowed to write: GOOGLECALENDAR_CREATE_EVENT"
    d = decide("COMPOSIO_MULTI_EXECUTE_TOOL", multi("SLACK_SEND_MESSAGE"))
    assert d == Decision("ask", ["SLACK_SEND_MESSAGE"])
    # mixed: refuse beats ask beats write
    d = decide("COMPOSIO_MULTI_EXECUTE_TOOL", multi("GOOGLECALENDAR_CREATE_EVENT", "SLACK_SEND_MESSAGE", "GMAIL_SEND_EMAIL"))
    assert d.action == "refuse" and d.slugs == ["GMAIL_SEND_EMAIL"]
    assert decide("COMPOSIO_MULTI_EXECUTE_TOOL", multi("GOOGLECALENDAR_CREATE_EVENT", "SLACK_SEND_MESSAGE")).action == "ask"
    assert decide("COMPOSIO_REMOTE_BASH_TOOL", {"command": "ls"}).action == "ask"
    assert decide("COMPOSIO_SEARCH_TOOLS", {}).action == "run"
    # the owner's env overrides and extends
    pol = toolkit_policy({"EVE_COMPOSIO_POLICY": "gmail=ask, slack=write,notion=bogus,=read"})
    assert pol == {"gmail": "ask", "googlecalendar": "auto", "googledrive": "auto", "slack": "write"}
    assert decide("COMPOSIO_MULTI_EXECUTE_TOOL", multi("GMAIL_SEND_EMAIL"), pol).action == "ask"
    assert toolkit_policy({}) == {"gmail": "read", "googlecalendar": "auto", "googledrive": "auto"}
    assert decide("COMPOSIO_MULTI_EXECUTE_TOOL", multi("GOOGLEDRIVE_LIST_FILES")).action == "run"
    assert decide("COMPOSIO_MULTI_EXECUTE_TOOL", multi("GOOGLEDRIVE_UPLOAD_FILE")).action == "run"

    mixed = {"tools": [
        {"tool_slug": "GMAIL_FETCH_EMAILS", "arguments": {"max_results": 5}},
        {"tool_slug": "GMAIL_SEND_EMAIL", "arguments": {"to": "x@y.com", "subject": "Hello"}},
        {"tool_slug": "GITHUB_LIST_REPOSITORY_ISSUES", "arguments": {}},
        {"tool_slug": "SLACK_SEND_MESSAGE", "arguments": {"channel": "#general", "text": "hi"}},
    ], "thought": "do it", "sync_response_to_workbench": False, "current_step": "1"}
    assert consequential_slugs("COMPOSIO_MULTI_EXECUTE_TOOL", mixed) == ["GMAIL_SEND_EMAIL", "SLACK_SEND_MESSAGE"]
    reads = {"tools": [{"tool_slug": "GMAIL_FETCH_EMAILS", "arguments": {}},
                       {"tool_slug": "GOOGLECALENDAR_FIND_EVENT", "arguments": {}}]}
    assert consequential_slugs("COMPOSIO_MULTI_EXECUTE_TOOL", reads) == []
    assert consequential_slugs("COMPOSIO_MULTI_EXECUTE_TOOL", {}) == []
    assert consequential_slugs("COMPOSIO_MULTI_EXECUTE_TOOL", {"tools": "junk"}) == []
    assert consequential_slugs("COMPOSIO_SEARCH_TOOLS", {"queries": [{"use_case": "send mail"}]}) == []
    assert consequential_slugs("COMPOSIO_GET_TOOL_SCHEMAS", {}) == []
    assert consequential_slugs("COMPOSIO_MANAGE_CONNECTIONS", {}) == []
    assert consequential_slugs("COMPOSIO_REMOTE_BASH_TOOL", {"command": "ls"}) == ["COMPOSIO_REMOTE_BASH_TOOL"]
    assert consequential_slugs("COMPOSIO_REMOTE_WORKBENCH", {}) == ["COMPOSIO_REMOTE_WORKBENCH"]

    line = describe_for_confirmation("COMPOSIO_MULTI_EXECUTE_TOOL", mixed)
    assert line == ("Run GMAIL_SEND_EMAIL with to: x@y.com, subject: Hello; "
                    "and SLACK_SEND_MESSAGE with channel: #general, text: hi?")
    assert "GMAIL_FETCH_EMAILS" not in line  # the reads are not what the owner is asked about
    assert describe_for_confirmation("COMPOSIO_REMOTE_BASH_TOOL", {"command": "ls -la"}) == (
        "Run COMPOSIO_REMOTE_BASH_TOOL with command: ls -la?")
    assert describe_for_confirmation("COMPOSIO_REMOTE_WORKBENCH", {}) == "Run COMPOSIO_REMOTE_WORKBENCH?"

    # Clipping: each value to 60 chars, the line to 300; newlines flattened; nested values serialised.
    long = {"tools": [{"tool_slug": "GMAIL_SEND_EMAIL", "arguments": {
        "to": "x@y.com", "body": "line one\nline two " + "z" * 500, "cc": ["a@b.c", "d@e.f"], "n": 3, "flag": True}}]}
    line = describe_for_confirmation("COMPOSIO_MULTI_EXECUTE_TOOL", long)
    assert line.startswith("Run GMAIL_SEND_EMAIL with to: x@y.com, body: line one line two zzz")
    assert line.endswith("?") and len(line) <= 300 and "\n" not in line
    body = line.split("body: ", 1)[1].split(", cc:", 1)[0]
    assert len(body) <= 60
    assert 'cc: ["a@b.c", "d@e.f"]' in line and "n: 3" in line and "flag: true" in line
    many = {"tools": [{"tool_slug": f"APP_CREATE_THING_{i}", "arguments": {"v": "w" * 60}} for i in range(12)]}
    line = describe_for_confirmation("COMPOSIO_MULTI_EXECUTE_TOOL", many)
    assert len(line) <= 300 and line.endswith("?")
    for text in (line, describe_for_confirmation("COMPOSIO_MULTI_EXECUTE_TOOL", mixed)):
        assert not any(ord(ch) > 0x2FFF for ch in text)  # no emoji (the only high code point allowed is the ellipsis)
        assert " - " not in text and " — " not in text


def test_execute_never_raises(tmp_path: Path, caplog: Any) -> None:
    cfg = _cfg(tmp_path)
    # Not started: a reason, not an exception.
    idle = ComposioBridge(cfg, client_factory=FakeClient)
    assert idle.execute("COMPOSIO_SEARCH_TOOLS", {}) == {"ok": False, "reason": "composio is not started"}
    assert idle.tools() == [] and idle.names == frozenset()

    # A good result is a plain dict the model can read, with ok and the log id.
    ok = ComposioBridge(cfg, client_factory=FakeClient)
    ok.start()
    out = ok.execute("COMPOSIO_SEARCH_TOOLS", {"queries": [{"use_case": "mail"}]})
    assert out == {"data": {"items": [1, 2]}, "error": None, "log_id": "log_123", "ok": True}
    json.dumps(out)
    assert ok._session.executed == [("COMPOSIO_SEARCH_TOOLS", {"queries": [{"use_case": "mail"}]})]

    # The server said error: ok is False, the payload is kept for the model to explain.
    said_no = ComposioBridge(cfg, client_factory=lambda: FakeClient(execute=FakeResult({}, error="not connected")))
    said_no.start()
    out = said_no.execute("COMPOSIO_MULTI_EXECUTE_TOOL", {"tools": []})
    assert out["ok"] is False and out["error"] == "not connected" and out["log_id"] == "log_123"

    # The SDK raised: the type and a short message, and the log names the type but never the arguments.
    boom = ComposioBridge(cfg, client_factory=lambda: FakeClient(execute=TimeoutError("upstream took " + "x" * 400)))
    boom.start()
    with caplog.at_level(logging.WARNING, logger="eve_bridge.composio_tools"):
        out = boom.execute("COMPOSIO_MULTI_EXECUTE_TOOL", {"tools": [{"tool_slug": "GMAIL_SEND_EMAIL",
                                                                       "arguments": {"body": "SECRET BODY"}}]})
    assert out["ok"] is False and out["reason"].startswith("TimeoutError: upstream took x")
    assert len(out["reason"]) <= len("TimeoutError: ") + 200
    json.dumps(out)
    assert "TimeoutError" in caplog.text and "SECRET BODY" not in caplog.text and "GMAIL_SEND_EMAIL" not in caplog.text

    # A result that is not pydantic and not a mapping is still a dict; unserialisable bits become strings.
    odd = ComposioBridge(cfg, client_factory=lambda: FakeClient(execute={"data": {"when": Path("/x")}, "log_id": "l"}))
    odd.start()
    out = odd.execute("COMPOSIO_GET_TOOL_SCHEMAS", {})
    assert out == {"data": {"when": "/x"}, "log_id": "l", "ok": True}


@pytest.mark.parametrize("slug,read", [
    ("GMAIL_GET_LABEL", True),                  # the unread count lives on the INBOX label: a read (live, 2026-09-23)
    ("GMAIL_LIST_LABELS", True),
    ("GMAIL_FETCH_EMAILS", True),
    ("GMAIL_ADD_LABEL_TO_EMAIL", False),
    ("GMAIL_CREATE_LABEL", False),
    ("GMAIL_MODIFY_THREAD_LABELS", False),
    ("GMAIL_LABEL_EMAIL", False),               # LABEL in verb position is still a write
    ("GMAIL_SEND_EMAIL", False),
    ("GMAIL_GET_DRAFT", True),                  # the same trap for the other noun-verbs
    ("GMAIL_LIST_DRAFTS", True),
    ("GMAIL_CREATE_EMAIL_DRAFT", False),
    ("GMAIL_SEND_DRAFT", False),
    ("GITHUB_GET_A_WORKFLOW_RUN", True),
    ("GITHUB_RUN_WORKFLOW", False),
    ("SHOPIFY_GET_ORDER", True),
    ("AMAZON_ORDER_ITEM", False),
    ("GOOGLECALENDAR_GET_SCHEDULE", True),
    ("GOOGLECALENDAR_SCHEDULE_MEETING", False),
    ("GMAIL_FETCH_AND_DELETE_DRAFT", False),    # a real write verb anywhere still blocks
])
def test_label_is_a_write_only_as_the_verb(slug: str, read: bool) -> None:
    from eve_bridge.composio_tools import is_read_only

    assert is_read_only(slug) is read


def test_web_toolkit_slugs_catch_web_search_and_scrapers_by_prefix() -> None:
    web = {"tools": [{"tool_slug": "EXA_SEARCH", "arguments": {}}, {"tool_slug": "GMAIL_FETCH_EMAILS", "arguments": {}},
                     {"tool_slug": "BROWSER_TOOL_CREATE_TASK", "arguments": {}},
                     {"tool_slug": "SCRAPEGRAPH_AI_SMART_SCRAPE", "arguments": {}}]}
    assert web_toolkit_slugs("COMPOSIO_MULTI_EXECUTE_TOOL", web) == [
        "EXA_SEARCH", "BROWSER_TOOL_CREATE_TASK", "SCRAPEGRAPH_AI_SMART_SCRAPE"]
    # the owner's own apps are never caught, nor a toolkit that only starts with the same letters
    apps = {"tools": [{"tool_slug": "GOOGLECALENDAR_CREATE_EVENT", "arguments": {}},
                      {"tool_slug": "EXAMPLEAPP_GET_THING", "arguments": {}}]}
    assert web_toolkit_slugs("COMPOSIO_MULTI_EXECUTE_TOOL", apps) == []
    assert web_toolkit_slugs("COMPOSIO_SEARCH_TOOLS", web) == []


def test_the_allow_list_adds_what_is_connected_and_never_a_web_toolkit(tmp_path: Path) -> None:
    cfg = _cfg(tmp_path)
    client = FakeClient(connected=(("canvas", "ACTIVE"), ("gmail", "EXPIRED"), ("exa", "ACTIVE"), ("figma", "INITIATED")))
    ComposioBridge(cfg, client_factory=lambda: client).start()
    enabled = client.sessions.toolkits[0]["enable"]
    assert "canvas" in enabled                                  # an app connected outside the list still works
    assert "exa" not in enabled and "figma" not in enabled      # a web toolkit never; a half-made connection not
    assert enabled == sorted(set(TOOLKITS) | {"canvas"})
    # connecting an app changes the list: the stored session is not resumed, a fresh one is made
    later = FakeClient(connected=(("canvas", "ACTIVE"), ("figma", "ACTIVE")))
    ComposioBridge(cfg, client_factory=lambda: later).start()
    assert later.sessions.used == [] and "figma" in later.sessions.toolkits[0]["enable"]
    # the same list again: resumed
    again = FakeClient(connected=(("canvas", "ACTIVE"), ("figma", "ACTIVE")))
    ComposioBridge(cfg, client_factory=lambda: again).start()
    assert again.sessions.created == [] and len(again.sessions.used) == 1


def test_the_setting_replaces_the_list_all_lifts_it_and_a_failed_lookup_keeps_it(tmp_path: Path) -> None:
    env = {"EVE_COMPOSIO": "1", "COMPOSIO_API_KEY": "ak_test", "EVE_COMPOSIO_STATE": str(tmp_path / "c.json")}
    assert configured({**env, "EVE_COMPOSIO_TOOLKITS": " Gmail, notion ,gmail"}, frozenset({7})).toolkits == (
        "gmail", "notion")
    unlimited = configured({**env, "EVE_COMPOSIO_TOOLKITS": "all"}, frozenset({7}))
    assert unlimited.toolkits is None
    client = FakeClient()
    ComposioBridge(unlimited, client_factory=lambda: client).start()
    # no allow-list, but the direct apps and a Gmail/Calendar with no other account stay out (Lean AppsPolicy P3)
    assert client.sessions.toolkits == [{"disable": ["github", "gmail", "googlecalendar", "googledrive"]}]
    down = FakeClient(accounts_raise=True)
    ComposioBridge(configured(env, frozenset({7})), client_factory=lambda: down).start()
    assert down.sessions.toolkits == [{"enable": sorted(TOOLKITS)}]


def test_two_accounts_on_one_app_make_a_multi_account_session_that_must_name_the_account(tmp_path: Path) -> None:
    cfg = _cfg(tmp_path)
    one = FakeClient(connected=(("slack", "ACTIVE", "personal"), ("notion", "ACTIVE")))
    bridge = ComposioBridge(cfg, client_factory=lambda: one)
    bridge.start()
    assert one.sessions.multi == [None]                         # one account per app: the session is unchanged
    assert bridge.multi_account is False and bridge.accounts_note() == ""

    two = FakeClient(connected=(("slack", "ACTIVE", "personal"), ("slack", "ACTIVE", "work"),
                                ("slack", "EXPIRED"), ("notion", "ACTIVE")))
    bridge = ComposioBridge(cfg, client_factory=lambda: two)
    bridge.start()
    # the stored one-account session is not resumed: multi-account mode is fixed when a session is made
    assert two.sessions.used == [] and two.sessions.created == ["telegram-7"]
    assert two.sessions.multi == [{"enable": True, "require_explicit_selection": True}]
    note = bridge.accounts_note()
    assert "slack: personal, work" in note and "notion" not in note   # an expired one is not offered
    assert f'"{PRIMARY_ALIAS}"' in note                         # writes go to the owner's own account

    again = FakeClient(connected=two.connected_accounts.connected)
    ComposioBridge(cfg, client_factory=lambda: again).start()
    assert again.sessions.used == ["trs_new_1"] and again.sessions.created == []   # same shape: resumed


def test_the_confirmation_line_names_the_account_a_write_goes_to() -> None:
    args = {"tools": [{"tool_slug": "GOOGLECALENDAR_CREATE_EVENT", "arguments": {"summary": "Lunch"},
                       "account": "work"}]}
    assert describe_for_confirmation("COMPOSIO_MULTI_EXECUTE_TOOL", args) == (
        "Run GOOGLECALENDAR_CREATE_EVENT on work with summary: Lunch?")
    del args["tools"][0]["account"]
    assert describe_for_confirmation("COMPOSIO_MULTI_EXECUTE_TOOL", args) == (
        "Run GOOGLECALENDAR_CREATE_EVENT with summary: Lunch?")


def test_only_the_owners_other_google_accounts_join_the_session_and_each_call_must_name_one() -> None:
    """2026-10-04: Drive and GitHub went direct and stay out whatever is connected. 2026-10-05 (owner: "keep composio
    for the second gmail"): Gmail and the calendar join the session only for the work account; the personal one
    (and an unnamed one, which cannot be told from it) stays out, and the session must name every account."""
    from eve_bridge import composio_tools

    def bridge_for(items: list) -> composio_tools.ComposioBridge:
        b = composio_tools.ComposioBridge(composio_tools.ComposioConfig(enabled=True, user_id="u"))
        b._client = type("C", (), {"connected_accounts": type("A", (), {
            "list": staticmethod(lambda **k: type("L", (), {"items": items})())})()})()
        return b

    items = [{"toolkit": {"slug": kit}, "status": "ACTIVE", "alias": a}
             for kit in ("googlecalendar", "gmail") for a in ("personal", "work")]
    items += [{"toolkit": {"slug": "gmail"}, "status": "ACTIVE", "alias": None, "id": "ca_unnamed"},
              {"toolkit": {"slug": "googledrive"}, "status": "ACTIVE", "alias": "work"},
              {"toolkit": {"slug": "slack"}, "status": "ACTIVE", "alias": "personal"}]
    b = bridge_for(items)
    assert b._allowed_toolkits() == ["gmail", "googlecalendar", "linear", "notion", "slack"]
    assert b._accounts["gmail"] == ["work"] and b._accounts["googlecalendar"] == ["work"]
    assert b.multi_account                                   # every call names its account
    note = b.accounts_note()
    assert "Gmail (gmail): work" in note and "never their own" in note and "personal" not in note
    # control: with only personal accounts, nothing of Google's joins and the session is as before
    only_personal = bridge_for([i for i in items if i["alias"] == "personal"])
    assert only_personal._allowed_toolkits() == ["linear", "notion", "slack"]
    assert not only_personal.multi_account and only_personal.accounts_note() == ""


def test_a_call_to_the_owners_own_google_account_through_composio_is_refused() -> None:
    from eve_bridge.composio_tools import decide

    def item(account: str | None) -> dict:
        t = {"tool_slug": "GMAIL_FETCH_EMAILS", "arguments": {}}
        if account is not None:
            t["account"] = account
        return {"tools": [t, {"tool_slug": "SLACK_SEARCH_MESSAGES", "arguments": {}}]}

    for account in (None, "", "personal", "Personal"):
        d = decide("COMPOSIO_MULTI_EXECUTE_TOOL", item(account))
        assert d.action == "refuse" and d.slugs == ["GMAIL_FETCH_EMAILS"] and "own account" in d.why
    assert decide("COMPOSIO_MULTI_EXECUTE_TOOL", item("work")).action == "run"
    # a workbench script cannot name its account, so one that calls Gmail is refused
    code = "r = run_composio_tool('GMAIL_FETCH_EMAILS', {})\nprint(len(r))"
    assert decide("COMPOSIO_REMOTE_WORKBENCH", {"code_to_execute": code}).action == "refuse"
