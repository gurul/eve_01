"""The click recheck tolerates what a live page changes on its own, and nothing else.

Two things refused good clicks: an in-page #fragment change (a "jump to comments" link, a single-page app's
router) counted as a new page address, and a label with a live counter ("142 points" → "143 points",
"3 minutes ago" → "4 minutes ago") counted as a new control. The recheck now ignores the fragment, and lets a
label differ only in digits and relative-time words when the role and the href (or the role and the DOM id)
are unchanged. Every other difference still refuses.

The last tests run the lane's real _COLLECT_JS and _RECHECK_JS in node over a small DOM stand-in, so the
JavaScript side is checked too: browser_fakes.FakePage answers the recheck from Python and would pass whatever
the script does.
"""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path
from typing import Any, Callable, Optional

import pytest
from browser_fakes import FakePage, button, link

from eve_bridge import browser_lane as bl

HREF = "https://news.example/item?id=41"


def rec(**over: Any) -> dict[str, Any]:
    base = {"url": "https://news.example/", "doc": "1", "role": "link", "name": "142 points", "href": HREF,
            "dom_id": ""}
    return {**base, **over}


def now(**over: Any) -> dict[str, Any]:
    return {**rec(), "found": True, **over}


@pytest.mark.parametrize(("recorded", "seen"), [
    (rec(), now(url="https://news.example/#comments")),                       # fragment only
    (rec(url="https://news.example/#top"), now(url="https://news.example/#bottom")),
    (rec(), now(name="143 points")),                                          # a live counter
    (rec(name="1,204 comments"), now(name="1,205 comments")),
    (rec(name="3 minutes ago"), now(name="4 minutes ago")),
    (rec(name="2 hours ago"), now(name="1 day ago")),
    (rec(name="just now"), now(name="2 minutes ago")),
    (rec(name="World (3 new)"), now(name="World (4 new)")),
    (rec(name="12.3K views · 3h"), now(name="12.4K views · 4h")),
    (rec(role="button", href="", dom_id="like-41", name="Like 5"), now(role="button", href="", dom_id="like-41",
                                                                       name="Like 6")),
])
def test_tolerated(recorded: dict[str, Any], seen: dict[str, Any]) -> None:
    assert bl.click_recheck(recorded, seen) == []


@pytest.mark.parametrize(("recorded", "seen", "what"), [
    (rec(), now(url="https://news.example/news?p=2"), "page address"),        # the query is not a fragment
    (rec(), now(url="https://news.example/other#comments"), "page address"),
    (rec(name="Upvote"), now(name="Unvote"), "name"),                         # a word changed
    (rec(name="Follow"), now(name="Following"), "name"),
    (rec(name="142 points"), now(name="142 points flagged"), "name"),
    (rec(role="button", href="", name="Show 5 more"), now(role="button", href="", name="Show 6 more"), "name"),
    (rec(role="button", href="", dom_id="a", name="Like 5"), now(role="button", href="", dom_id="b",
                                                                 name="Like 6"), "name"),
    (rec(), now(name="143 points", href="https://news.example/item?id=42"), "href"),
    (rec(), now(name="143 points", role="button"), "role"),
    (rec(), now(doc="2"), "document"),
])
def test_still_refused(recorded: dict[str, Any], seen: dict[str, Any], what: str) -> None:
    assert what in bl.click_recheck(recorded, seen)


def test_a_counter_and_a_fragment_change_do_not_stop_a_click() -> None:
    page = FakePage("https://news.example/", "News", "Top stories",
                    [link("0", "142 points", HREF), button("1", "Subscribe")])
    p = bl._Page(page)
    target = next(c for c in p.snapshot().elements if c.label == "142 points")

    def live(pg: FakePage) -> None:
        pg.url = "https://news.example/#item-41"
        pg.element("0")["name"] = "143 points"

    page.mutate = live
    said = p.click_candidate(target)
    assert said.startswith("clicked"), said
    assert page.clicks == ["0"]


def test_a_renamed_control_is_still_refused_through_the_page() -> None:
    page = FakePage("https://news.example/", "News", "Top stories", [link("0", "41 comments", HREF)])
    p = bl._Page(page)
    target = p.snapshot().elements[0]
    page.mutate = lambda pg: pg.element("0").update(name="41 comments hidden")
    assert p.click_candidate(target).startswith(bl.STALE_PREFIX)
    assert page.clicks == []


# ---- the real scripts, in node ---------------------------------------------------------------------------

NODE = shutil.which("node")

# A DOM stand-in just wide enough for _IDENTITY_JS, _COLLECT_JS and _RECHECK_JS. Elements come in as JSON
# and their attributes (data-eve-id) go back out, so the state carries from one evaluate to the next.
HARNESS = r"""
const input = JSON.parse(require('fs').readFileSync(0, 'utf8'));
const st = input.state;
class El {
  constructor(d) { Object.assign(this, d); this.attrs = d.attrs || {}; }
  get tagName() { return this.tag.toUpperCase(); }
  getAttribute(k) { return k in this.attrs ? this.attrs[k] : null; }
  hasAttribute(k) { return k in this.attrs; }
  setAttribute(k, v) { this.attrs[k] = String(v); }
  removeAttribute(k) { delete this.attrs[k]; }
  get href() { return this.attrs.href || ''; }
  get id() { return this.attrs.id || ''; }
  get innerText() { return this.text || ''; }
  get value() { return this.attrs.value; }
  get placeholder() { return this.attrs.placeholder || ''; }
  get title() { return this.attrs.title || ''; }
  get type() { return this.attrs.type || ''; }
  get disabled() { return 'disabled' in this.attrs; }
  get isContentEditable() { return false; }
  get labels() { return []; }
  getBoundingClientRect() { const [x, y, w, h] = this.rect || [10, 10, 100, 20];
    return {left: x, top: y, width: w, height: h, right: x + w, bottom: y + h}; }
  closest() { return null; }
  querySelector() { return null; }
}
const els = st.elements.map(d => new El(d));
global.window = {innerHeight: 800, innerWidth: 1200};
global.location = {href: st.url};
global.performance = {timeOrigin: st.doc};
global.getComputedStyle = () => ({visibility: 'visible', display: 'block', opacity: '1'});
global.document = {
  title: st.title, body: {innerText: st.body}, activeElement: null,
  getElementById: () => null,
  querySelectorAll: (sel) => sel === '[data-eve-id]' ? els.filter(e => e.hasAttribute('data-eve-id'))
                           : sel.startsWith('dialog') ? [] : els,
  querySelector: (sel) => { const m = /data-eve-id="([^"]*)"/.exec(sel);
    return m ? (els.find(e => e.getAttribute('data-eve-id') === m[1]) || null) : null; },
};
const fn = eval('(' + input.script + ')');
const result = fn(input.arg);
process.stdout.write(JSON.stringify({result, attrs: els.map(e => e.attrs)}));
"""


class NodePage:
    """A page whose evaluate runs the lane's script in node over ``state``. Clicks are recorded, not run."""

    def __init__(self, tmp: Path, url: str, elements: list[dict[str, Any]]) -> None:
        self.harness = tmp / "harness.js"
        self.harness.write_text(HARNESS)
        self.state = {"url": url, "doc": 1000.5, "title": "News", "body": "Top stories", "elements": elements}
        self.clicks: list[str] = []
        self.mutate: Optional[Callable[["NodePage"], None]] = None
        self.ran: list[str] = []

    def evaluate(self, script: str, arg: Any = None) -> Any:
        self.ran.append("collect" if script == bl._COLLECT_JS else "recheck" if script == bl._RECHECK_JS else "?")
        out = subprocess.run([NODE, str(self.harness)], input=json.dumps({"state": self.state, "script": script,
                             "arg": arg}), capture_output=True, text=True, timeout=20, check=True)
        got = json.loads(out.stdout)
        for d, attrs in zip(self.state["elements"], got["attrs"], strict=True):
            d["attrs"] = attrs
        return got["result"]

    def el(self, eid: str) -> dict[str, Any]:
        return next(d for d in self.state["elements"] if d["attrs"].get("data-eve-id") == eid)

    def locator(self, selector: str) -> "NodePage._Loc":
        return NodePage._Loc(self, selector.split('"')[1])

    class _Loc:
        def __init__(self, page: "NodePage", eid: str) -> None:
            self.page, self.eid = page, eid
            self.first = self

        def count(self) -> int:
            return sum(1 for d in self.page.state["elements"] if d["attrs"].get("data-eve-id") == self.eid)

        def scroll_into_view_if_needed(self, timeout: int = 0) -> None:
            if self.page.mutate is not None:
                self.page.mutate(self.page)
                self.page.mutate = None

        def click(self, timeout: int = 0) -> None:
            self.page.clicks.append(self.eid)

    def wait_for_timeout(self, ms: int) -> None:
        return None


needs_node = pytest.mark.skipif(NODE is None, reason="node is not installed")


def hn_elements() -> list[dict[str, Any]]:
    return [{"tag": "a", "attrs": {"href": HREF}, "text": "142 points", "rect": [10, 10, 80, 18]},
            {"tag": "a", "attrs": {"href": "https://news.example/from?site=example.com"}, "text": "example.com",
             "rect": [10, 40, 60, 18]},
            {"tag": "button", "attrs": {"id": "replies-41"}, "text": "5 replies", "rect": [10, 70, 60, 18]}]


def node_click(tmp_path: Path, label: str, mutate: Callable[[NodePage], None]) -> tuple[str, NodePage]:
    page = NodePage(tmp_path, "https://news.example/", hn_elements())
    p = bl._Page(page)
    snap = p.snapshot()
    target = next(c for c in snap.elements if c.label == label)
    page.mutate = mutate
    return p.click_candidate(target), page


@needs_node
def test_real_scripts_read_an_unchanged_control_as_the_same(tmp_path: Path) -> None:
    said, page = node_click(tmp_path, "example.com", lambda pg: None)
    assert said.startswith("clicked"), said
    assert page.ran == ["collect", "recheck"] and page.clicks


@needs_node
def test_real_scripts_tolerate_a_counter_and_a_fragment(tmp_path: Path) -> None:
    def live(pg: NodePage) -> None:
        pg.state["url"] = "https://news.example/#item-41"
        pg.el("0")["text"] = "143 points"
        pg.el("2")["text"] = "6 replies"

    said, page = node_click(tmp_path, "142 points", live)
    assert said.startswith("clicked"), said
    said, page = node_click(tmp_path, "5 replies", live)        # a button: tolerated by its unchanged DOM id
    assert said.startswith("clicked"), said


@needs_node
@pytest.mark.parametrize(("label", "mutate", "what"), [
    ("example.com", lambda pg: pg.el("1").update(text="Sponsored"), "name"),
    ("example.com", lambda pg: pg.el("1")["attrs"].update(href="https://ads.example/x"), "href"),
    ("142 points", lambda pg: pg.state.update(url="https://news.example/news?p=2"), "page address"),
    ("142 points", lambda pg: pg.state.update(doc=2000.25), "document"),
    ("5 replies", lambda pg: pg.el("2")["attrs"].update(id="replies-42") or pg.el("2").update(text="6 replies"),
     "name"),
    ("5 replies", lambda pg: pg.el("2").update(text="5 replies hidden"), "name"),
    ("example.com", lambda pg: pg.el("1").update(tag="button"), "role"),
])
def test_real_scripts_still_refuse_a_changed_control(tmp_path: Path, label: str, mutate: Any, what: str) -> None:
    said, page = node_click(tmp_path, label, mutate)
    assert said.startswith(bl.STALE_PREFIX) and what in said, said
    assert page.clicks == []
