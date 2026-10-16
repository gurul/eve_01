"""A fake Playwright page for the browser guard tests (test_browser_challenge.py, test_click_recheck.py,
test_challenge_settle.py, test_click_recheck_tolerance.py).

It answers the lane's own scripts by identity (``bl._COLLECT_JS``, ``bl._RECHECK_JS`` …), the way
test_browser_navigation_freshness.py does, so no browser runs. Not a test module (no test_ prefix): a shared helper, like initdata.py.
"""

from __future__ import annotations

from typing import Any, Optional

from eve_bridge import browser_lane as bl


class FakeLocator:
    def __init__(self, page: "FakePage", eid: str) -> None:
        self.page, self.eid = page, eid

    def count(self) -> int:
        return 1 if self.page.element(self.eid) is not None else 0

    @property
    def first(self) -> "FakeLocator":
        return self

    def scroll_into_view_if_needed(self, timeout: int = 0) -> None:
        self.page.before_click()

    def click(self, timeout: int = 0) -> None:
        self.page.clicks.append(self.eid)

    def fill(self, value: str, timeout: int = 0) -> None:
        self.page.clicks.append(self.eid)


class FakeKeyboard:
    def __init__(self, page: "FakePage") -> None:
        self.page = page

    def press(self, key: str) -> None:
        self.page.keys.append(key)

    def type(self, text: str, delay: int = 0) -> None:
        self.page.keys.append(text)


class FakePage:
    """One document: ``url``, ``title_text``, ``body`` and ``elements`` (the collector's raw rows).
    ``routes`` maps a URL to (title, body, elements) for goto. ``doc`` stands for performance.timeOrigin;
    a goto starts a new document. ``mutate`` runs after the scroll and before the recheck."""

    def __init__(self, url: str, title: str, body: str, elements: Optional[list[dict[str, Any]]] = None,
                 routes: Optional[dict[str, tuple[str, str, list[dict[str, Any]]]]] = None) -> None:
        self.url, self.title_text, self.body = url, title, body
        self.elements = [dict(e) for e in elements or []]
        self.routes = routes or {}
        self.doc = 1000.5
        self.clicks: list[str] = []
        self.keys: list[str] = []
        self.rechecks = 0
        self.mutate: Any = None
        self.keyboard = FakeKeyboard(self)

    def element(self, eid: str) -> Optional[dict[str, Any]]:
        return next((e for e in self.elements if str(e["id"]) == str(eid)), None)

    def before_click(self) -> None:
        if self.mutate is not None:
            self.mutate(self)
            self.mutate = None

    def evaluate(self, script: str, arg: Any = None) -> Any:
        if script == bl._COLLECT_JS:
            return {"elements": [dict(e) for e in self.elements], "title": self.title_text, "url": self.url,
                    "dialog": "", "focused_id": None, "document_id": str(self.doc),
                    "head": self.body[:600], "text_hash": f"{len(self.body)}:{self.body[:4000]}"}
        if script == getattr(bl, "_RECHECK_JS", None):
            # The script's read, answered from Python; test_click_recheck_tolerance.py runs the real one in node.
            self.rechecks += 1
            e = self.element(arg["id"])
            now = {"url": self.url, "doc": str(self.doc), "found": e is not None}
            if e is not None:
                now.update(role=e.get("role", ""), name=e.get("name", ""), href=e.get("href", ""),
                           dom_id=e.get("dom_id", ""))
            return now
        if script == bl._CONSENT_JS:
            return None
        if script == bl._WEBMCP_JS:
            return {"api": False}
        return self.body + " "                                       # text_visible's read

    def title(self) -> str:
        return self.title_text

    def inner_text(self, selector: str, timeout: int = 0) -> str:
        return self.body

    def locator(self, selector: str) -> FakeLocator:
        return FakeLocator(self, selector.split('"')[1] if '"' in selector else selector)

    def goto(self, url: str, wait_until: str = "", timeout: int = 0) -> None:
        self.url = url
        self.title_text, self.body, elements = self.routes.get(url, ("", "", []))
        self.elements = [dict(e) for e in elements]
        self.doc += 1.0

    def wait_for_load_state(self, state: str = "", timeout: int = 0) -> None:
        return None

    def wait_for_timeout(self, ms: int) -> None:
        return None

    def bring_to_front(self) -> None:
        return None

    def close(self) -> None:
        return None


class FakeContext:
    def __init__(self, page: FakePage) -> None:
        self.pages = [page]

    def close(self) -> None:
        return None


class NoVault:
    def lines(self) -> list[str]:
        return []


def lane_on(page: FakePage, tmp_path: Any) -> bl.BrowserLane:
    """A BrowserLane whose launcher hands it ``page``: no Playwright, no browser."""
    return bl.BrowserLane(bl.BrowserLaneConfig(profile=tmp_path / "profile", headless=True),
                          launcher=lambda: FakeContext(page), vault=NoVault())


def link(eid: str, name: str, href: str) -> dict[str, Any]:
    return {"id": eid, "role": "link", "name": name, "value": "", "x": 10, "y": 10 + 30 * int(eid), "w": 120,
            "h": 20, "enabled": True, "href": href}


def button(eid: str, name: str) -> dict[str, Any]:
    return {"id": eid, "role": "button", "name": name, "value": "", "x": 10, "y": 10 + 30 * int(eid), "w": 120,
            "h": 20, "enabled": True, "href": ""}
