"""caption_pager.py with a fake clock: wrapping, page holds, streaming page
turns, reply chaining, barge-in, overflow, and the geometry shared with the
firmware. Pure module, no asyncio."""

from __future__ import annotations

import logging
import random
import re
from pathlib import Path

import pytest

from eve_bridge.caption_pager import (
    CaptionPager,
    Clear,
    PagerConfig,
    ShowPage,
    caption_instructions,
    complete_prefix,
    page_hold_secs,
    paginate,
    wrap,
)

REPO = Path(__file__).resolve().parents[2]
CFG = PagerConfig()


def _clamp(x: float, lo: float, hi: float) -> float:
    return max(lo, min(hi, x))


def _chars(page: tuple[str, ...]) -> int:
    return sum(len(line) for line in page)


class _Stream:
    """Feeds a reply into a pager at `cps` and records every event with its time."""

    def __init__(self, pager: CaptionPager, text: str, cps: float, *, tick: float = 0.05,
                 delta: int = 4, jitter: random.Random | None = None, t0: float = 0.0) -> None:
        self.pager, self.text, self.cps, self.tick, self.delta, self.jitter = pager, text, cps, tick, delta, jitter
        self.events: list[tuple[float, object]] = []
        self.updates: list[tuple[float, str, bool]] = []
        self.t = t0

    def _poll(self) -> None:
        for e in self.pager.poll(self.t):
            self.events.append((self.t, e))

    def run(self, max_secs: float = 600.0) -> None:
        self.pager.begin_reply(self.t)
        self._poll()
        sent = 0
        next_delta_at = self.t
        final_at: float | None = None
        end = self.t + max_secs
        while self.t < end:
            while sent < len(self.text) and self.t >= next_delta_at:
                n = self.delta if self.jitter is None else self.jitter.randint(1, 2 * self.delta)
                sent = min(len(self.text), sent + n)
                final = sent == len(self.text)
                self.pager.update(self.t, self.text[:sent], final)
                self.updates.append((self.t, self.text[:sent], final))
                if final:
                    final_at = self.t
                gap = n / self.cps
                next_delta_at += gap if self.jitter is None else gap * self.jitter.uniform(0.5, 1.5)
            self._poll()
            if final_at is not None and not self.pager.busy and not self.pager.replies:
                return
            self.t = round(self.t + self.tick, 6)
        raise AssertionError("pager never went idle")

    def shows(self) -> list[tuple[float, ShowPage]]:
        return [(t, e) for t, e in self.events if isinstance(e, ShowPage)]

    def clears(self) -> list[float]:
        return [t for t, e in self.events if isinstance(e, Clear)]


# ---- pure helpers ------------------------------------------------------------------------------

def test_wrap_never_exceeds_cols_and_keeps_word_order() -> None:
    plain = "Nice,  Spotify's open\nand ready.\n\nGoodbye,   eve. What time is it now, exactly?"
    lines = wrap(plain, 17)
    assert lines and all(1 <= len(line) <= 17 for line in lines)
    assert all(line == line.strip() for line in lines)
    assert " ".join(lines).split() == plain.split()
    url = "see https://example.com/a/very/long/path"        # a 30-char token: hard-split
    lines = wrap(url, 17)
    assert all(1 <= len(line) <= 17 for line in lines)
    assert "".join("".join(lines).split()) == "".join(url.split())
    assert lines[0] == "see" and lines[1] == "https://example.c"
    assert wrap("", 17) == [] and wrap("   \n ", 17) == []


def test_wrap_is_prefix_stable() -> None:
    rng = random.Random(7)
    for _ in range(200):
        words = ["".join(rng.choices("abcdefghijklmnop", k=rng.randint(1, 12))) for _ in range(rng.randint(1, 40))]
        full = wrap(" ".join(words), 17)
        for k in range(1, len(words) + 1):
            part = wrap(" ".join(words[:k]), 17)
            n = len(part)
            assert part[:-1] == full[: n - 1]
            assert full[n - 1].startswith(part[-1])


def test_complete_prefix() -> None:
    assert complete_prefix("Ten past thr", False) == "Ten past "
    assert complete_prefix("Ten past thr", True) == "Ten past thr"
    assert complete_prefix("a ", False) == "a "
    assert complete_prefix("", False) == "" and complete_prefix("", True) == ""
    assert complete_prefix("word", False) == ""


def test_paginate() -> None:
    seven = [f"l{i}" for i in range(7)]
    pages = paginate(seven, 4)
    assert [len(p) for p in pages] == [4, 3] and pages[0] == ("l0", "l1", "l2", "l3")
    assert paginate([], 4) == []
    assert paginate(seven[:4], 4) == [tuple(seven[:4])]


@pytest.mark.parametrize("chars,first,last,expect", [
    (68, True, False, 68 / 12 + 0.8),        # 6.47
    (10, True, False, 2.0),                  # floor
    (200, True, False, 9.0),                 # cap
    (47, True, True, 47 / 12 + 0.8 + 3.0),   # 7.72
    (5, True, True, 6.0),
    (100, True, True, 10.0),
    (15, True, True, 6.0),                   # "Ten past three."
    (30, False, False, 2.5),                 # no lead on a later page
])
def test_page_hold_table(chars: int, first: bool, last: bool, expect: float) -> None:
    assert page_hold_secs(chars, CFG, first=first, last=last) == pytest.approx(expect)
    assert page_hold_secs(68, PagerConfig(read_cps=8), first=True, last=False) == 9.0


# ---- streaming ---------------------------------------------------------------------------------

REPLY_110 = ("Nice, Spotify is open and ready for you now. The playlist you asked for is queued up. "
             "Goodbye, eve, catch you.")


def test_stream_at_40cps_pages_and_holds() -> None:
    assert len(REPLY_110) == 110
    st = _Stream(CaptionPager(), REPLY_110, 40.0)
    st.run()
    shows = st.shows()
    t0, first = shows[0]
    assert first.page == 0 and first.of == 0 and first.chirp is True and first.final is False
    assert first.lines and t0 == pytest.approx(0.1)          # first complete word ("Nice,") after the first delta
    pages = paginate(wrap(REPLY_110, 17), 4)
    assert len(pages) == 2
    # page 0 froze when page 1 first had content
    t_frozen0 = next(t for t, text, final in st.updates if len(paginate(wrap(complete_prefix(text, final), 17), 4)) > 1)
    refills = [(t, e) for t, e in shows if e.page == 0 and not e.chirp]
    assert refills
    for (ta, _), (tb, _) in zip(refills, refills[1:], strict=False):
        assert tb - ta >= 0.15 - 1e-9 or tb == t_frozen0       # the freeze refill goes out at once
    assert refills[-1][1].lines == pages[0]
    turn = [(t, e) for t, e in shows if e.page == 1]
    assert len(turn) == 1
    t1, second = turn[0]
    expect_t1 = max(t0 + _clamp(_chars(pages[0]) / 12 + 0.8, 2.0, 9.0), t_frozen0 + 2.0)
    assert t1 == pytest.approx(expect_t1, abs=0.05 + 1e-9) and t1 >= expect_t1 - 1e-9
    hold1 = _clamp(_chars(pages[1]) / 12 + 3.0, 6.0, 10.0)
    assert second == ShowPage(page=1, of=2, lines=pages[1], hold_ms=round(1000 * hold1), chirp=True, final=True)
    clears = st.clears()
    assert len(clears) == 1 and clears[0] == pytest.approx(t1 + hold1, abs=0.05 + 1e-9) and clears[0] >= t1 + hold1 - 1e-9
    assert st.pager.busy is False
    for _, e in st.events:
        w = e.to_wire()
        assert w["cmd"] == "caption"
        if "lines" in w:
            assert len(w["lines"]) <= 4 and all(len(line) <= 17 for line in w["lines"])


def test_slow_stream_respects_freeze_dwell() -> None:
    st = _Stream(CaptionPager(), REPLY_110, 5.0)
    st.run()
    shows = st.shows()
    t0 = shows[0][0]
    t_frozen0 = next(t for t, text, final in st.updates if len(paginate(wrap(complete_prefix(text, final), 17), 4)) > 1)
    t1 = next(t for t, e in shows if e.page == 1)
    assert t_frozen0 > t0 + 6.47                              # the hold passed long before the page froze
    assert t1 >= t_frozen0 + 2.0 - 1e-9 and t1 < t_frozen0 + 2.0 + 0.05 + 1e-9


def test_dwell_invariants_fuzz() -> None:
    rng = random.Random(2026)
    for _ in range(200):
        words = ["".join(rng.choices("abcdefghijklmnopqrstuvwxyz", k=rng.randint(1, 9))) for _ in range(400)]
        text = ""
        while len(text) < rng.randint(20, 300):
            text += ("" if not text else " ") + words[len(text.split())]
        cps = rng.uniform(5.0, 80.0)
        st = _Stream(CaptionPager(), text, cps, jitter=rng)
        st.run()
        shows = st.shows()
        pages = paginate(wrap(text, 17), 4)
        overflow = len(pages) > 6
        frozen_at: dict[int, float] = {}
        for t, txt, fin in st.updates:
            ps = paginate(wrap(complete_prefix(txt, fin), 17), 4)
            for i in range(len(ps)):
                if i not in frozen_at and (i < len(ps) - 1 or fin):
                    frozen_at[i] = t
        shown_at: dict[int, float] = {}
        last_lines: dict[int, tuple[str, ...]] = {}
        seen_pages: list[int] = []
        for t, e in shows:
            if e.chirp:
                assert e.page not in shown_at
                shown_at[e.page] = t
                seen_pages.append(e.page)
            elif not overflow:
                prev = last_lines[e.page]
                assert len(e.lines) >= len(prev)
                for a, b in zip(prev[:-1], e.lines, strict=False):
                    assert a == b                          # a full line never changes
                assert e.lines[len(prev) - 1].startswith(prev[-1])
            last_lines[e.page] = e.lines
        assert seen_pages == sorted(seen_pages) and len(set(seen_pages)) == len(seen_pages)
        n = len(seen_pages)
        assert n == min(len(pages), 6)
        for i in range(n - 1):
            visible = shown_at[i + 1] - shown_at[i]
            assert visible >= max(2.0, _chars(last_lines[i]) / 12) - 1e-9
            assert shown_at[i + 1] - frozen_at[i] >= 2.0 - 1e-9
        clear = st.clears()[0]
        last = n - 1
        assert clear - shown_at[last] >= 6.0 - 1e-9
        assert clear - max(shown_at[last], frozen_at[last]) <= 10.0 + 0.11   # one tick + jitter of the poll
        if not overflow:
            assert " ".join(" ".join(last_lines[i]) for i in range(n)).split() == text.split()


def test_logged_replies_regression() -> None:
    """Replies from the daemon log of 2026-09-06 (there was no 213-char reply in
    it; the longest logged reply is this 130-char one)."""
    logged = [
        "Nice, Spotify’s open and ready. Goodbye, eve.",
        "Hey there! Thanks for the welcome. I’m here, ready to help with anything you’ve got—just let me know "
        "what you fancy tackling next.",
    ]
    assert len(logged[1]) == 130
    for text in logged:
        st = _Stream(CaptionPager(), text, 40.0)
        st.run()
        shows = st.shows()
        # every line is on screen at least max(1.0, len/15) s: a line first appears
        # in some show and stays until the page turns / the clear
        pages = paginate(wrap(text, 17), 4)
        appeared: dict[tuple[int, int], float] = {}
        gone: dict[int, float] = {}
        for t, e in shows:
            for i in range(len(e.lines)):
                appeared.setdefault((e.page, i), t)
            if e.chirp and e.page > 0:
                gone[e.page - 1] = t
        gone[len(pages) - 1] = st.clears()[0]
        for (page, i), t_in in appeared.items():
            line = pages[page][i]
            assert gone[page] - t_in >= max(1.0, len(line) / 15) - 1e-9
        assert " ".join(" ".join(p) for p in pages).split() == text.split()
        assert all(len(line) <= 17 for p in pages for line in p)


def test_reply_chaining_demotes_last_page() -> None:
    pager = CaptionPager()
    ev: list[tuple[float, object]] = []

    def poll(t: float) -> None:
        for e in pager.poll(t):
            ev.append((t, e))

    a = "Nice, Spotify is ready."                             # 23 chars -> 25 with wrap? measure below
    pager.begin_reply(0.0)
    pager.update(0.0, a, True)
    poll(0.0)
    chars_a = _chars(paginate(wrap(a, 17), 4)[0])
    assert isinstance(ev[0][1], ShowPage) and ev[0][1].final is True and ev[0][1].hold_ms == 6000
    pager.begin_reply(0.5)
    poll(0.5)
    b = "Sure, playing it now."
    pager.update(1.0, b, True)
    poll(1.0)
    expect = max(0.0 + _clamp(chars_a / 12 + 0.8, 2.0, 9.0), 0.0 + 2.0)
    t = 1.0
    while t < expect - 1e-9:
        t = round(t + 0.05, 6)
        poll(t)
        if t < expect - 1e-9:
            assert len(ev) == 1, "B's page appeared before A's demoted hold ran out"
    shows = [(t, e) for t, e in ev if isinstance(e, ShowPage)]
    assert len(shows) == 2
    tb, page_b = shows[1]
    assert tb == pytest.approx(expect, abs=0.05 + 1e-9)
    assert page_b.page == 0 and page_b.chirp is True and page_b.final is True and page_b.of == 1
    assert not any(isinstance(e, Clear) for _, e in ev)
    while t < tb + 6.0 + 0.1:
        t = round(t + 0.05, 6)
        poll(t)
    clears = [t for t, e in ev if isinstance(e, Clear)]
    assert len(clears) == 1 and clears[0] == pytest.approx(tb + 6.0, abs=0.05 + 1e-9)
    assert pager.busy is False


def test_reset_drops_queue_and_clears() -> None:
    pager = CaptionPager()
    assert pager.reset(0.0) == []
    pager.begin_reply(0.0)
    pager.update(0.0, "Ten past three.", True)
    assert [type(e) for e in pager.poll(0.0)] == [ShowPage]
    pager.begin_reply(0.2)
    pager.update(0.3, "And a queued reply.", True)
    assert pager.poll(1.0) == []
    assert pager.reset(1.0) == [Clear()]
    assert pager.busy is False and pager.poll(5.0) == []
    pager.update(6.0, "Fresh start ", False)
    ev = pager.poll(6.0)
    assert len(ev) == 1 and isinstance(ev[0], ShowPage) and ev[0].page == 0 and ev[0].chirp is True


def test_overflow_truncates_with_ellipsis(caplog: pytest.LogCaptureFixture) -> None:
    text = " ".join(f"word{i}" for i in range(72))          # ~ 500 chars
    assert 480 <= len(text) <= 520
    pager = CaptionPager()
    with caplog.at_level(logging.WARNING, logger="eve_bridge.caption_pager"):
        pager.begin_reply(0.0)
        for k in range(100, len(text), 100):
            pager.update(k / 100, text[:k], False)
        pager.update(6.0, text, True)
    pages = pager.replies[0].pages
    assert len(pages) == 6
    assert pages[-1][-1].endswith("…") and len(pages[-1][-1]) <= 17
    assert sum(1 for r in caplog.records if "longer than 6 pages" in r.getMessage()) == 1


def test_geometry_matches_firmware() -> None:
    main = (REPO / "firmware/claude_pet_stackchan/src/main.cpp").read_text()
    data = (REPO / "firmware/claude_pet_stackchan/src/data.h").read_text()
    cols = int(re.search(r"CAP_COLS = (\d+)", main).group(1))
    lines = int(re.search(r"CAP_LINES = (\d+)", main).group(1))
    m = re.search(r"CAP_MAX_LINES = (\d+), CAP_MAX_COLS = (\d+)", data)
    max_lines, max_cols = int(m.group(1)), int(m.group(2))
    assert (CFG.cols, CFG.lines) == (cols, lines)
    assert CFG.lines <= max_lines and CFG.cols <= max_cols


def test_caption_instructions_state_the_geometry() -> None:
    text = caption_instructions(CFG)
    assert "4 lines of 17 characters" in text and "68" in text and "under 100 characters" in text
    assert "three lines" not in text


def test_tool_only_response_while_a_page_is_held_does_not_strand_it() -> None:
    """response.created for a tool-only response queues an empty reply behind the page on
    screen; end_reply (response.done with no text) must drop it so the page on screen is the
    last page again and clears on time — otherwise it sits there until barge-in."""
    pg = CaptionPager()
    pg.begin_reply(0.0)
    pg.update(0.1, "Ten past three.", True)
    shows = pg.poll(0.1)
    assert shows and shows[-1].final is True
    clear_alone = pg._clear_at(0, 0)
    pg.begin_reply(0.5)                      # a second response starts (tool call only)
    assert not pg._is_last_overall(0, 0)   # the held page is no longer "last"
    pg.end_reply(0.6)                        # ... and ends with no text
    assert len(pg.replies) == 1 and pg._is_last_overall(0, 0) and pg._clear_at(0, 0) == clear_alone
    events = pg.poll(clear_alone + 0.01)
    assert any(isinstance(e, Clear) for e in events)
    assert not pg.busy


def test_last_page_clear_waits_for_the_freeze_dwell() -> None:
    """clear(last) = max(t_shown + hold_last, t_frozen + last_page_extra): a page that kept
    filling until late must still be readable for last_page_extra after its last change."""
    cfg = PagerConfig()
    pg = CaptionPager(cfg)
    pg.begin_reply(0.0)
    pg.update(0.1, "One", False)
    pg.poll(0.1)                                              # shown at 0.1
    pg.update(5.0, "One two three four five six", True)       # final text lands late, still one page
    pg.poll(5.0)
    deadline = pg._clear_at(0, 0)
    assert deadline is not None
    # invariants: never before the final text has been readable for last_page_extra, and never
    # before the hold a page of that length earns from the moment it was last (re)shown
    assert deadline >= 5.0 + cfg.last_page_extra_secs - 1e-9
    assert deadline >= 5.0 + cfg.last_page_min_secs - 1e-9
    assert not any(isinstance(e, Clear) for e in pg.poll(deadline - 0.05))
    assert any(isinstance(e, Clear) for e in pg.poll(deadline + 0.01))
