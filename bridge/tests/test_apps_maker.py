"""apps_maker: eve builds small apps ("make me a habit tracker") that open inside Telegram and keep their data.

Everything here runs offline: Claude is a scripted fake and the phone check is a fake, except the one test that
runs the prompt's own reference skeleton through the real headless check.
"""

from __future__ import annotations

import asyncio
import datetime as dt
import json
import os
import re
import time
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import httpx
import pytest
from initdata import sign_init_data

from eve_bridge import apps_maker
from eve_bridge.app_check import CheckReport
from eve_bridge.apps_maker import (
    MAKER_PROMPT,
    MAX_VERSIONS,
    SKELETON,
    AppMaker,
    AppStore,
    ChatMaker,
    Generated,
    apply_edits,
    claude_generate,
    data_sample,
    extract_html,
    owner_context,
    serve_app_html,
    slugify,
    static_issues,
)
from eve_bridge.miniapp import MiniAppConfig, MiniAppServer, SpendLedger, cost_usd

TOKEN = "123456:TEST-token"
OWNER = 4242
DOC = ("<!doctype html><html><head><title>Habit Tracker</title><meta name=\"eve-icon\" content=\"✅\">"
       "<meta content=\"Check in daily and keep streaks.\" name=\"description\"></head><body>hi<script>"
       "eve.load().then(render); function add() { eve.save(state); }</script></body></html>")
USAGE = {"input_tokens": 2000, "output_tokens": 10000}


def signed(user_id: int = OWNER) -> str:
    return sign_init_data({"auth_date": str(int(time.time())), "user": json.dumps({"id": user_id})}, TOKEN)


def doc(marker: str, title: str = "Habit Tracker") -> str:
    return DOC.replace("hi", marker).replace("Habit Tracker", title)


# ---- pieces -------------------------------------------------------------------------------------

def test_slugs_are_short_and_safe() -> None:
    assert slugify("Habit Tracker!") == "habit-tracker" and slugify("../../etc") == "etc" and slugify("") == "app"


@pytest.mark.parametrize("text,ok", [
    (f"Here you go:\n```html\n{DOC}\n```\nEnjoy", True),
    (DOC, True),
    ("```html\n<!doctype html><html><head>", False),          # cut off: no </html>
    ("Sorry, I can't.", False),
])
def test_only_a_complete_document_counts(text: str, ok: bool) -> None:
    assert (extract_html(text) is not None) is ok


def test_served_apps_get_telegram_and_window_eve_first() -> None:
    out = serve_app_html(DOC)
    assert out.index('src="/eve.js"') < out.index("<title>") and "telegram-web-app.js" in out


def test_static_checks_hold_the_storage_contract_and_the_allowed_hosts() -> None:
    assert static_issues(DOC) == []
    bare = "<html><head></head><body><script>render()</script></body></html>"
    found = " ".join(static_issues(bare))
    assert "<title>" in found and "eve.load()" in found and "eve.save()" in found
    loads = DOC.replace("hi", '<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter">'
                              '<script>fetch("https://api.example.com/x"); new XMLHttpRequest().open("GET", '
                              '"//evil.test/a")</script>')
    hosts = [re.search(r"loads from (\S+);", i).group(1) for i in static_issues(loads)]
    assert hosts == ["api.example.com", "evil.test", "fonts.googleapis.com"]
    fine = DOC.replace("hi", '<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.1/dist/chart.umd.min.js"></script>'
                             '<script src="https://telegram.org/js/telegram-web-app.js"></script>'
                             '<svg xmlns="http://www.w3.org/2000/svg"></svg><input placeholder="https://example.com">'
                             '<a href="https://example.com">x</a><script>tg.openLink("https://example.com")</script>')
    assert static_issues(fine) == []


def test_owner_context_has_the_day_the_time_and_a_units_guess() -> None:
    now = dt.datetime(2026, 9, 24, 21, 40, tzinfo=dt.timezone.utc)
    us = owner_context({"EVE_TIMEZONE": "America/New_York", "EVE_LOCATION": "Portland, OR, USA"}, now=now)
    assert "Thursday, 24 September 2026 (2026-09-24)" in us and "17:40" in us and "America/New_York" in us
    assert "Portland" in us and "miles" in us and "Sunday" in us
    uk = owner_context({"EVE_TIMEZONE": "Europe/London", "EVE_LOCATION": "Leeds, UK"}, now=now)
    assert "22:40" in uk and "£" in uk and "Monday" in uk
    anywhere = owner_context({"EVE_TIMEZONE": "Not/AZone"}, now=now)
    assert "2026-09-24" in anywhere and "No location is configured" in anywhere
    # the phone's locale formats dates and numbers; a format named here would be hard-coded into the app
    for ctx in (us, uk, anywhere, owner_context({"EVE_LOCATION": "Pune, India"}, now=now)):
        assert not re.search(r"first dates|hour clock|digit grouping", ctx), ctx


def test_a_data_sample_keeps_the_shape_and_shortens_the_bulk() -> None:
    small = {"v": 1, "habits": [{"name": "Read"}]}
    assert json.loads(data_sample(small)) == small
    big = {"v": 2, "log": [{"day": f"2026-01-{i:02d}", "note": "x" * 300} for i in range(1, 29)] * 20}
    out = data_sample(big)
    assert len(out) <= apps_maker.DATA_SAMPLE_CHARS + 10 and "more items" in out and '"v": 2' in out


# ---- the prompt ---------------------------------------------------------------------------------

def test_the_prompt_carries_the_contract_the_runtime_and_the_skeleton() -> None:
    for must in ('name="eve-icon"', 'name="description"', "<title>", "eve.load()", "eve.save(value)",
                 "cdn.jsdelivr.net", "MainButton", "BackButton", "HapticFeedback", "showConfirm", "isVersionAtLeast",
                 "--tg-safe-area-inset", "--tg-content-safe-area-inset", "disableVerticalSwipes", "themeChanged",
                 "crypto.randomUUID", "migrate", "WHOLE updated file"):
        assert must in MAKER_PROMPT, must
    assert SKELETON in MAKER_PROMPT


def test_the_prompt_never_names_a_storage_or_date_api_it_must_not_pick() -> None:
    # A name in a system prompt is a candidate for the model's output, even inside a "don't": the prompt says
    # what to use, and this proves the names it must not reach for are absent.
    banned = ["localStorage", "sessionStorage", "indexedDB", "toISOString", "document.cookie",
              # a locale tag or a length attribute named anywhere is one the model hard-codes into every app
              "en-GB", "en-US", "en-IN", "de-DE", "maxlength"]
    assert [b for b in banned if b in MAKER_PROMPT] == []
    assert "eve.save" in MAKER_PROMPT and "getFullYear" in MAKER_PROMPT   # the checker finds what is there


def test_the_reference_skeleton_itself_passes_the_phone_check() -> None:
    pytest.importorskip("playwright.async_api")
    from eve_bridge.app_check import check_app

    r = asyncio.run(check_app(SKELETON))
    assert r.skipped == "" and r.ok, r.issues
    assert static_issues(SKELETON) == [] and r.saves >= 2 and r.screenshot


def test_the_skeleton_never_saves_over_data_it_cannot_read() -> None:
    pytest.importorskip("playwright.async_api")
    from eve_bridge.app_check import check_app

    newer = {"v": 9, "cups": [{"at": 1}]}
    r = asyncio.run(check_app(SKELETON, seed=newer))
    assert r.saved == newer and r.saves == 0                              # the owner's data, untouched
    assert any("could not read that data" in i for i in r.issues), r.issues


def test_eve_js_sends_one_save_at_a_time_and_the_newest_value_wins() -> None:
    pytest.importorskip("playwright.async_api")
    from playwright.async_api import async_playwright

    from eve_bridge.apps_maker import EVE_JS

    harness = """window.sent = []; window.inFlight = 0; window.most = 0;
    window.fetch = (path, init) => { inFlight++; most = Math.max(most, inFlight);
      sent.push({ path, keepalive: init.keepalive, body: JSON.parse(init.body) });
      return new Promise((done) => setTimeout(() => { inFlight--;
        done({ ok: true, json: async () => ({ ok: true, data: { n: 1 } }) }); }, 120)); }; true"""

    async def go():
        async with async_playwright() as p:
            b = await p.chromium.launch()
            page = await b.new_page()
            await page.set_content("<html><body></body></html>")
            await page.evaluate(harness)
            await page.add_script_tag(content=EVE_JS)
            out = await page.evaluate("""async () => {
              const s = { n: 1 };
              const all = [eve.save(s)]; s.n = 2; all.push(eve.save(s)); s.n = 3; all.push(eve.save(s));
              const results = await Promise.all(all);
              const loaded = await eve.load();
              return { results, loaded, sent, most };
            }""")
            await b.close()
            return out

    out = asyncio.run(go())
    saves = [s for s in out["sent"] if s["path"].endswith("/save")]
    assert out["most"] == 1                                               # never two requests at once
    assert [s["body"]["data"] for s in saves] == [{"n": 1}, {"n": 3}]     # the middle one folded into the newest
    assert out["results"] == [True, True, True] and all(s["keepalive"] for s in saves)
    assert out["loaded"] == {"n": 1} and "t" in saves[0]["body"] and "initData" not in saves[0]["body"]


# ---- the store ----------------------------------------------------------------------------------

def test_store_saves_lists_finds_and_describes(tmp_path: Path) -> None:
    store = AppStore(tmp_path)
    a = store.save(DOC, "a habit tracker")
    assert (a.slug, a.title, a.icon, a.description, a.versions) == (
        "habit-tracker", "Habit Tracker", "✅", "Check in daily and keep streaks.", 0)
    b = store.save(DOC, "another")                               # same title: a second app, not an overwrite
    assert b.slug == "habit-tracker-2"
    edited = store.save(doc("v2"), "add streaks", slug=a.slug)
    assert edited.slug == a.slug and edited.created == a.created and edited.versions == 1
    assert "v2" in store.html(a.slug)
    assert [r["text"] for r in store.requests(a.slug)] == ["a habit tracker", "add streaks"]
    assert store.find("my habit tracker").slug.startswith("habit-tracker") and store.find("zebra") is None
    assert store.find("Habit Tracker").slug == "habit-tracker"
    assert {x.slug for x in store.list()} == {"habit-tracker", "habit-tracker-2"}
    assert set(a.as_dict()) == {"slug", "title", "created", "updated", "icon", "description", "versions",
                                "undo_warning"}


def test_versions_are_capped_and_undo_pops_the_newest(tmp_path: Path) -> None:
    store = AppStore(tmp_path)
    slug = store.save(doc("v0"), "first").slug
    for i in range(1, MAX_VERSIONS + 3):
        store.save(doc(f"v{i}"), f"change {i}", slug=slug)
    assert store.info(slug).versions == MAX_VERSIONS
    back = store.revert(slug)
    assert f"v{MAX_VERSIONS + 1}" in store.html(slug) and back.versions == MAX_VERSIONS - 1
    for _ in range(MAX_VERSIONS - 1):
        store.revert(slug)
    assert "v2" in store.html(slug)                              # v0 and v1 fell off the end
    with pytest.raises(KeyError):
        store.revert(slug)


def test_undo_restores_the_old_title_and_icon_unless_renamed(tmp_path: Path) -> None:
    store = AppStore(tmp_path)
    slug = store.save(DOC, "x").slug
    store.save(doc("v2", title="Streaks").replace("✅", "🔥"), "rename via code", slug=slug)
    assert (store.info(slug).title, store.info(slug).icon) == ("Streaks", "🔥")
    back = store.revert(slug)
    assert (back.title, back.icon) == ("Habit Tracker", "✅")


def test_a_renamed_app_keeps_its_name_through_changes(tmp_path: Path) -> None:
    store = AppStore(tmp_path)
    slug = store.save(DOC, "x").slug
    assert store.rename(slug, "  Daily   habits ").title == "Daily habits"
    store.save(doc("v2", title="Something Else"), "change", slug=slug)
    assert store.info(slug).title == "Daily habits" and store.info(slug).slug == slug
    with pytest.raises(ValueError):
        store.rename(slug, "   ")
    with pytest.raises(KeyError):
        store.rename("no-such-app", "x")


def test_delete_moves_the_app_to_the_trash(tmp_path: Path) -> None:
    store = AppStore(tmp_path)
    slug = store.save(DOC, "x").slug
    store.save_data(slug, {"v": 1})
    store.delete(slug)
    assert store.list() == [] and store.find("habit tracker") is None and store.info(slug) is None
    trashed = list((tmp_path / ".trash").iterdir())
    assert len(trashed) == 1 and trashed[0].name.startswith(slug) and (trashed[0] / "data.json").exists()
    with pytest.raises(KeyError):
        store.load_data(slug)
    with pytest.raises(KeyError):
        store.delete(slug)
    again = store.save(DOC, "x")                                 # the name is free again
    assert again.slug == slug and store.load_data(slug) is None


def test_apps_from_before_versions_keep_their_previous_page_and_request(tmp_path: Path) -> None:
    d = tmp_path / "old-app"
    d.mkdir()
    (d / "index.html").write_text(doc("now"))
    (d / "index.prev.html").write_text(doc("before"))
    (d / "meta.json").write_text(json.dumps({"title": "Old App", "created": "2026-09-24T10:00:00",
                                             "updated": "2026-09-24T11:00:00", "request": "an old app"}))
    store = AppStore(tmp_path)
    assert store.info("old-app").versions == 1 and store.requests("old-app")[0]["text"] == "an old app"
    store.revert("old-app")
    assert "before" in store.html("old-app") and not (d / "index.prev.html").exists()


@pytest.mark.parametrize("bad", ["../secrets", "a/b", "Habit", "habit\n", ".trash", "", "-x", "a" * 49, "..", "/etc"])
def test_no_path_leaves_the_apps_folder(tmp_path: Path, bad: str) -> None:
    store = AppStore(tmp_path / "apps")
    (tmp_path / "apps").mkdir()
    (tmp_path / "secrets").mkdir()
    (tmp_path / "secrets" / "index.html").write_text("secret")
    assert store.info(bad) is None and store.html(bad) is None and store.requests(bad) == []
    for fn in (store.load_data, store.revert, store.delete):
        with pytest.raises(KeyError):
            fn(bad)
    with pytest.raises(KeyError):
        store.save_data(bad, 1)
    with pytest.raises(KeyError):
        store.rename(bad, "x")
    if bad:                                                      # "" means a new app, named by its title
        with pytest.raises(ValueError):
            store.save(DOC, "x", slug=bad)
    assert (tmp_path / "secrets" / "index.html").read_text() == "secret"


def test_a_symlinked_app_folder_is_refused(tmp_path: Path) -> None:
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "index.html").write_text(DOC)
    (outside / "meta.json").write_text(json.dumps({"title": "x", "created": "a", "updated": "b"}))
    (tmp_path / "apps").mkdir()
    os.symlink(outside, tmp_path / "apps" / "sneaky")
    store = AppStore(tmp_path / "apps")
    assert store.info("sneaky") is None and store.list() == []
    with pytest.raises(KeyError):
        store.delete("sneaky")
    assert (outside / "index.html").exists()


def test_data_round_trips_and_is_bounded(tmp_path: Path) -> None:
    store = AppStore(tmp_path)
    slug = store.save(DOC, "x").slug
    assert store.load_data(slug) is None
    store.save_data(slug, {"habits": ["run"], "done": {"2026-09-24": ["run"]}})
    assert store.load_data(slug)["habits"] == ["run"]
    with pytest.raises(ValueError):
        store.save_data(slug, "x" * 1_100_000)
    with pytest.raises(KeyError):
        store.load_data("no-such-app")


# ---- the maker ----------------------------------------------------------------------------------

class Script:
    """A fake Claude: answers from a list, one per call, and records every call's messages."""

    def __init__(self, *answers: Any) -> None:
        self.answers = list(answers)
        self.calls: list[list[dict[str, Any]]] = []

    async def __call__(self, messages, progress) -> Generated:
        self.calls.append(list(messages))
        a = self.answers.pop(0) if len(self.answers) > 1 else self.answers[0]
        if isinstance(a, Exception):
            raise a
        text, stop = a if isinstance(a, tuple) else (a, "end_turn")
        progress("thinking", 0)
        progress("writing", len(text))
        return Generated(text, dict(USAGE), stop, content=[SimpleNamespace(type="text", text=text, n=len(self.calls))])


def fenced(html: str) -> str:
    return f"Here it is.\n```html\n{html}\n```"


class FakeCheck:
    """The phone check, faked: the issues for a page are keyed by a marker in it."""

    def __init__(self, issues: dict[str, list[str]] | None = None) -> None:
        self.issues = issues or {}
        self.seeds: list[Any] = []

    async def __call__(self, html: str, *, seed: Any = None) -> CheckReport:
        self.seeds.append(seed)
        found = next((v for k, v in self.issues.items() if k in html), [])
        return CheckReport(ok=not found, issues=list(found), taps=3, fields=1, saves=2, screenshot=b"\xff\xd8jpeg")


def maker_for(tmp_path: Path, generate, check: Any = None, **kw):
    ledger = SpendLedger(tmp_path / "spend.json", 0)
    store = AppStore(tmp_path / "apps")
    maker = AppMaker(store, generate, cost=lambda u: cost_usd("claude-opus-5-5", u), spend=ledger.add,
                     context=lambda: "Today is Thursday.", check=check if check is not None else FakeCheck(), **kw)
    return maker, ledger, store


ONE = cost_usd("claude-opus-5-5", USAGE)


def test_making_an_app_saves_it_bills_it_and_reports_each_stage(tmp_path: Path) -> None:
    gen = Script(fenced(DOC))
    maker, ledger, store = maker_for(tmp_path, gen)
    stages: list[tuple[str, int]] = []
    made = asyncio.run(maker.make("a habit tracker with streaks", lambda s, n: stages.append((s, n))))
    assert made.ok and made.app.slug == "habit-tracker" and store.html("habit-tracker") == DOC
    first = gen.calls[0][0]["content"]
    assert "habit tracker with streaks" in first and "<owner_context>\nToday is Thursday." in first
    assert ledger.spent() == pytest.approx(ONE) and made.usd == pytest.approx(ONE)
    assert made.rounds == 0 and made.check.ok and made.issues == [] and made.app.icon == "✅"
    assert [s for s, _ in stages] == ["thinking", "writing", "testing", "saving"]


def test_a_problem_found_on_the_phone_goes_back_for_one_repair(tmp_path: Path) -> None:
    bad, good = doc("v1"), doc("v2")
    gen = Script(fenced(bad), fenced(good))
    check = FakeCheck({"v1": ['Uncaught error while tapping "Add": x is not defined (line 12)']})
    maker, ledger, store = maker_for(tmp_path, gen, check)
    stages: list[tuple[str, int]] = []
    made = asyncio.run(maker.make("habit tracker", lambda s, n: stages.append((s, n))))
    assert made.ok and made.rounds == 1 and "v2" in store.html(made.app.slug) and made.check.ok
    second = gen.calls[1]
    assert [m["role"] for m in second] == ["user", "assistant", "user"]
    assert second[0] is gen.calls[0][0]                                   # the first turn, untouched
    assert second[1]["content"][0].n == 1                                 # Claude's own content, unchanged
    assert 'tapping "Add"' in second[2]["content"] and "whole fixed file" in second[2]["content"]
    assert ("fixing", 1) in stages and stages.count(("fixing", 1)) >= 1
    assert ledger.spent() == pytest.approx(2 * ONE) and made.usd == pytest.approx(2 * ONE)
    assert store.info(made.app.slug).versions == 0                        # the broken draft is not a version


def test_a_repair_answered_with_edits_changes_only_those_lines(tmp_path: Path) -> None:
    bad = doc("v1")
    edit = "Fixed.\n<<<<<<< FIND\nv1<script>\n=======\nv2<script>\n>>>>>>> REPLACE\n"
    gen = Script(fenced(bad), edit)
    check = FakeCheck({"v1": ["Uncaught error while tapping \"Add\""]})
    maker, _, store = maker_for(tmp_path, gen, check)
    made = asyncio.run(maker.make("habit tracker"))
    assert made.ok and made.rounds == 1 and store.html(made.app.slug) == bad.replace("v1<script>", "v2<script>")
    assert "<<<<<<< FIND" in gen.calls[1][2]["content"] and "whole fixed file" in gen.calls[1][2]["content"]
    assert made.history == [['Uncaught error while tapping "Add"'], []]


def test_an_edit_that_does_not_fit_the_file_is_sent_back_with_the_reason(tmp_path: Path) -> None:
    gen = Script(fenced(doc("v1")), "<<<<<<< FIND\nnot in the file\n=======\nx\n>>>>>>> REPLACE", fenced(doc("v3")))
    maker, _, store = maker_for(tmp_path, gen, FakeCheck({"v1": ["broken"]}))
    made = asyncio.run(maker.make("habit tracker"))
    assert made.ok and "v3" in store.html(made.app.slug) and len(gen.calls) == 3
    assert "Edit 1 of 1 could not be applied" in gen.calls[2][4]["content"]
    assert "tapped every button" not in gen.calls[2][4]["content"]        # nothing new was tested


@pytest.mark.parametrize("answer,expect", [
    ("<<<<<<< FIND\n<b>a</b>\n=======\n<b>z</b>\n>>>>>>> REPLACE", "<b>z</b>"),
    # the same lines with other indentation still fit, when they fit one place only
    ("<<<<<<< FIND\n<b>a</b>\n    <i>b</i>\n=======\n<i>y</i>\n>>>>>>> REPLACE", "<i>y</i>"),
    # an empty replacement deletes; two blocks apply in order
    ("<<<<<<< FIND\n<i>b</i>\n=======\n>>>>>>> REPLACE\n<<<<<<< FIND\n<b>a</b>\n=======\n<p>c</p>\n>>>>>>> REPLACE",
     "<p>c</p>"),
])
def test_edits_apply_to_the_file(answer: str, expect: str) -> None:
    base = "<!doctype html><html><body>\n  <b>a</b>\n  <i>b</i>\n</body></html>"
    out, why = apply_edits(base, answer)
    assert why == "" and out is not None and expect in out and out.endswith("</html>")


@pytest.mark.parametrize("answer,why", [
    ("<<<<<<< FIND\n<u>x</u>\n=======\ny\n>>>>>>> REPLACE", "occur 0 times"),
    ("<<<<<<< FIND\n<s>d</s>\n=======\ny\n>>>>>>> REPLACE", "occur 2 times"),
    ("<<<<<<< FIND\n</html>\n=======\n\n>>>>>>> REPLACE", "no longer one complete HTML document"),
])
def test_edits_that_cannot_apply_say_why(answer: str, why: str) -> None:
    out, reason = apply_edits("<!doctype html><html><body><s>d</s><s>d</s>\n</html>", answer)
    assert out is None and why in reason


def test_an_answer_without_edits_is_not_an_edit() -> None:
    assert apply_edits("<html></html>", fenced(DOC)) == (None, "")


def test_repairs_that_do_not_converge_keep_the_best_version(tmp_path: Path) -> None:
    gen = Script(fenced(doc("v1")), fenced(doc("v2")), fenced(doc("v3")))
    check = FakeCheck({"v1": ["a", "b", "c"], "v2": ["only b"], "v3": ["b", "d"]})
    maker, ledger, store = maker_for(tmp_path, gen, check)
    made = asyncio.run(maker.make("habit tracker"))
    assert made.ok and made.rounds == 2 and len(gen.calls) == 3
    assert "v2" in store.html(made.app.slug) and made.issues == ["only b"] and made.check.issues == ["only b"]
    assert ledger.spent() == pytest.approx(3 * ONE)


def test_a_static_problem_is_repaired_even_when_the_phone_check_passes(tmp_path: Path) -> None:
    never_saves = DOC.replace("eve.save(state)", "render()")
    gen = Script(fenced(never_saves), fenced(DOC))
    maker, _, store = maker_for(tmp_path, gen)
    made = asyncio.run(maker.make("habit tracker"))
    assert made.ok and made.rounds == 1 and "eve.save()" in gen.calls[1][2]["content"]
    assert store.html(made.app.slug) == DOC


def test_an_answer_without_a_file_is_asked_for_again(tmp_path: Path) -> None:
    gen = Script(("```html\n<!doctype html><html><head>", "max_tokens"), fenced(DOC))
    maker, _, store = maker_for(tmp_path, gen)
    made = asyncio.run(maker.make("habit tracker"))
    assert made.ok and made.rounds == 1 and "cut off" in gen.calls[1][2]["content"]
    assert "tapped every button" not in gen.calls[1][2]["content"]       # nothing was tested yet
    assert "<<<<<<< FIND" not in gen.calls[1][2]["content"]             # no file to edit: the whole file again


@pytest.mark.parametrize("answer,why", [(("```html\n<html><body>", "max_tokens"), "too long"),
                                        (("no.", "refusal"), "declined"), (("just words", "end_turn"), "complete")])
def test_a_build_that_never_produces_a_file_saves_nothing_and_says_why(tmp_path: Path, answer, why: str) -> None:
    gen = Script(answer)
    maker, ledger, store = maker_for(tmp_path, gen)
    made = asyncio.run(maker.make("anything"))
    assert not made.ok and why in made.reason and store.list() == [] and ledger.spent() > 0
    assert len(gen.calls) == (1 if why == "declined" else 3)


def test_a_failed_repair_round_keeps_the_first_version(tmp_path: Path) -> None:
    gen = Script(fenced(doc("v1")), RuntimeError("overloaded"))
    maker, _, store = maker_for(tmp_path, gen, FakeCheck({"v1": ["small thing"]}))
    made = asyncio.run(maker.make("habit tracker"))
    assert made.ok and "v1" in store.html(made.app.slug) and made.issues == ["small thing"]


def test_a_failure_before_any_version_is_raised(tmp_path: Path) -> None:
    maker, _, _ = maker_for(tmp_path, Script(RuntimeError("down")))
    with pytest.raises(RuntimeError):
        asyncio.run(maker.make("x"))


def test_the_check_can_be_off(tmp_path: Path) -> None:
    gen = Script(fenced(DOC))
    ledger = SpendLedger(tmp_path / "spend.json", 0)
    maker = AppMaker(AppStore(tmp_path / "apps"), gen, cost=lambda u: 0.0, spend=ledger.add, context=lambda: "",
                     check=None)
    made = asyncio.run(maker.make("x"))
    assert made.ok and made.check is None and made.rounds == 0


def test_changing_an_app_gives_claude_the_file_the_data_and_the_history(tmp_path: Path) -> None:
    gen = Script(fenced(DOC), fenced(doc("v2")))
    check = FakeCheck()
    maker, _, store = maker_for(tmp_path, gen, check)
    asyncio.run(maker.make("habit tracker"))
    store.save_data("habit-tracker", {"v": 1, "habits": [{"name": "Stretch", "days": ["2026-09-23"]}]})
    made = asyncio.run(maker.edit("the habit tracker", "add a weekly chart"))
    prompt = gen.calls[1][0]["content"]
    assert made.ok and made.app.slug == "habit-tracker" and made.app.versions == 1
    assert DOC in prompt and "add a weekly chart" in prompt and '"Habit Tracker"' in prompt
    assert '"Stretch"' in prompt and "- " in prompt and ": habit tracker" in prompt
    assert "whole updated file" in prompt and "<owner_context>" in prompt
    assert check.seeds[-1] == {"v": 1, "habits": [{"name": "Stretch", "days": ["2026-09-23"]}]}
    assert not asyncio.run(maker.edit("zebra planner", "x")).ok


def test_an_app_deleted_while_it_was_being_changed_is_not_brought_back(tmp_path: Path) -> None:
    maker, _, store = maker_for(tmp_path, Script(fenced(DOC)))
    asyncio.run(maker.make("habit tracker"))

    async def slow(messages, progress):
        store.delete("habit-tracker")
        return Generated(fenced(doc("v2")), dict(USAGE), "end_turn", [])

    maker._generate = slow
    made = asyncio.run(maker.edit("habit tracker", "x"))
    assert not made.ok and "deleted" in made.reason and store.list() == []


# ---- the real generate, against a fake SDK client ------------------------------------------------

class FakeStream:
    def __init__(self, events, final) -> None:
        self.events, self.final = events, final

    async def __aenter__(self):
        return self

    async def __aexit__(self, *a):
        return False

    def __aiter__(self):
        async def gen():
            for e in self.events:
                yield e
        return gen()

    async def get_final_message(self):
        return self.final


def test_claude_generate_streams_stages_and_caches_the_prefix() -> None:
    calls: list[dict[str, Any]] = []
    content = [SimpleNamespace(type="thinking", thinking="", signature="sig"), SimpleNamespace(type="text", text="ab")]
    final = SimpleNamespace(usage={"input_tokens": 1}, stop_reason="end_turn", content=content)
    events = [SimpleNamespace(type="content_block_start", content_block=SimpleNamespace(type="thinking")),
              SimpleNamespace(type="text", text="a"), SimpleNamespace(type="text", text="b"),
              SimpleNamespace(type="message_stop")]
    client = SimpleNamespace(messages=SimpleNamespace(stream=lambda **kw: calls.append(kw) or FakeStream(events, final)))
    stages: list[tuple[str, int]] = []
    gen = claude_generate("claude-opus-5-5", "high", client=client)
    out = asyncio.run(gen([{"role": "user", "content": "x"}], lambda s, n: stages.append((s, n))))
    assert (out.text, out.stop_reason, out.content) == ("ab", "end_turn", content)
    kw = calls[0]
    assert kw["model"] == "claude-opus-5-5" and kw["output_config"] == {"effort": "high"}
    assert kw["thinking"] == {"type": "adaptive"} and kw["cache_control"] == {"type": "ephemeral"}
    assert kw["system"][0]["text"] == MAKER_PROMPT
    assert kw["system"][0]["cache_control"] == {"type": "ephemeral", "ttl": "1h"}     # outlives a long first round
    assert stages == [("thinking", 0), ("thinking", 0), ("writing", 1), ("writing", 2)]


# ---- the server's app routes (the /api/make stream is miniapp's, tested in test_miniapp.py) ------

def server_for(tmp_path: Path, generate=None):
    cfg = MiniAppConfig(enabled=True, token=TOKEN, owner_ids=frozenset({OWNER}), ledger_path=tmp_path / "spend.json")
    maker, ledger, store = maker_for(tmp_path, generate or Script(fenced(DOC)))
    return MiniAppServer(cfg, ledger, page=b"home", store=store, maker=maker), store


async def call(port: int, method: str, path: str, body: dict | None = None) -> httpx.Response:
    async with httpx.AsyncClient(base_url=f"http://127.0.0.1:{port}", timeout=10) as c:
        return await (c.get(path) if method == "GET" else c.post(path, json=body or {}))


def test_an_app_is_served_with_window_eve_and_keeps_its_data(tmp_path: Path) -> None:
    async def go():
        srv, store = server_for(tmp_path)
        store.save(DOC, "habit tracker")
        port = await srv.start()
        try:
            listed = await call(port, "POST", "/api/apps", {"initData": signed()})
            page = await call(port, "GET", "/" + listed.json()["apps"][0]["open"])
            js = await call(port, "GET", "/eve.js")
            saved = await call(port, "POST", "/api/apps/habit-tracker/save",
                               {"initData": signed(), "data": {"habits": ["read"]}})
            loaded = await call(port, "POST", "/api/apps/habit-tracker/load", {"initData": signed()})
            missing = await call(port, "GET", "/apps/nope/")
            return listed, page, js, saved, loaded, missing
        finally:
            await srv.close()

    listed, page, js, saved, loaded, missing = asyncio.run(go())
    assert [a["slug"] for a in listed.json()["apps"]] == ["habit-tracker"]
    assert page.status_code == 200 and "/eve.js" in page.text and "window.eve" in js.text
    assert saved.json()["ok"] and loaded.json()["data"] == {"habits": ["read"]}
    assert missing.status_code == 302 and missing.headers["location"] == "/?open=nope"


def test_the_app_api_is_owner_only(tmp_path: Path) -> None:
    gen = Script(fenced(DOC))

    async def go():
        srv, store = server_for(tmp_path, gen)
        store.save(DOC, "x")
        port = await srv.start()
        try:
            return [await call(port, "POST", p, {"initData": d, "request": "x", "data": 1})
                    for p in ("/api/make", "/api/apps", "/api/apps/habit-tracker/load", "/api/apps/habit-tracker/save")
                    for d in ("", signed(999))], store.load_data("habit-tracker")
        finally:
            await srv.close()

    responses, data = asyncio.run(go())
    assert all(r.status_code == 403 for r in responses) and gen.calls == [] and data is None


# ---- the chat door ------------------------------------------------------------------------------

class FakeMiniApp:
    def __init__(self, maker, store, url="https://quiet-owl.trycloudflare.com") -> None:
        self.maker, self.store, self.url = maker, store, url

    def app_url(self, slug: str) -> str:
        return f"{self.url}/apps/{slug}/" if self.url else ""

    def chat_url(self, slug: str) -> str:
        return f"{self.url}/?chat={slug}" if self.url else ""


class Chat:
    def __init__(self, photo_fails: bool = False) -> None:
        self.sent: list[tuple[int, str, list]] = []
        self.photos: list[tuple[int, bytes, str, list]] = []
        self.said: list[str] = []
        self.photo_fails = photo_fails

    async def send(self, chat, text, buttons):
        self.sent.append((chat, text, buttons))

    async def say(self, chat, text):
        self.said.append(text)

    async def send_photo(self, chat, jpeg, caption, buttons):
        if self.photo_fails:
            raise RuntimeError("telegram said no")
        self.photos.append((chat, jpeg, caption, buttons))


def door_for(tmp_path: Path, generate=None, url="https://quiet-owl.trycloudflare.com"):
    maker, _, store = maker_for(tmp_path, generate or Script(fenced(DOC)))
    spawned: list = []
    door = ChatMaker(FakeMiniApp(maker, store, url), lambda coro, name: spawned.append(asyncio.ensure_future(coro)))
    return door, store, spawned


OPEN = ("Open Habit Tracker", "https://quiet-owl.trycloudflare.com/apps/habit-tracker/")
CHANGE = ("Change", "https://quiet-owl.trycloudflare.com/?chat=habit-tracker")


def test_a_build_from_the_chat_arrives_as_the_apps_picture_with_an_open_button(tmp_path: Path) -> None:
    chat = Chat()

    async def go():
        door, store, spawned = door_for(tmp_path)
        first = await door.handle("make_app", {"request": "habit tracker"}, 7, chat.send, chat.say, chat.send_photo)
        await asyncio.gather(*spawned)
        return first

    first = asyncio.run(go())
    assert first["ok"] and chat.sent == [] and chat.said == ["Written. Testing it on a phone…"]
    assert chat.photos == [(7, b"\xff\xd8jpeg", "✅ Habit Tracker is ready.\nCheck in daily and keep streaks.\n"
                                                "(Picture: a test run with sample entries.)", [OPEN])]


@pytest.mark.parametrize("photo", ["none", "fails"])
def test_without_a_picture_the_button_still_arrives(tmp_path: Path, photo: str) -> None:
    chat = Chat(photo_fails=photo == "fails")

    async def go():
        door, store, spawned = door_for(tmp_path)
        await door.handle("make_app", {"request": "habit tracker"}, 7, chat.send, chat.say,
                          chat.send_photo if photo == "fails" else None)
        await asyncio.gather(*spawned)

    asyncio.run(go())
    assert chat.sent == [(7, "✅ Habit Tracker is ready.\nCheck in daily and keep streaks.", [OPEN])]


def test_the_chat_door_changes_undoes_renames_deletes_and_lists(tmp_path: Path) -> None:
    chat = Chat()

    async def go():
        door, store, spawned = door_for(tmp_path, Script(fenced(DOC), fenced(doc("v2"))))
        out = {}
        out["make"] = await door.handle("make_app", {"request": "habit tracker"}, 7, chat.send, chat.say)
        await asyncio.gather(*spawned)
        out["change"] = await door.handle("change_app", {"app": "habit tracker", "change": "add a chart"}, 7,
                                          chat.send, chat.say)
        await asyncio.gather(*spawned)
        out["v2"] = store.html("habit-tracker")
        out["undo"] = await door.handle("undo_app", {"app": "habit tracker"}, 7, chat.send, chat.say)
        out["undo_again"] = await door.handle("undo_app", {"app": "habit tracker"}, 7, chat.send, chat.say)
        out["rename"] = await door.handle("rename_app", {"app": "habit tracker", "title": "Daily Habits"}, 7,
                                          chat.send, chat.say)
        out["list"] = await door.handle("list_apps", {}, 7, chat.send, chat.say)
        out["unknown"] = await door.handle("delete_app", {"app": "zebra"}, 7, chat.send, chat.say)
        out["delete"] = await door.handle("delete_app", {"app": "daily habits"}, 7, chat.send, chat.say)
        out["after"] = await door.handle("list_apps", {}, 7, chat.send, chat.say)
        return out, store

    out, store = asyncio.run(go())
    assert out["make"]["ok"] and out["change"]["ok"] and "v2" in out["v2"]
    assert chat.sent[1] == (7, "✅ Habit Tracker is updated.\nCheck in daily and keep streaks.", [OPEN, CHANGE])
    assert out["undo"]["ok"] and out["undo"]["earlier_versions_left"] == 0
    assert chat.sent[2][1] == "Habit Tracker is back to how it was before its last change."
    assert not out["undo_again"]["ok"] and "no earlier version" in out["undo_again"]["reason"]
    assert out["rename"]["renamed"] == {"from": "Habit Tracker", "to": "Daily Habits"}
    assert out["list"]["apps"] == ["Daily Habits"] and chat.sent[-1][2][0][0] == "✅ Daily Habits"
    assert not out["unknown"]["ok"] and "Daily Habits" in out["unknown"]["reason"]
    assert out["delete"]["ok"] and out["delete"]["deleted"] == "Daily Habits" and ".trash" in out["delete"]["note"]
    assert out["after"]["apps"] == [] and store.list() == []
    assert chat.said == ["Written. Testing it on a phone…"] * 2               # one line per build, then the result


def test_an_app_being_changed_cannot_be_deleted_under_it(tmp_path: Path) -> None:
    chat = Chat()

    async def go():
        door, store, spawned = door_for(tmp_path)
        store.save(DOC, "x")
        door.building.add("habit-tracker")
        return await door.handle("delete_app", {"app": "habit tracker"}, 7, chat.send, chat.say), store

    out, store = asyncio.run(go())
    assert not out["ok"] and "being changed" in out["reason"] and store.info("habit-tracker") is not None


def test_the_chat_door_offers_nothing_while_the_tunnel_is_down(tmp_path: Path) -> None:
    down, _, _ = door_for(tmp_path, url="")
    up, _, _ = door_for(tmp_path)
    assert down.tools() == [] and {t["name"] for t in up.tools()} == apps_maker.MAKER_TOOL_NAMES
    assert apps_maker.MAKER_TOOL_NAMES == {"make_app", "change_app", "list_apps", "delete_app", "rename_app",
                                           "undo_app", "restore_app"}
    for t in up.tools():                                         # strict tools: every property required
        assert set(t["parameters"]["required"]) == set(t["parameters"]["properties"])


# ---- review fixes: the right app, a safe save, no guesses, a hard gate on outside code -------------

def test_a_slug_names_its_own_app_before_any_title(tmp_path: Path) -> None:
    # Two builds of "Notes" are notes and notes-2, both titled Notes: "notes" is the first, never the newest.
    store = AppStore(tmp_path)
    old = store.save(doc("old", title="Notes"), "notes").slug
    time.sleep(1.1)                                              # the second one is newer
    new = store.save(doc("new", title="Notes"), "notes again").slug
    assert (old, new) == ("notes", "notes-2") and store.list()[0].slug == "notes-2"
    assert store.find("notes").slug == "notes" and store.find("notes-2").slug == "notes-2"
    store.rename("notes-2", "habit-tracker")                     # a title that is another app's slug
    store.save(DOC, "habit tracker")
    assert store.find("habit-tracker").slug == "habit-tracker"


def test_changing_the_older_of_two_same_named_apps_changes_that_one(tmp_path: Path) -> None:
    gen = Script(fenced(doc("changed", title="Notes")))
    maker, _, store = maker_for(tmp_path, gen)
    store.save(doc("old", title="Notes"), "notes")
    time.sleep(1.1)
    store.save(doc("new", title="Notes"), "notes again")
    made = asyncio.run(maker.edit("notes", "bigger text"))           # the slug the Mini App sends
    assert made.ok and made.app.slug == "notes"
    assert "changed" in store.html("notes") and "new" in store.html("notes-2")
    assert "old" in gen.calls[0][0]["content"]                       # Claude was shown the right file


@pytest.mark.parametrize("fails", ["write", "replace"])
def test_a_failed_page_write_leaves_the_app_as_it_was(tmp_path: Path, monkeypatch, fails: str) -> None:
    store = AppStore(tmp_path)
    slug = store.save(doc("v1"), "first").slug
    store.save_data(slug, {"v": 1})
    real_write, real_replace = Path.write_text, Path.replace

    def write(self, *a, **kw):
        if fails == "write" and self.name == "index.tmp":
            raise OSError(28, "No space left on device")
        return real_write(self, *a, **kw)

    def replace(self, target):
        if fails == "replace" and self.name == "index.tmp":
            raise OSError(28, "No space left on device")
        return real_replace(self, target)

    monkeypatch.setattr(Path, "write_text", write)
    monkeypatch.setattr(Path, "replace", replace)
    with pytest.raises(OSError):
        store.save(doc("v2"), "change", slug=slug)
    monkeypatch.undo()
    info = store.info(slug)
    assert info is not None and info.versions == 0 and "v1" in store.html(slug)      # still listed, still opens
    assert [a.slug for a in store.list()] == [slug] and store.load_data(slug) == {"v": 1}
    assert not (tmp_path / slug / "index.tmp").exists()


def test_the_chat_door_never_deletes_undoes_or_renames_on_a_guess(tmp_path: Path) -> None:
    chat = Chat()

    async def go():
        door, store, _ = door_for(tmp_path)
        store.save(doc("a", title="Water Log"), "a")
        store.save(doc("b", title="Water Plants"), "b")
        store.save(doc("c", title="Habit Tracker"), "c")
        store.save(doc("c2", title="Habit Tracker"), "c", slug="habit-tracker")
        out = {name: await door.handle(tool, args, 7, chat.send, chat.say) for name, tool, args in [
            ("both_water", "delete_app", {"app": "the water app"}),
            ("no_such", "delete_app", {"app": "water tracker"}),
            ("undo_no_such", "undo_app", {"app": "the water tracker"}),
            ("rename_vague", "rename_app", {"app": "water", "title": "X"}),
            ("habit_word", "delete_app", {"app": "my habit journal"}),
        ]}
        kept = sorted(a.slug for a in store.list())
        versions = store.info("habit-tracker").versions
        out["sure"] = await door.handle("delete_app", {"app": "the water plants"}, 7, chat.send, chat.say)
        return out, kept, versions, sorted(a.slug for a in store.list())

    out, kept, versions, after = asyncio.run(go())
    assert kept == ["habit-tracker", "water-log", "water-plants"] and versions == 1       # nothing happened
    assert not out["both_water"]["ok"] and set(out["both_water"]["candidates"]) == {"Water Plants", "Water Log"}
    for key in ("no_such", "undo_no_such", "rename_vague", "habit_word"):
        assert not out[key]["ok"] and "nothing was done" in out[key]["reason"], key
    assert out["sure"]["ok"] and after == ["habit-tracker", "water-log"]


def test_same_titled_apps_are_told_apart_by_slug_when_the_owner_is_asked(tmp_path: Path) -> None:
    store = AppStore(tmp_path)
    store.save(doc("a", title="Notes"), "a")
    store.save(doc("b", title="Notes"), "b")
    app, candidates = store.resolve("notes")                     # the slug "notes" is exact: that one
    assert app is not None and app.slug == "notes" and candidates == []
    app, candidates = store.resolve("Notes ")
    assert app is not None and app.slug == "notes"
    store.rename("notes", "Journal")
    store.rename("notes-2", "Journal")
    app, candidates = store.resolve("journal")
    assert app is None and {c.slug for c in candidates} == {"notes", "notes-2"}


def test_a_deleted_app_comes_back_with_its_data(tmp_path: Path) -> None:
    store = AppStore(tmp_path)
    slug = store.save(DOC, "x").slug
    store.save_data(slug, {"v": 1, "habits": ["read"]})
    store.delete(slug)
    assert [a.title for a in store.trashed()] == ["Habit Tracker"] and store.info(slug) is None
    back = store.restore(slug)
    assert back.slug == slug and store.load_data(slug) == {"v": 1, "habits": ["read"]} and store.trashed() == []
    store.delete(slug)
    store.save(doc("new"), "a new one with the same name")       # the address is taken again
    with pytest.raises(FileExistsError):
        store.restore(slug)
    with pytest.raises(KeyError):
        store.restore("never-was")
    with pytest.raises(KeyError):
        store.restore("../x")


def test_restore_from_the_chat(tmp_path: Path) -> None:
    chat = Chat()

    async def go():
        door, store, _ = door_for(tmp_path)
        store.save(DOC, "x")
        await door.handle("delete_app", {"app": "habit tracker"}, 7, chat.send, chat.say)
        missing = await door.handle("restore_app", {"app": "zebra"}, 7, chat.send, chat.say)
        back = await door.handle("restore_app", {"app": "the habit tracker"}, 7, chat.send, chat.say)
        return missing, back, store

    missing, back, store = asyncio.run(go())
    assert not missing["ok"] and "Habit Tracker" in missing["reason"]
    assert back["ok"] and back["restored"] == "Habit Tracker" and store.info("habit-tracker") is not None
    assert chat.sent[-1] == (7, "✅ Habit Tracker is back, with its data.", [OPEN])


def test_undo_warns_when_the_data_has_moved_on_since_that_version(tmp_path: Path) -> None:
    store = AppStore(tmp_path)
    slug = store.save(doc("v1"), "first").slug
    store.save_data(slug, {"v": 1, "habits": []})
    store.save(doc("v2"), "add categories", slug=slug)            # the page v1 was written for data v1
    assert store.info(slug).undo_warning is False                 # nothing migrated yet
    store.save_data(slug, {"v": 2, "habits": [], "categories": []})
    assert store.info(slug).undo_warning is True and store.info(slug).as_dict()["undo_warning"] is True
    store.revert(slug)
    assert store.info(slug).undo_warning is False                 # no earlier version left to warn about


def test_undo_from_the_chat_says_when_newer_entries_may_not_open(tmp_path: Path) -> None:
    chat = Chat()

    async def go():
        door, store, _ = door_for(tmp_path)
        store.save(doc("v1"), "first")
        store.save_data("habit-tracker", {"v": 1})
        store.save(doc("v2"), "change", slug="habit-tracker")
        store.save_data("habit-tracker", {"v": 2})
        return await door.handle("undo_app", {"app": "habit tracker"}, 7, chat.send, chat.say)

    out = asyncio.run(go())
    assert out["ok"] and "newer shape" in out["note"] and "may not open" in chat.sent[-1][1]


@pytest.mark.parametrize("url,ok", [
    ("https://cdn.jsdelivr.net/npm/chart.js@4.4.1/dist/chart.umd.min.js", True),
    ("https://cdn.jsdelivr.net/npm/@kurkle/color@0.3.2/dist/color.min.js", True),
    ("https://cdn.jsdelivr.net/npm/chart.js@4.4.1", True),
    ("https://cdn.jsdelivr.net/npm/chart.js/dist/chart.umd.min.js", False),       # no version: changes
    ("https://cdn.jsdelivr.net/npm/chart.js@latest/dist/chart.umd.min.js", False),
    ("https://cdn.jsdelivr.net/npm/chart.js@4/dist/chart.umd.min.js", False),     # a range
    ("https://cdn.jsdelivr.net/gh/someone/lib@main/x.js", False),                 # anyone's branch
    ("https://cdn.jsdelivr.net/combine/npm/a@1.0.0,npm/b", False),
])
def test_a_library_must_be_an_npm_package_at_an_exact_version(url: str, ok: bool) -> None:
    page = DOC.replace("hi", f'<script src="{url}"></script>')
    assert (apps_maker.load_issues(page) == []) is ok and (static_issues(page) == []) is ok


def test_a_version_that_loads_outside_code_is_never_saved(tmp_path: Path) -> None:
    unpinned = DOC.replace("hi", '<script src="https://cdn.jsdelivr.net/npm/chart.js/dist/chart.umd.min.js"></script>')
    maker, _, store = maker_for(tmp_path, Script(fenced(unpinned)))
    made = asyncio.run(maker.make("a chart"))
    assert not made.ok and "outside" in made.reason and store.list() == []
    pinned = unpinned.replace("chart.js/dist", "chart.js@4.4.1/dist")
    maker, _, store = maker_for(tmp_path / "b", Script(fenced(unpinned), fenced(pinned)))
    made = asyncio.run(maker.make("a chart"))
    assert made.ok and made.rounds == 1 and "chart.js@4.4.1" in store.html(made.app.slug)


def test_a_version_the_check_could_not_run_on_never_beats_a_tested_one(tmp_path: Path) -> None:
    class Flaky(FakeCheck):
        async def __call__(self, html, *, seed=None):
            if "v2" in html:
                return CheckReport(skipped="the check could not run (Error)")
            return await super().__call__(html, seed=seed)

    gen = Script(fenced(doc("v1")), fenced(doc("v2")))
    maker, _, store = maker_for(tmp_path, gen, Flaky({"v1": ["a", "b", "c"]}))
    made = asyncio.run(maker.make("habit tracker"))
    assert made.ok and "v1" in store.html(made.app.slug) and made.issues == ["a", "b", "c"]
    assert made.check is not None and not made.check.skipped


def test_a_chat_change_that_fails_says_why_and_that_the_app_is_unchanged(tmp_path: Path) -> None:
    import anthropic

    chat = Chat()
    request = httpx.Request("POST", "https://api.anthropic.com/v1/messages")

    async def go():
        door, store, spawned = door_for(tmp_path, Script(anthropic.APIConnectionError(request=request)))
        store.save(DOC, "x")
        await door.handle("change_app", {"app": "habit tracker", "change": "add a chart"}, 7, chat.send, chat.say)
        await door.handle("make_app", {"request": "a reading list"}, 7, chat.send, chat.say)
        await asyncio.gather(*spawned)

    asyncio.run(go())
    assert chat.sent[0][1:] == ("I couldn't change Habit Tracker; it is unchanged: Claude could not be reached. "
                                "Check the Mac's internet connection.", [CHANGE])
    assert chat.said[0].startswith("I couldn't build that app: Claude could not be reached.")   # no app, no chat


def test_a_chat_build_says_when_it_is_tested_and_fixed_and_what_may_still_be_wrong(tmp_path: Path,
                                                                                 monkeypatch) -> None:
    monkeypatch.setattr(apps_maker, "CHAT_PROGRESS_SECS", 0.0)
    chat = Chat()
    check = FakeCheck({"v1": ["Tapping Add threw."], "v2": ["Tapping Add threw."], "v3": ["Tapping Add threw."]})

    async def go():
        door, store, spawned = door_for(tmp_path, Script(fenced(doc("v1")), fenced(doc("v2")), fenced(doc("v3"))))
        door._app.maker._check = check
        await door.handle("make_app", {"request": "habit tracker"}, 7, chat.send, chat.say, chat.send_photo)
        await asyncio.gather(*spawned)
        await asyncio.sleep(0)

    asyncio.run(go())
    assert chat.said == ["Written. Testing it on a phone…", "The phone test found 1 problem; fixing it…",
                         "The phone test found 1 problem; fixing it…"]
    caption = chat.photos[0][2]
    assert "It may still have a problem: Tapping Add threw. Tell me what to fix." in caption
    assert caption.endswith("(Picture: a test run with sample entries.)")


def test_back_on_an_apps_first_view_goes_home_and_elsewhere_runs_the_apps_own_handler() -> None:
    pytest.importorskip("playwright.async_api")
    from playwright.async_api import async_playwright

    from eve_bridge.apps_maker import EVE_JS

    # Telegram's BackButton as its script builds it: one native button, handlers kept by Telegram.
    stub = """window.Telegram = { WebApp: { isVersionAtLeast: () => true, BackButton: (() => {
      const cbs = []; const b = { isVisible: false,
        show() { b.isVisible = true; return b; }, hide() { b.isVisible = false; return b; },
        onClick(f) { cbs.push(f); return b; }, offClick(f) { const i = cbs.indexOf(f); if (i >= 0) cbs.splice(i, 1); return b; },
        press() { cbs.slice().forEach((f) => f()); } }; return b; })() } };"""
    app = """<script>const bb = Telegram.WebApp.BackButton; window.backs = 0;
      bb.onClick(() => { window.backs++; bb.hide(); });   // the app's own "back to my home view"
      bb.hide();                                           // it starts on its home view</script>"""
    page_html = f"<html><head><script>{stub}</script><script>{EVE_JS}</script></head><body>{app}</body></html>"

    async def go():
        async with async_playwright() as p:
            b = await p.chromium.launch()
            page = await b.new_page()
            await page.route("https://app.test/**", lambda r: r.fulfill(
                status=200, content_type="text/html", body=page_html if "/apps/" in r.request.url else "HOME"))
            await page.goto("https://app.test/apps/x/?t=abc")
            shown_on_home = await page.evaluate("Telegram.WebApp.BackButton.isVisible")
            await page.evaluate("Telegram.WebApp.BackButton.show(); Telegram.WebApp.BackButton.press()")
            inner = await page.evaluate("({ backs: window.backs, url: location.href, slug: eve.slug })")
            async with page.expect_navigation():
                await page.evaluate("Telegram.WebApp.BackButton.press()")
            url = page.url
            await b.close()
            return shown_on_home, inner, url

    shown_on_home, inner, url = asyncio.run(go())
    assert shown_on_home is True                                 # Back shows on the app's first view too
    assert inner == {"backs": 1, "url": "https://app.test/apps/x/?t=abc", "slug": "x"}   # the app's own back
    assert url == "https://app.test/"                            # then home, to eve's list


# ---- journeys: the maker writes them, the store versions them, the check walks them -------------------------

JOURNEY = [{"name": "Add one", "steps": [{"do": "tap", "target": "Add"}], "expect": ["1 habit"]}]


def with_journeys(page: str, journeys: list[dict[str, Any]] = JOURNEY) -> str:
    return fenced(page) + "\n```journeys\n" + json.dumps(journeys) + "\n```\n"


class JourneyCheck(FakeCheck):
    """FakeCheck that also records the journeys it was given, and fails a journey on a marked page."""

    def __init__(self, fail: str = "\0") -> None:
        super().__init__()
        self.fail = fail
        self.given: list[Any] = []

    async def __call__(self, html: str, *, seed: Any = None, journeys: Any = None) -> CheckReport:
        self.given.append(journeys)
        bad = self.fail in html and journeys and journeys[0].expect == ("1 habit",)
        issues = ['Journey "Add one": after all 1 steps the screen does not show "1 habit".'] if bad else []
        return CheckReport(ok=not issues, issues=issues, taps=3, fields=1, saves=2, screenshot=b"\xff\xd8jpeg")


def test_the_prompt_asks_for_journeys_and_the_skeletons_parse() -> None:
    from eve_bridge import jev_verify

    for must in ("```journeys", '"do": "tap"', "literal", "fresh app with no saved data", "aria-label"):
        assert must in MAKER_PROMPT, must
    found, problems = jev_verify.parse_journeys(json.loads(apps_maker.SKELETON_JOURNEYS))
    assert problems == [] and len(found) == 3 and apps_maker.SKELETON_JOURNEYS in MAKER_PROMPT


def test_a_build_keeps_its_journeys_and_undo_restores_the_pair(tmp_path: Path) -> None:
    check = JourneyCheck()
    first = [{**JOURNEY[0], "name": "First"}]
    maker, _, store = maker_for(tmp_path, Script(with_journeys(doc("v1"), first), with_journeys(doc("v2"))), check)
    made = asyncio.run(maker.make("habit tracker"))
    slug = made.app.slug
    assert [j.name for j in store.journeys(slug)] == ["First"] and check.given[0][0].name == "First"
    made2 = asyncio.run(maker.edit(slug, "add a thing"))
    assert made2.ok and [j.name for j in store.journeys(slug)] == ["Add one"]
    edit_prompt_text = maker._generate.calls[1][0]["content"]
    assert "Its journeys now:" in edit_prompt_text and '"First"' in edit_prompt_text
    assert (tmp_path / "apps" / slug / "versions" / "1.journeys.json").is_file()
    store.revert(slug)
    assert "v1" in store.html(slug) and [j.name for j in store.journeys(slug)] == ["First"]


def test_an_app_without_journeys_still_builds_and_is_checked_by_the_script_alone(tmp_path: Path) -> None:
    check = JourneyCheck()
    maker, _, store = maker_for(tmp_path, Script(fenced(doc("v1"))), check)
    made = asyncio.run(maker.make("habit tracker"))
    assert made.ok and store.journeys(made.app.slug) is None and check.given == [None]


def test_a_failed_journey_goes_to_the_repair_round_and_a_journey_only_fix_is_rechecked(tmp_path: Path) -> None:
    check = JourneyCheck(fail="v1")
    maker, _, store = maker_for(tmp_path, Script(with_journeys(doc("v1")), with_journeys(doc("v2"))), check)
    made = asyncio.run(maker.make("habit tracker"))
    repair = maker._generate.calls[1][2]["content"]
    assert 'Journey "Add one"' in repair and "Never change a journey to get around" in repair
    assert made.ok and made.rounds == 1 and "v2" in store.html(made.app.slug)
    # a repair that only rewrites the journeys keeps the last file and checks it again with them
    check2 = JourneyCheck(fail="v1")
    fixed = [{**JOURNEY[0], "expect": ["other"]}]
    maker2, _, store2 = maker_for(tmp_path / "b", Script(with_journeys(doc("v1")),
                                                         "```journeys\n" + json.dumps(fixed) + "\n```"), check2)
    made2 = asyncio.run(maker2.make("habit tracker"))
    assert len(check2.given) == 2 and check2.given[1][0].expect == ("other",)
    assert store2.journeys(made2.app.slug)[0].expect == ("other",)


def test_a_malformed_journeys_block_is_a_problem_the_repair_sees(tmp_path: Path) -> None:
    gen = Script(fenced(doc("v1")) + "\n```journeys\n[not json\n```", with_journeys(doc("v1")))
    maker, _, store = maker_for(tmp_path, gen, JourneyCheck())
    made = asyncio.run(maker.make("habit tracker"))
    assert "Journeys block: The journeys block is not valid JSON" in gen.calls[1][2]["content"]
    assert made.ok and store.journeys(made.app.slug) is not None


def test_the_skeleton_walks_its_own_journeys_with_a_literal_verifier() -> None:
    pytest.importorskip("playwright.async_api")
    from test_jev_verify import LiteralJev

    from eve_bridge import jev_verify
    from eve_bridge.app_check import check_app

    found, _ = jev_verify.parse_journeys(json.loads(apps_maker.SKELETON_JOURNEYS))
    r = asyncio.run(check_app(SKELETON, journeys=found, verifier=jev_verify.Verifier(LiteralJev())))
    assert r.journeys.passed == 3, r.journeys.as_dict()
    assert r.ok and "journeys 3/3" in r.summary()


def test_an_old_versions_journeys_go_with_it_when_it_is_pruned(tmp_path: Path) -> None:
    from eve_bridge import jev_verify

    store = AppStore(tmp_path / "apps")
    found, _ = jev_verify.parse_journeys(JOURNEY)
    info = store.save(doc("v0"), "make it", journeys=found)
    for n in range(1, apps_maker.MAX_VERSIONS + 3):
        store.save(doc(f"v{n}"), f"change {n}", info.slug, journeys=found if n % 2 else None)
    vdir = tmp_path / "apps" / info.slug / "versions"
    pages = {p.stem for p in vdir.glob("*.html")}
    kept = {p.name.split(".")[0] for p in vdir.glob("*.journeys.json")}
    assert len(pages) == apps_maker.MAX_VERSIONS and kept <= pages          # no journeys outlive their page
    assert store.journeys(info.slug) is None                                # the last save had none


# ---- the app's change chat: what each build did, kept in meta.json ----------------------------------------------

class WalkedCheck(FakeCheck):
    """FakeCheck whose report carries Jev's journeys: ``passed`` of ``total`` walked, or not run (``why``)."""

    def __init__(self, passed: int = 3, total: int = 3, why: str = "") -> None:
        super().__init__()
        self.passed, self.total, self.why = passed, total, why

    async def __call__(self, html: str, *, seed: Any = None, journeys: Any = None) -> CheckReport:
        from eve_bridge.jev_verify import JourneyResult, JourneyRun

        results = [JourneyResult(f"j{i}", "passed" if i < self.passed else "step", steps=2)
                   for i in range(self.total)] if not self.why else []
        run = JourneyRun(results=results, total=self.total, not_run=self.why)
        issues = run.issues()
        return CheckReport(ok=not issues, issues=issues, taps=3, fields=1, saves=2, journeys=run)


def test_each_build_records_its_version_check_journeys_and_cost_for_the_change_chat(tmp_path: Path) -> None:
    gen = Script(with_journeys(DOC), with_journeys(doc("v2")))
    maker, _, store = maker_for(tmp_path, gen, WalkedCheck())
    asyncio.run(maker.make("a habit tracker"))
    asyncio.run(maker.edit("habit-tracker", "make the buttons bigger"))
    thread = store.thread("habit-tracker")
    assert [(i["kind"], i["text"]) for i in thread] == [("request", "a habit tracker"),
                                                         ("request", "make the buttons bigger")]
    first, second = thread[0]["result"], thread[1]["result"]
    assert first["ok"] and first["version"] == 1 and second["version"] == 2
    assert first["check"] == "passed: 1 fields filled, 3 taps, 2 saves, journeys 3/3" and first["tested"]
    assert first["journeys"] == {"passed": 3, "total": 3, "not_run": ""}
    assert first["usd"] == pytest.approx(ONE, abs=1e-4) and first["problems"] == 0 and first["rounds"] == 0
    assert set(first) == set(apps_maker._RESULT_KEYS)
    meta = json.loads((tmp_path / "apps" / "habit-tracker" / "meta.json").read_text())
    assert meta["version"] == 2 and len(meta["builds"]) == 2
    assert [r["text"] for r in store.requests("habit-tracker")] == ["a habit tracker", "make the buttons bigger"]


def test_a_journey_that_did_not_run_or_failed_is_in_the_record(tmp_path: Path) -> None:
    maker, _, store = maker_for(tmp_path, Script(with_journeys(DOC)), WalkedCheck(why="Jev did not answer"))
    asyncio.run(maker.make("a habit tracker"))
    maker._check = WalkedCheck(passed=1, total=2)
    maker.max_repairs = 0
    asyncio.run(maker.edit("habit-tracker", "add categories"))
    first, second = (i["result"] for i in store.thread("habit-tracker"))
    assert first["journeys"] == {"passed": 0, "total": 3, "not_run": "Jev did not answer"}
    assert second["journeys"] == {"passed": 1, "total": 2, "not_run": ""} and second["problems"] == 1
    assert second["issues"][0] == '"j1" did not go through.'           # the owner's line, not the repair's


def test_a_change_that_saved_nothing_is_recorded_but_is_not_a_request(tmp_path: Path) -> None:
    gen = Script(fenced(DOC), "Sorry, I can't.", ConnectionError("down"))
    maker, _, store = maker_for(tmp_path, gen, max_repairs=0)
    asyncio.run(maker.make("a habit tracker"))
    gen.answers = ["Sorry, I can't."]
    made = asyncio.run(maker.edit("habit-tracker", "add a chart"))
    gen.answers = [ConnectionError("down")]
    with pytest.raises(ConnectionError):
        asyncio.run(maker.edit("habit-tracker", "make it purple"))
    thread = store.thread("habit-tracker")
    assert not made.ok and [i["text"] for i in thread] == ["a habit tracker", "add a chart", "make it purple"]
    chart, purple = thread[1]["result"], thread[2]["result"]
    assert not chart["ok"] and chart["reason"] == "Claude did not return a complete app" and chart["version"] is None
    assert chart["usd"] == pytest.approx(ONE, abs=1e-4)                           # it cost money and did nothing
    assert not purple["ok"] and purple["reason"] == "the build stopped before it finished" and purple["usd"] == 0
    # the next build's prompt hears only what the app was changed to do
    assert [r["text"] for r in store.requests("habit-tracker")] == ["a habit tracker"]
    assert store.info("habit-tracker").versions == 0 and "hi" in store.html("habit-tracker")


def test_the_thread_keeps_undos_and_requests_from_before_results_and_stays_bounded(tmp_path: Path) -> None:
    store = AppStore(tmp_path / "apps")
    d = tmp_path / "apps" / "habit-tracker"
    d.mkdir(parents=True)
    (d / "index.html").write_text(DOC)
    (d / "meta.json").write_text(json.dumps({                     # an app built before results were recorded
        "title": "Habit Tracker", "created": "2026-09-20T10:00:00", "updated": "2026-09-21T10:00:00",
        "requests": [{"at": "2026-09-20T10:00:00", "text": "a habit tracker"},
                     {"at": "2026-09-21T10:00:00", "text": "add streaks"},
                     {"at": "2026-09-21T11:00:00", "text": "(undo)"}]}))
    store.save(doc("v3"), "add a chart", "habit-tracker", build={"ok": True, "check": "passed"})
    thread = store.thread("habit-tracker")
    assert [(i["kind"], i["text"], i["result"] is not None) for i in thread] == [
        ("request", "a habit tracker", False), ("request", "add streaks", False), ("undo", "(undo)", False),
        ("request", "add a chart", True)]
    assert thread[-1]["result"]["version"] == 3                   # two builds before it (the undo is not one)
    store.revert("habit-tracker")
    assert store.thread("habit-tracker")[-1]["kind"] == "undo"
    for n in range(apps_maker.MAX_BUILDS + 5):
        store.record_build("habit-tracker", f"try {n}", {"ok": False, "reason": "no"})
    meta = json.loads((d / "meta.json").read_text())
    assert len(meta["builds"]) == apps_maker.MAX_BUILDS and meta["builds"][-1]["request"] == f"try {apps_maker.MAX_BUILDS + 4}"
    with pytest.raises(KeyError):
        store.thread("no-such-app")
    with pytest.raises(KeyError):
        store.record_build("no-such-app", "x", {"ok": False})


# ---- the way into an app's change chat from inside the app (eve.js) -------------------------------------------

def test_inside_an_app_a_pencil_and_telegrams_settings_item_lead_to_its_change_chat() -> None:
    pytest.importorskip("playwright.async_api")
    from playwright.async_api import async_playwright

    from eve_bridge.apps_maker import EVE_JS

    stub = """window.Telegram = { WebApp: { isVersionAtLeast: (v) => parseFloat(v) <= 7.0,
      BackButton: { show() {}, hide() {}, onClick() {}, offClick() {} },
      SettingsButton: (() => { const cbs = []; const b = { isVisible: false, show() { b.isVisible = true; return b; },
        onClick(f) { cbs.push(f); return b; }, press() { cbs.forEach((f) => f()); } }; return b; })() } };"""
    # An app whose own CSS would hide or restyle a plain injected button, and that re-renders its whole body.
    hostile_css = """<style>* { visibility: hidden !important; position: static !important; }
      button { display: none !important; } html, body { overflow: hidden; }</style>"""
    body = """<h1>A very long app title that runs</h1><script>document.body.innerHTML = '<p>re-rendered</p>';</script>"""
    page_html = (f"<html><head><script>{stub}</script><script>{EVE_JS}</script>{hostile_css}</head>"
                 f"<body>{body}</body></html>")

    async def go():
        async with async_playwright() as p:
            b = await p.chromium.launch()
            page = await b.new_page(viewport={"width": 390, "height": 760}, has_touch=True)
            await page.route("https://app.test/**", lambda r: r.fulfill(
                status=200, content_type="text/html", body=page_html if "/apps/" in r.request.url else "HOME"))
            await page.goto("https://app.test/apps/habit-tracker/?t=abc")
            seen = await page.evaluate("""() => { const host = document.querySelector('eve-change');
              const r = host.getBoundingClientRect(), s = getComputedStyle(host);
              const top = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
              return { w: r.width, h: r.height, right: innerWidth - r.right, top: r.top, visible: s.visibility,
                       topmost: top === host, count: document.querySelectorAll('eve-change').length,
                       settings: Telegram.WebApp.SettingsButton.isVisible,
                       wide: document.documentElement.scrollWidth > innerWidth }; }""")
            async with page.expect_navigation():
                await page.touchscreen.tap(390 - 6 - 22, 6 + 22)
            tapped = page.url
            await page.goto("https://app.test/apps/habit-tracker/?t=abc")
            async with page.expect_navigation():
                await page.evaluate("Telegram.WebApp.SettingsButton.press()")
            settings = page.url
            await b.close()
            return seen, tapped, settings

    seen, tapped, settings = asyncio.run(go())
    assert seen == {"w": 44, "h": 44, "right": 6, "top": 6, "visible": "visible", "topmost": True, "count": 1,
                    "settings": True, "wide": False}
    assert tapped == "https://app.test/?chat=habit-tracker" and settings == "https://app.test/?chat=habit-tracker"


def test_without_telegrams_settings_item_the_pencil_still_leads_there() -> None:
    pytest.importorskip("playwright.async_api")
    from playwright.async_api import async_playwright

    from eve_bridge.apps_maker import EVE_JS

    old = "window.Telegram = { WebApp: { isVersionAtLeast: (v) => parseFloat(v) <= 6.9 } };"
    page_html = f"<html><head><script>{old}</script><script>{EVE_JS}</script></head><body>app</body></html>"

    async def go():
        async with async_playwright() as p:
            b = await p.chromium.launch()
            page = await b.new_page()
            errors: list[str] = []
            page.on("pageerror", lambda e: errors.append(str(e)))
            await page.route("https://app.test/**", lambda r: r.fulfill(status=200, content_type="text/html",
                                                                        body=page_html))
            await page.goto("https://app.test/apps/x/?t=abc")
            label = await page.evaluate("document.querySelector('eve-change') !== null")
            await b.close()
            return label, errors

    present, errors = asyncio.run(go())
    assert present and errors == []


def test_a_change_from_the_chat_door_brings_a_change_button_to_its_chat(tmp_path: Path) -> None:
    chat = Chat()

    async def go():
        door, store, spawned = door_for(tmp_path, Script(fenced(DOC), fenced(doc("v2"))))
        await door.handle("make_app", {"request": "habit tracker"}, 7, chat.send, chat.say, chat.send_photo)
        await asyncio.gather(*spawned)
        await door.handle("change_app", {"app": "habit tracker", "change": "add categories"}, 7, chat.send, chat.say,
                          chat.send_photo)
        await asyncio.gather(*spawned)

    asyncio.run(go())
    made, changed = chat.photos
    assert made[3] == [OPEN] and changed[3] == [OPEN, CHANGE]            # Open stays Open <app>; Change is second


# ---- review round 3: each of these failed on the code before its fix ----------------------------------------

TWO = [{"name": "Add one", "steps": [{"do": "tap", "target": "Add"}], "expect": ["1 habit"]},
       {"name": "Add two", "steps": [{"do": "tap", "target": "Add"}, {"do": "tap", "target": "Add"}],
        "expect": ["2 habits"]}]


def test_an_empty_journeys_block_never_drops_the_journeys_in_hand(tmp_path: Path) -> None:
    check = JourneyCheck()
    gen = Script(with_journeys(DOC, TWO), fenced(doc("v2")) + "\n```journeys\n[]\n```", fenced(doc("v2")))
    maker, _, store = maker_for(tmp_path, gen, check, max_repairs=1)
    asyncio.run(maker.make("a habit tracker"))
    made = asyncio.run(maker.edit("habit-tracker", "bigger buttons"))
    assert [j.name for j in check.given[1]] == ["Add one", "Add two"]             # walked with the ones in hand
    assert "Journeys block: The journeys block holds no journey" in gen.calls[2][2]["content"]
    assert made.ok and [j.name for j in store.journeys("habit-tracker")] == ["Add one", "Add two"]


def test_a_broken_journeys_block_stays_a_problem_until_a_good_one_comes(tmp_path: Path) -> None:
    gen = Script(fenced(doc("v1")) + "\n```journeys\n[not json\n```",
                 f"{apps_maker.EDIT_START}\nv1\n{apps_maker.EDIT_MID}\nv1b\n{apps_maker.EDIT_END}")
    maker, _, store = maker_for(tmp_path, gen, JourneyCheck(), max_repairs=1)
    made = asyncio.run(maker.make("habit tracker"))
    assert any(i.startswith("Journeys block: The journeys block is not valid JSON") for i in made.issues), made.issues


def test_a_repair_that_leaves_a_journey_out_still_walks_it(tmp_path: Path) -> None:
    check = JourneyCheck(fail="v1")
    gen = Script(with_journeys(doc("v1"), TWO), with_journeys(doc("v2"), TWO[1:]))
    maker, _, store = maker_for(tmp_path, gen, check, max_repairs=1)
    made = asyncio.run(maker.make("habit tracker"))
    assert [j.name for j in check.given[1]] == ["Add two", "Add one"]             # never weakened by a repair
    assert made.ok and {j.name for j in store.journeys(made.app.slug)} == {"Add one", "Add two"}


def test_an_edits_journeys_never_carry_the_owners_saved_data_to_jev(tmp_path: Path) -> None:
    check = JourneyCheck()
    leaky = [{"name": "Split", "steps": [{"do": "type", "target": "Name", "value": "Priya"},
                                         {"do": "tap", "target": "Add"}], "expect": ["Priya owes $4.20"]},
             {"name": "Clean", "steps": [{"do": "type", "target": "Name", "value": "Alex"},
                                         {"do": "tap", "target": "Add"}], "expect": ["Alex owes $3.00"]},
             {"name": "Default", "steps": [{"do": "tap", "target": "Groceries"}], "expect": ["Groceries"]}]
    page = doc("v2").replace("</body>", "<p>Groceries</p></body>")        # the app's own default: not the owner's
    gen = Script(with_journeys(DOC, TWO), with_journeys(page, leaky), with_journeys(page, leaky[1:]))
    maker, _, store = maker_for(tmp_path, gen, check, max_repairs=1)
    asyncio.run(maker.make("a splitter"))
    store.save_data("habit-tracker", {"v": 1, "people": ["Priya", "Sam"], "cats": ["Groceries"],
                                      "exp": [{"what": "Dinner at the corner place", "amt": 420}]})
    made = asyncio.run(maker.edit("habit-tracker", "add categories"))
    walked = [j.name for j in check.given[1]]
    assert walked == ["Clean", "Default"]                                    # "Split" never reaches the verifier
    repair = gen.calls[2][2]["content"]
    assert 'Journeys block: journey "Split" uses the owner\'s saved data' in repair and "Priya" not in repair
    assert made.ok and [j.name for j in store.journeys("habit-tracker")] == ["Clean", "Default"]


def test_the_prompt_asks_for_result_text_invented_values_a_free_corner_and_option_picks() -> None:
    journeys_part = MAKER_PROMPT[MAKER_PROMPT.index("<journeys>"):MAKER_PROMPT.index("</journeys>")]
    assert "toast" in journeys_part and "the value the journey entered" in journeys_part
    assert "made-up example values" in journeys_part and "never from the saved data" in journeys_part
    assert "a group of option buttons" in journeys_part
    assert "top-right 56x56px" in MAKER_PROMPT and "padding-right: 56px" in MAKER_PROMPT


def rate_limited() -> Exception:
    import anthropic

    request = httpx.Request("POST", "https://api.anthropic.com/v1/messages")
    return anthropic.RateLimitError("slow down", response=httpx.Response(429, request=request), body=None)


def test_a_change_that_stops_on_an_api_error_records_why_and_what_it_already_cost(tmp_path: Path) -> None:
    gen = Script(fenced(DOC), ("```html\n<!doctype html><html><body>cut", "max_tokens"), rate_limited())
    maker, _, store = maker_for(tmp_path, gen, max_repairs=1)
    asyncio.run(maker.make("a habit tracker"))
    gen.answers = [("```html\n<!doctype html><html><body>cut", "max_tokens"), rate_limited()]
    with pytest.raises(Exception, match="slow down"):
        asyncio.run(maker.edit("habit-tracker", "add a chart"))
    res = store.thread("habit-tracker")[-1]["result"]
    assert res["reason"] == "Claude is rate-limited right now. Try again in a minute"
    assert res["usd"] == pytest.approx(ONE, abs=1e-4) and res["rounds"] == 1


def test_the_owner_reads_journey_problems_in_plain_words_and_long_lines_end_in_an_ellipsis() -> None:
    from eve_bridge.jev_verify import JourneyResult, JourneyRun

    failed = JourneyResult("Split a dinner", "step", steps=6, step=3, step_words='tap "Add expense"',
                           reason='tap "Add expense": could not find "Add expense" to tap (the controls on screen: '
                                  + ", ".join(f'"Control {n}"' for n in range(14)) + ").",
                           evidence='"' + "Expense Splitter | People " * 30 + '"')
    run = JourneyRun(results=[failed], total=1)
    long_one = "Uncaught error while tapping: " + "x" * 400
    report = CheckReport(ok=False, issues=run.issues() + [long_one], journeys=run)
    rec = apps_maker.build_record(ok=True, report=report, issues=report.issues, usd=0.1, secs=5, rounds=1)
    assert rec["issues"][0] == 'Couldn\'t tap "Add expense" in "Split a dinner".'
    assert len(rec["issues"][1]) == 300 and rec["issues"][1].endswith("…")
    assert apps_maker.owner_issues(report, report.issues)[0] == rec["issues"][0]


def test_a_chat_build_caption_names_a_journey_problem_in_plain_words(tmp_path: Path) -> None:
    from eve_bridge.jev_verify import JourneyResult, JourneyRun

    class Failing(FakeCheck):
        async def __call__(self, html: str, *, seed: Any = None, journeys: Any = None) -> CheckReport:
            run = JourneyRun(results=[JourneyResult("Add one", "expect", steps=1,
                                                    expects=[{"text": "1 habit", "score": 0.1, "shown": False}],
                                                    evidence='"' + "No habits yet " * 40 + '"')], total=1)
            return CheckReport(ok=False, issues=run.issues(), journeys=run, screenshot=b"\xff\xd8jpeg")

    chat = Chat()

    async def go():
        door, store, spawned = door_for(tmp_path, Script(with_journeys(DOC)))
        door._app.maker._check = Failing()
        door._app.maker.max_repairs = 0
        await door.handle("make_app", {"request": "habit tracker"}, 7, chat.send, chat.say, chat.send_photo)
        await asyncio.gather(*spawned)

    asyncio.run(go())
    assert 'It may still have a problem: After "Add one", the screen didn\'t show "1 habit".' in chat.photos[0][2]


def test_a_chat_change_that_fails_still_offers_its_change_chat(tmp_path: Path) -> None:
    chat = Chat()

    async def go():
        door, store, spawned = door_for(tmp_path, Script("Sorry, I can't."))
        door._app.maker.max_repairs = 0
        store.save(DOC, "x")
        await door.handle("change_app", {"app": "habit tracker", "change": "add a chart"}, 7, chat.send, chat.say)
        await door.handle("change_app", {"app": "habit tracker", "change": "add a chart"}, 7, chat.send, chat.say)
        await asyncio.gather(*spawned)
        door._app.maker._generate = Script(rate_limited())
        await door.handle("change_app", {"app": "habit tracker", "change": "add a chart"}, 7, chat.send, chat.say)
        await asyncio.gather(*spawned)

    asyncio.run(go())
    lines = [(text, buttons) for _, text, buttons in chat.sent]
    assert lines[0] == ("I couldn't change Habit Tracker; it is unchanged: Claude did not return a complete app.",
                        [CHANGE])
    assert lines[-1] == ("I couldn't change Habit Tracker; it is unchanged: Claude is rate-limited right now. Try "
                         "again in a minute.", [CHANGE])


def test_the_pencil_steps_aside_for_an_apps_own_control_in_its_corner() -> None:
    pytest.importorskip("playwright.async_api")
    from playwright.async_api import async_playwright

    from eve_bridge.apps_maker import EVE_JS

    stub = "window.Telegram = { WebApp: { isVersionAtLeast: () => false } };"
    body = """<header style="display:flex;padding:12px 16px 0"><h1 style="flex:1;margin:0">Budget</h1>
      <button id="gear" aria-label="Settings" style="width:44px;height:44px" onclick="window.__gear=1">⚙</button></header>
      <p>content</p>"""
    page_html = f"<html><head><script>{stub}</script><script>{EVE_JS}</script></head><body style='margin:0'>{body}</body></html>"

    async def go():
        async with async_playwright() as p:
            b = await p.chromium.launch()
            page = await b.new_page(viewport={"width": 390, "height": 760}, has_touch=True)
            await page.route("https://app.test/**", lambda r: r.fulfill(status=200, content_type="text/html",
                                                                        body=page_html))
            await page.goto("https://app.test/apps/budget/?t=abc")
            await page.wait_for_timeout(100)
            state = "() => getComputedStyle(document.querySelector('eve-change')).visibility"
            covered = await page.evaluate(state)
            await page.touchscreen.tap(390 - 6 - 22, 6 + 22)          # where the pencil would be
            gear = await page.evaluate("window.__gear === 1")
            await page.evaluate("document.getElementById('gear').remove()")      # the view changes: the corner is free
            await page.wait_for_timeout(100)
            free = await page.evaluate(state)
            await b.close()
            return covered, gear, free, page.url

    covered, gear, free, url = asyncio.run(go())
    assert covered == "hidden" and gear is True and free == "visible" and url.endswith("/apps/budget/?t=abc")
