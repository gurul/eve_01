"""Suite-wide pytest configuration.

Live tests touch real services on this Mac: the owner's claude-mem worker (a real observation is written,
then deleted) or a real Chromium. They run only when asked, with EVE_LIVE=1, so a plain `pytest` —
every verification run — never writes to the owner's memory store.
"""

from __future__ import annotations

import itertools
import os

import pytest

_TEST_DIRS = itertools.count()


@pytest.fixture(scope="session")
def _per_test_root(tmp_path_factory: pytest.TempPathFactory):
    """One numbered folder for the whole session; each test gets a plain subfolder of it (``_test_dir``). pytest's
    mktemp scans the whole basetemp on every call (make_numbered_dir), so four autouse mktemp calls per test grew
    quadratic: about 100 s of a 13-minute run at 6,000 tests (2026-10-09 review). A mkdir under one parent is O(1)."""
    return tmp_path_factory.mktemp("per-test")


@pytest.fixture
def _test_dir(_per_test_root):
    """This test's own folder, for the autouse fixtures below. A subfolder each, never the folder itself: a test
    rglobs the spend folder for leaked words, and must not find the chief's files there."""
    d = _per_test_root / str(next(_TEST_DIRS))
    d.mkdir()
    return d


def pytest_collection_modifyitems(config: pytest.Config, items: list[pytest.Item]) -> None:
    if os.environ.get("EVE_LIVE", "").strip().lower() in ("1", "true", "yes", "on"):
        return
    skip = pytest.mark.skip(reason="live test: set EVE_LIVE=1 to run it against this Mac's real services")
    for item in items:
        if "live" in item.keywords:
            item.add_marker(skip)


@pytest.fixture(autouse=True)
def _spend_ledger_in_tmp(_test_dir):
    """Every test's spend lines (spend.py) go to a temporary folder, never the owner's real ledger: the wiring
    records from code paths many tests drive with fakes."""
    from eve_bridge import spend

    folder = _test_dir / "spend"
    folder.mkdir()
    spend.set_dir(folder)
    yield folder
    spend.set_dir(None)


@pytest.fixture(autouse=True)
def _vault_in_tmp(_test_dir, monkeypatch: pytest.MonkeyPatch):
    """Every test's vault (vault.py) is a temporary index and a keychain file that does not exist, never the
    owner's: a BrowserLane builds a Vault, and its outline lists the vault's items."""
    monkeypatch.setenv("EVE_VAULT_INDEX", str(_test_dir / "vault.json"))
    monkeypatch.setenv("EVE_VAULT_KEYCHAIN", str(_test_dir / "no-such.keychain-db"))


@pytest.fixture(autouse=True)
def _watch_list_in_tmp(_test_dir, monkeypatch: pytest.MonkeyPatch):
    """Every test's watch list (watch.py) is a temporary file, never the owner's real one: the daemon builds a
    Watcher whenever the Telegram door is on, and a Watcher reads its file when it is made."""
    monkeypatch.setenv("EVE_WATCH_FILE", str(_test_dir / "watch" / "watches.json"))


@pytest.fixture(autouse=True)
def _chief_ledger_in_tmp(_test_dir, monkeypatch: pytest.MonkeyPatch):
    """Every test's chief ledger (chief_ledger.py: cards, events, attention) is a temporary folder, never the
    owner's real ~/.config/eve-bridge/chief."""
    folder = _test_dir / "chief"
    folder.mkdir()
    monkeypatch.setenv("EVE_CHIEF_DIR", str(folder))
    yield folder


@pytest.fixture(autouse=True)
def _no_jev_from_a_telegram_turn(monkeypatch: pytest.MonkeyPatch):
    """A plain `pytest` never sends the test's words to Jev (2026-10-05): a TelegramInlet builds its toolset router
    from the environment (toolset_router.make_router), so a Jev key there would make every faked turn a real call.
    A test that wants a key sets one itself; the router's own tests pass their environment in."""
    if os.environ.get("EVE_LIVE", "").strip().lower() in ("1", "true", "yes", "on"):
        return
    for key in ("TYPESAFE_API_KEY", "OPENROUTER_API_KEY"):
        monkeypatch.delenv(key, raising=False)


@pytest.fixture(autouse=True)
def _no_ticketmaster_key(monkeypatch: pytest.MonkeyPatch):
    """A plain `pytest` never lends events_search from the owner's own key (events.py, 2026-10-08): a TelegramInlet
    lends it, and writes its line into the prompt, whenever TICKETMASTER_API_KEY is set, so a key in the shell would
    change every faked request body. A test that wants the tool sets a key itself."""
    if os.environ.get("EVE_LIVE", "").strip().lower() in ("1", "true", "yes", "on"):
        return
    monkeypatch.delenv("TICKETMASTER_API_KEY", raising=False)


@pytest.fixture(autouse=True)
def _no_desk_lane_on_the_real_desktop(monkeypatch: pytest.MonkeyPatch):
    """A plain `pytest` never clicks this Mac (2026-10-06): an agent the daemon builds tries the desk lane
    (desk_lane.py) before Codex, and that lane reads and clicks the real frontmost window. Its own tests switch it
    on with a fake desktop."""
    monkeypatch.setenv("EVE_DESK_LANE", "0")


@pytest.fixture(autouse=True)
def _no_paid_monid_runs(monkeypatch: pytest.MonkeyPatch):
    """A plain `pytest` never spends the owner's Monid balance (2026-10-08): the web search falls back to Monid
    (monid.py) when both engines fail, and many tests fail both engines on purpose. Monid's own tests switch it
    back on with a fake runner."""
    monkeypatch.setenv("EVE_MONID", "0")
    from eve_bridge import monid

    monid.reset_for_tests()
    yield
    monid.reset_for_tests()


@pytest.fixture(autouse=True)
def _chat_log_in_tmp(_test_dir, monkeypatch: pytest.MonkeyPatch):
    """Every test's chat log (chat_log.py, on by default) is a temporary file, never the owner's real
    ~/.config/eve-bridge/chat/log.json: the daemon opens one whenever the Telegram door is on."""
    monkeypatch.setenv("EVE_CHAT_LOG_FILE", str(_test_dir / "chat" / "log.json"))


@pytest.fixture(autouse=True)
def _taint_marks_per_test():
    """The taint gate's marks (tool_gate.MARKS) are one per process: each test starts with none, so a tainted reply
    one test wrote never taints another test's turn."""
    from eve_bridge import tool_gate

    tool_gate.MARKS.clear()
    yield
    tool_gate.MARKS.clear()


@pytest.fixture(autouse=True)
def _menu_tools_pinned_off(monkeypatch: pytest.MonkeyPatch):
    """The menu tools ship on (menu_tools.SHIPPED, 2026-10-09), but the suite's request-body and prompt checks were
    written against the switch-off bytes, which stay byte for byte as before: a plain test runs with it off. The menu,
    events, tool-choice and intent tests switch it on themselves (EVE_MENU_TOOLS=on)."""
    monkeypatch.setenv("EVE_MENU_TOOLS", "off")


@pytest.fixture(autouse=True)
def _phone_location_in_tmp(_test_dir, monkeypatch: pytest.MonkeyPatch):
    """Every test's phone fix (phone_location.py) is a temporary file that does not exist yet, never the owner's real
    one: with no fix, no turn offers phone_location and quick answers keep their configured place, so the suite's
    request bytes are as before. A test that wants a fix stores one."""
    monkeypatch.setenv("EVE_PHONE_LOCATION_FILE", str(_test_dir / "phone" / "phone-location.json"))
    monkeypatch.delenv("EVE_PHONE_LOCATION", raising=False)


@pytest.fixture(autouse=True)
def _science_search_pinned_off(monkeypatch: pytest.MonkeyPatch):
    """Ask science ships on (science_search.py, 2026-10-12), but the suite's request-body and prompt checks were
    written against the bytes without its tool, so a plain test runs with it off, as the menu tools' switch does.
    Its own tests (test_science_search.py) switch it on and check every surface it rides. A plain `pytest` never
    sends a search: no faked model calls it, and its own tests use a mocked transport."""
    monkeypatch.setenv("EVE_SCIENCE_SEARCH", "0")


@pytest.fixture(autouse=True)
def _find_image_pinned_off(monkeypatch: pytest.MonkeyPatch):
    """Pictures for cards ship on (find_image.py, 2026-10-13); like Ask science, a plain test runs with the tool off
    so the request-body checks keep their bytes. Its own tests (test_find_image.py) pass their own environ."""
    monkeypatch.setenv("EVE_FIND_IMAGE", "0")
