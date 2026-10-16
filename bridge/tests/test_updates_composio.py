"""updates_composio.py: Updates' Composio sources (Slack, Notion, Linear, the work Gmail and Calendar). Each parses
Composio's answer shape (tests/fixtures/updates/composio/responses.json, every value fictional), is off without a
connection, fails (and so is lost) on an expired one, and can only run the exact read slugs on its allow-list. No
network: a fake bridge answers from the fixture."""
from __future__ import annotations

import asyncio
import copy
import json
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Optional

import pytest

from eve_bridge import composio_tools, updates, updates_composio
from eve_bridge.updates import Sources, Updates, clean_cards, gather, lost_kinds, ref_kind
from eve_bridge.updates_composio import READ_SLUGS, Reader, SourceOff

TZ = timezone(timedelta(hours=-7))
NOW = datetime(2026, 10, 12, 9, 30, tzinfo=TZ)
FIXTURE = json.loads((Path(__file__).parent / "fixtures" / "updates" / "composio" / "responses.json").read_text())
COMPOSIO_KINDS = set(updates_composio.KINDS)


class FakeBridge:
    """ComposioBridge as updates_composio reads it: ``can_read``, ``connected_accounts()`` and ``read_tool(slug,
    arguments, account_id)``, answering from the fixture. ``fail`` names slugs that fail; ``accounts`` replaces the
    listing; ``list_fails`` makes the listing raise."""

    def __init__(self, *, accounts: Optional[list[dict[str, str]]] = None, fail: tuple[str, ...] = (),
                 list_fails: bool = False) -> None:
        self.can_read = True
        self.accounts = copy.deepcopy(FIXTURE["connected_accounts"] if accounts is None else accounts)
        self.fail, self.list_fails = fail, list_fails
        self.calls: list[tuple[str, dict[str, Any], str]] = []

    def connected_accounts(self) -> list[dict[str, str]]:
        if self.list_fails:
            raise ConnectionError("down")
        return copy.deepcopy(self.accounts)

    def read_tool(self, slug: str, arguments: dict[str, Any], account_id: str) -> dict[str, Any]:
        self.calls.append((slug, dict(arguments), account_id))
        if slug in self.fail:
            return {"ok": False, "reason": "HTTP 401: the connection has expired"}
        answer = self._answer(slug, arguments)
        return {**copy.deepcopy(answer), "ok": True}

    @staticmethod
    def _answer(slug: str, args: dict[str, Any]) -> dict[str, Any]:
        if slug == "SLACK_SEARCH_MESSAGES":
            q = args["query"]
            key = "from:me" if q == "from:me" else "is:dm" if q.startswith("is:dm") else "mention"
            return FIXTURE["SLACK_SEARCH_MESSAGES"][key]
        if slug == "NOTION_GET_ABOUT_ME":
            return FIXTURE["NOTION"]["about_me"]
        if slug == "NOTION_SEARCH_NOTION_PAGE":
            return FIXTURE["NOTION"]["search"]
        if slug == "NOTION_FETCH_BLOCK_CONTENTS":
            return FIXTURE["NOTION"]["blocks"].get(args["block_id"]) or {"data": {"results": []}, "successful": True}
        if slug == "LINEAR_LIST_LINEAR_ISSUES":
            return FIXTURE["LINEAR_LIST_LINEAR_ISSUES"]
        if slug == "GMAIL_FETCH_EMAILS":
            return FIXTURE["GMAIL_FETCH_EMAILS"]["sent" if "in:sent" in args["query"] else "inbox"]
        if slug == "GOOGLECALENDAR_EVENTS_LIST":
            return FIXTURE["GOOGLECALENDAR_EVENTS_LIST"]
        raise AssertionError(f"unexpected slug {slug}")


def sources(bridge: Any, hidden: Any = None, environ: Optional[dict[str, str]] = None) -> Sources:
    class S(Sources):
        def apps(self, app: str) -> Any:
            return None

        def vault(self) -> Optional[Path]:
            return None

        def chief(self) -> Any:
            return None

    return S(records_dir=lambda: None, hidden=lambda: hidden, composio=lambda: bridge, environ=environ or {})


def run_gather(src: Sources, **kw: Any) -> tuple[dict[str, Any], dict[str, str]]:
    status: dict[str, str] = {}
    snap = asyncio.run(gather(src, now=NOW, tz="America/Los_Angeles", status=status, **kw))
    return snap, status


def by_id(items: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    return {i["id"]: i for i in items}


# ---- parsing, one source at a time ------------------------------------------------------------------------------

def test_slack_keeps_unanswered_dms_and_mentions_one_per_conversation() -> None:
    rows = by_id(updates_composio.slack_items(Reader(FakeBridge()), NOW))
    dm = next(v for k, v in rows.items() if k.startswith("slack:D0000DM01/"))
    mention = next(v for k, v in rows.items() if k.startswith("slack:C0000GEN1/"))
    assert len(rows) == 2                                   # the answered DM and the 11-day-old one are out
    assert dm["kind"] == "slack" and dm["title"] == "Slack: direct message from sam.rivera"
    assert "2 messages the owner has not answered" in dm["body"]
    assert "budget sheet" in dm["body"] and "https://" not in dm["body"]     # a Slack link reads as its label
    assert "@you" in dm["body"] and "UOWNER001" not in dm["body"]
    assert dm["link"].startswith("https://example-team.slack.com/archives/D0000DM01/")
    assert dm["id"].endswith(FIXTURE["SLACK_SEARCH_MESSAGES"]["is:dm"]["data"]["messages"]["matches"][0]["ts"])
    assert mention["title"] == "Slack: mention in #project-alpha from alex.chen"
    assert "#project-alpha" in mention["body"]
    assert list(rows)[0] == mention["id"]                   # newest first


def test_notion_reads_open_todos_mentions_and_assigned_open_tasks() -> None:
    rows = by_id(updates_composio.notion_items(Reader(FakeBridge()), NOW))
    p1, p2 = "notion:aaaaaaaa-0000-4000-8000-000000000001", "notion:aaaaaaaa-0000-4000-8000-000000000002"
    assert set(rows) == {p1, p2}            # the done task, the page with nothing to do and the old page are out
    trip = rows[p1]
    assert trip["title"] == "Notion: Trip planning"
    assert "- [ ] Book the ferry for Oct 18" in trip["body"] and "Renew passport" not in trip["body"]
    assert "Mentions the owner: @Owner please confirm the dates" in trip["body"]
    assert trip["link"].startswith("https://www.notion.so/")
    task = rows[p2]
    assert "Assigned to the owner. Status: In progress." in task["body"] and "Date: 2026-10-15." in task["body"]


def test_notion_reads_no_blocks_after_its_deadline() -> None:
    bridge = FakeBridge()
    rows = updates_composio.notion_items(Reader(bridge), NOW, deadline=0.0, clock=lambda: 1.0)
    assert [r["id"] for r in rows] == ["notion:aaaaaaaa-0000-4000-8000-000000000002"]   # assigned: no blocks needed
    assert not any(c[0] == "NOTION_FETCH_BLOCK_CONTENTS" for c in bridge.calls)


def test_linear_keeps_open_issues_due_soon_or_recently_updated() -> None:
    bridge = FakeBridge()
    rows = updates_composio.linear_items(Reader(bridge), NOW)
    assert [r["id"] for r in rows] == ["linear:bbbbbbbb-0000-4000-8000-000000000002",      # due first
                                       "linear:bbbbbbbb-0000-4000-8000-000000000001"]
    due, recent = rows
    assert due["title"] == "Linear EX-102: Write the migration guide" and "Due: 2026-10-14." in due["body"]
    assert due["when"] == "2026-10-14"
    assert "Priority: Urgent." in recent["body"] and "State: In Progress." in recent["body"]
    assert recent["link"].startswith("https://linear.app/")
    assert bridge.calls[0][1] == {"assignee_id": "me", "first": updates_composio.LINEAR_FIRST}


def test_work_email_reads_the_work_account_only_one_item_per_thread() -> None:
    bridge = FakeBridge()
    rows = by_id(updates_composio.work_email_items(Reader(bridge), NOW))
    assert set(rows) == {"wgmail:work/th1", "wgmail:work/th2"}
    renewal, agenda = rows["wgmail:work/th1"], rows["wgmail:work/th2"]
    assert renewal["kind"] == "work_email" and renewal["title"] == "Work email: Contract renewal"
    assert "Please sign the renewal by Oct 20." in renewal["body"]           # the thread's newest message
    assert renewal["from"].startswith("Pat Lee")
    assert "The owner already replied after this message, on 2026-10-11." in agenda["body"]
    assert renewal["link"].startswith("https://mail.google.com/")
    assert {c[2] for c in bridge.calls} == {"ca_gmailwork"}                  # never the personal account
    inbox = next(c[1] for c in bridge.calls if "in:inbox" in c[1]["query"])
    assert inbox["query"] == updates.MAIL_QUERY


def test_work_calendar_reads_like_the_owners_calendar() -> None:
    bridge = FakeBridge()
    rows = by_id(updates_composio.work_calendar_items(Reader(bridge), NOW))
    assert set(rows) == {"wcal:work/w1", "wcal:work/w3"}                     # declined and cancelled are out
    review = rows["wcal:work/w1"]
    assert review["title"] == "Work calendar: Design review" and review["kind"] == "work_calendar"
    assert "The owner has not answered this invitation." in review["body"] and "Guests: 4" in review["body"]
    assert review["link"].startswith("https://www.google.com/calendar/")
    assert {c[2] for c in bridge.calls} == {"ca_calwork01"}


def test_no_other_account_means_the_work_kinds_are_off() -> None:
    only_personal = [a for a in FIXTURE["connected_accounts"] if a["alias"] != "work"]
    reader = Reader(FakeBridge(accounts=only_personal))
    for kind in ("work_email", "work_calendar"):
        with pytest.raises(SourceOff):
            reader.accounts_for(kind)


def test_links_are_kept_only_on_the_apps_own_https_hosts() -> None:
    ok = updates_composio.safe_link
    assert ok("https://team.slack.com/archives/C1/p1", "slack")
    assert ok("https://linear.app/x/issue/a-1", "linear")
    assert ok("http://team.slack.com/archives/C1/p1", "slack") is None
    assert ok("https://slack.com.evil.example/x", "slack") is None
    assert ok("https://user@team.slack.com/x", "slack") is None
    assert ok("https://linear.app/x", "slack") is None
    assert ok("javascript:alert(1)", "notion") is None


# ---- gather: status, off, failed, lost ------------------------------------------------------------------------

def test_every_composio_source_lands_in_gather_with_status_ok() -> None:
    snap, status = run_gather(sources(FakeBridge()))
    assert {k: status[k] for k in COMPOSIO_KINDS} == {k: "ok" for k in COMPOSIO_KINDS}
    kinds = {i["kind"] for i in snap["items"]}
    assert kinds == COMPOSIO_KINDS
    for it in snap["items"]:
        assert set(it) <= {"id", "kind", "title", "from", "when", "body", "link"}
        assert ref_kind(it["id"]) == it["kind"]
        assert len(it["body"]) <= updates.BODY_CHARS and len(it["title"]) <= updates.TITLE_CHARS


def test_the_model_never_reads_an_items_link() -> None:
    snap, _ = run_gather(sources(FakeBridge()))
    body = updates.prompt_input(snap)
    assert "slack.com" not in body and "linear.app" not in body and "notion.so" not in body


def test_off_without_a_bridge_or_with_the_switch_off() -> None:
    for src in (sources(None), sources(FakeBridge(), environ={"EVE_UPDATES_COMPOSIO": "0"})):
        snap, status = run_gather(src)
        assert {status[k] for k in COMPOSIO_KINDS} == {"off"} and snap["items"] == []


def test_a_bridge_that_cannot_read_is_off() -> None:
    bridge = FakeBridge()
    bridge.can_read = False                     # Composio not installed or not started
    _, status = run_gather(sources(bridge))
    assert {status[k] for k in COMPOSIO_KINDS} == {"off"} and bridge.calls == []


def test_a_toolkit_that_is_not_connected_is_off_and_the_rest_still_read() -> None:
    accounts = [a for a in FIXTURE["connected_accounts"] if a["toolkit"] != "linear"]
    _, status = run_gather(sources(FakeBridge(accounts=accounts)))
    assert status["linear"] == "off" and status["slack"] == "ok" and status["notion"] == "ok"


def test_an_expired_connection_fails_and_is_lost() -> None:
    accounts = [dict(a, status="EXPIRED") if a["toolkit"] == "notion" else a for a in FIXTURE["connected_accounts"]]
    _, status = run_gather(sources(FakeBridge(accounts=accounts)))
    assert status["notion"] == "failed" and status["slack"] == "ok"
    assert "notion" in lost_kinds(status, {"notion": "ok"})
    assert "notion" in lost_kinds(status, {})                     # failed is lost even with no build before


def test_a_failing_call_fails_its_source_only() -> None:
    _, status = run_gather(sources(FakeBridge(fail=("LINEAR_LIST_LINEAR_ISSUES",))))
    assert status["linear"] == "failed" and status["work_email"] == "ok"


def test_an_unreadable_listing_fails_every_composio_source() -> None:
    _, status = run_gather(sources(FakeBridge(list_fails=True)))
    assert {status[k] for k in COMPOSIO_KINDS} == {"failed"}


def test_a_source_that_goes_off_after_a_full_build_is_lost() -> None:
    accounts = [a for a in FIXTURE["connected_accounts"] if a["toolkit"] != "slack"]
    _, status = run_gather(sources(FakeBridge(accounts=accounts)))
    assert status["slack"] == "off"
    assert "slack" in lost_kinds(status, {"slack": "ok"}) and "slack" not in lost_kinds(status, {"slack": "off"})


def test_the_accounts_are_listed_once_per_gather() -> None:
    class Counting(FakeBridge):
        listed = 0

        def connected_accounts(self) -> list[dict[str, str]]:
            Counting.listed += 1
            return super().connected_accounts()

    run_gather(sources(Counting()))
    assert Counting.listed == 1


def test_hidden_words_and_dismissed_refs_drop_composio_items() -> None:
    hidden = lambda text: "budget sheet" in text.lower() or "migration guide" in text.lower()   # noqa: E731
    snap, _ = run_gather(sources(FakeBridge(), hidden=hidden), exclude=["wcal:work/w3"])
    ids = {i["id"] for i in snap["items"]}
    assert not any(i.startswith("slack:D0000DM01/") for i in ids)            # matched in the body
    assert "linear:bbbbbbbb-0000-4000-8000-000000000002" not in ids          # matched in the title
    assert "wcal:work/w3" not in ids and "wcal:work/w1" in ids               # dismissed
    assert "linear:bbbbbbbb-0000-4000-8000-000000000001" in ids


# ---- the allow-list -------------------------------------------------------------------------------------------

def test_every_allowed_slug_only_reads() -> None:
    for slug in READ_SLUGS:
        assert composio_tools.is_read_only(slug), slug
        assert composio_tools.consequential_slugs(composio_tools.MULTI_EXECUTE, {"tools": [{"tool_slug": slug}]}) == []


def test_a_sending_slug_is_refused_before_it_reaches_composio() -> None:
    bridge = FakeBridge()
    reader = Reader(bridge)
    for slug in ("SLACK_SEND_MESSAGE", "GMAIL_SEND_EMAIL", "LINEAR_CREATE_LINEAR_ISSUE", "NOTION_ARCHIVE_NOTION_PAGE"):
        with pytest.raises(PermissionError):
            reader.call(slug, {}, "ca_slack0001")
    assert bridge.calls == []


def test_the_consequential_check_guards_even_an_allow_listed_slug(monkeypatch: pytest.MonkeyPatch) -> None:
    """Positive control for the second guard: a writing slug that somehow got onto the allow-list is still refused."""
    monkeypatch.setattr(updates_composio, "READ_SLUGS", READ_SLUGS | {"SLACK_SEND_MESSAGE"})
    bridge = FakeBridge()
    with pytest.raises(PermissionError):
        Reader(bridge).call("SLACK_SEND_MESSAGE", {"channel": "C1", "text": "hi"}, "ca_slack0001")
    assert bridge.calls == []


def test_every_slug_the_sources_run_is_on_the_allow_list() -> None:
    bridge = FakeBridge()
    run_gather(sources(bridge))
    ran = {c[0] for c in bridge.calls}
    assert ran and ran <= READ_SLUGS


def test_the_bridge_refuses_a_writing_slug_itself() -> None:
    class Client:
        def __init__(self) -> None:
            self.ran: list[str] = []
            self.tools = self

        def execute(self, slug: str, **kw: Any) -> dict[str, Any]:
            self.ran.append(slug)
            return {"data": {"ok": True}, "error": None, "successful": True}

    cfg = composio_tools.ComposioConfig(enabled=True, user_id="telegram-1")
    bridge = composio_tools.ComposioBridge(cfg, client_factory=Client)
    bridge._client = Client()
    assert bridge.read_tool("SLACK_SEND_MESSAGE", {}, "ca_1")["ok"] is False
    assert bridge.read_tool("GITHUB_LIST_REPOSITORIES", {}, "ca_1")["ok"] is False      # a direct app
    assert bridge.read_tool("SLACK_SEARCH_MESSAGES", {}, "")["ok"] is False             # no account
    assert bridge._client.ran == []
    got = bridge.read_tool("SLACK_SEARCH_MESSAGES", {"query": "from:me"}, "ca_1")
    assert got["ok"] is True and bridge._client.ran == ["SLACK_SEARCH_MESSAGES"]


def test_connected_accounts_leaves_direct_apps_out() -> None:
    class Listing:
        items = [{"toolkit": {"slug": "SLACK"}, "id": "ca_1", "alias": None, "status": "ACTIVE"},
                 {"toolkit": {"slug": "googledrive"}, "id": "ca_2", "alias": None, "status": "ACTIVE"},
                 {"toolkit": {"slug": "gmail"}, "id": "ca_3", "alias": "work", "status": "EXPIRED"}]

    class Client:
        class connected_accounts:                 # noqa: N801 — the SDK's attribute
            @staticmethod
            def list(user_ids: list[str]) -> Listing:
                return Listing()

    bridge = composio_tools.ComposioBridge(composio_tools.ComposioConfig(enabled=True, user_id="telegram-1"))
    bridge._client = Client()
    assert bridge.connected_accounts() == [
        {"toolkit": "slack", "id": "ca_1", "alias": "", "status": "ACTIVE"},
        {"toolkit": "gmail", "id": "ca_3", "alias": "work", "status": "EXPIRED"}]


# ---- cards: refs, links and the whole rebuild -----------------------------------------------------------------

def test_clean_cards_accepts_every_composio_ref_and_keeps_the_link() -> None:
    snap, _ = run_gather(sources(FakeBridge()))
    refs = {i["kind"]: i["id"] for i in snap["items"]}
    raw = {"cards": [{"verb": "Reply", "text": f"to the {kind} item today", "entity": None, "refs": [ref],
                      "due": None} for kind, ref in refs.items()]}
    cards = clean_cards(raw, snap)
    assert {c["kind"] for c in cards} == COMPOSIO_KINDS
    for c in cards:
        assert updates_composio.safe_link(c["link"], c["kind"]) == c["link"]
        served = updates.served(c)
        assert served["link"] == c["link"] and served["kind"] == c["kind"]


def test_a_card_citing_an_unknown_composio_ref_is_dropped() -> None:
    snap, _ = run_gather(sources(FakeBridge()))
    raw = {"cards": [{"verb": "Reply", "text": "to Sam", "entity": None, "refs": ["slack:D0000DM01/1.000001"],
                      "due": None}]}
    assert clean_cards(raw, snap) == []


def test_a_merged_card_keeps_the_link_of_its_first_ref_that_has_one() -> None:
    snap, _ = run_gather(sources(FakeBridge()))
    snap["items"].append({"id": "gmail:t1", "kind": "email", "title": "Budget", "body": "x"})
    slack_ref = next(i["id"] for i in snap["items"] if i["kind"] == "slack")
    raw = {"cards": [{"verb": "Reply", "text": "to Sam about the budget", "entity": None,
                      "refs": ["gmail:t1", slack_ref], "due": None}]}
    (c,) = clean_cards(raw, snap)
    assert c["kind"] == "email" and "slack.com" in updates.served(c)["link"]


def test_a_served_card_never_carries_a_link_off_its_apps_host() -> None:
    card = {"id": "x", "verb": "Reply", "text": "to Sam", "kind": "slack", "source": "Slack", "refs": ["slack:a/1.1"],
            "link": "https://example.test/phish"}
    assert "link" not in updates.served(card)


class OneCardClient:
    def __init__(self) -> None:
        self.calls: list[str] = []

    def complete(self, instructions: str, body: str) -> str:
        self.calls.append(body)
        snap = json.loads(body.split("<snapshot>\n", 1)[1].rsplit("\n</snapshot>", 1)[0])
        ref = next(i["id"] for i in snap["items"] if i["kind"] == "linear")
        return json.dumps({"cards": [{"verb": "Fix", "text": "the login timeout, it is urgent", "entity": "EX-101",
                                      "refs": [ref], "due": None}]})


def test_a_rebuild_keeps_a_lost_composio_sources_old_cards(tmp_path: Path) -> None:
    bridge = FakeBridge()
    u = Updates(path=tmp_path / "u.json", sources=sources(bridge), client=lambda: OneCardClient(), now=lambda: NOW,
                environ={})
    assert asyncio.run(u._rebuild(OneCardClient())) == 1
    first = u.store.load()
    assert first["sources"]["linear"] == "ok" and first["cards"][0]["kind"] == "linear"
    # Linear's connection expires: the build loses it, keeps its card, and is not stamped as fresh
    bridge.accounts = [dict(a, status="EXPIRED") if a["toolkit"] == "linear" else a for a in bridge.accounts]

    class NoCards:
        def complete(self, instructions: str, body: str) -> str:
            return json.dumps({"cards": []})

    later = NOW + timedelta(hours=3)
    u._now = lambda: later
    asyncio.run(u._rebuild(NoCards()))
    state = u.store.load()
    assert u.last_status["linear"] == "failed"
    assert [c["kind"] for c in state["cards"]] == ["linear"]
    assert state["generated_at"] == first["generated_at"]
    assert u._next_try > 0                                       # the loop tries again soon


def test_the_prompt_names_the_kinds_as_one_positive_set() -> None:
    line = next(line for line in updates.SYSTEM.splitlines() if "the kinds are exactly these" in line)
    for kind in updates.KINDS:
        assert kind in line


def test_updates_knows_every_composio_kind_and_ref_prefix() -> None:
    assert set(updates_composio.KINDS) <= set(updates.KINDS)
    for kind, prefix in updates_composio.REF_PREFIX.items():
        assert updates.REF_KINDS[prefix] == kind and ref_kind(f"{prefix}:x") == kind
    assert set(updates_composio.LIMITS) == set(updates_composio.KINDS) == set(updates_composio.READERS)
