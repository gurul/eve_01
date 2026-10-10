"""Caption pager: eve's text replies, one readable page at a time.

The robot's screen has room for 4 lines of 17 characters under the eyes. A
reply streams in word by word from the model; this module turns the running
text into pages and decides *when* each page goes up and how long it stays,
at a reading pace rather than the model's writing pace:

    hold(page)    = clamp(chars / read_cps + lead-if-first-page, 2 s, 9 s)
    deadline(i)   = max(t_shown + hold, t_frozen + 2 s)      -> next page shown here
    clear(last)   = max(t_shown + clamp(chars / read_cps + lead + 3 s, 6 s, 10 s), t_frozen + 3 s)

Page 0 of a reply fills word by word as the text arrives (refills throttled to
one per 0.15 s); a page is *frozen* once a later page has content or the reply
is final, and only a frozen page can turn. A reply that starts while a page is
still up queues behind it (the page on screen loses its last-page hold); a
barge-in drops everything. Every number lives in `PagerConfig`.

Pure: no asyncio, no wall clock, no I/O. The caller passes a monotonic `now`
and sends the returned events to the board (`voice_agent.py`).
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Optional, Union

log = logging.getLogger(__name__)

ELLIPSIS = "…"


@dataclass(frozen=True)
class PagerConfig:
    cols: int = 17                       # size-3 Font0 cells: 7 + 17*18 = 313 px <= 314
    lines: int = 4                       # band y 112..204 at 23 px per line
    read_cps: float = 12.0               # BBC 15-17 cps discounted for a 2" panel; env EVE_CAPTION_CPS
    first_page_lead_secs: float = 0.8    # attention latency after the chirp (page 0 of a reply only)
    min_hold_secs: float = 2.0           # a page flip must register as an event
    max_hold_secs: float = 9.0
    last_page_extra_secs: float = 3.0
    last_page_min_secs: float = 6.0
    last_page_max_secs: float = 10.0
    freeze_dwell_secs: float = 2.0       # a page's last line stays >= this after it stops changing
    refill_interval_secs: float = 0.15   # page-0 refills throttled
    max_pages: int = 6                   # per reply; beyond it the text is dropped with an ellipsis
    reply_budget_chars: int = 100        # what the model is asked to stay under (a 2-page budget, rounded)


# ---- pure text helpers -------------------------------------------------------------------

def wrap(text: str, cols: int) -> list[str]:
    """Greedy word wrap of the whitespace-normalised text. A word longer than
    `cols` is hard-split. No line exceeds `cols`, none has leading/trailing
    spaces, "" -> []. Prefix-stable: appending words never changes earlier lines."""
    lines: list[str] = []
    cur = ""
    for word in text.split():
        while len(word) > cols:
            if cur:
                lines.append(cur)
                cur = ""
            lines.append(word[:cols])
            word = word[cols:]
        if not cur:
            cur = word
        elif len(cur) + 1 + len(word) <= cols:
            cur = f"{cur} {word}"
        else:
            lines.append(cur)
            cur = word
    if cur:
        lines.append(cur)
    return lines


def complete_prefix(text: str, final: bool) -> str:
    """The part of a streaming text that ends on a whole word: everything up to
    and including the last whitespace ("Ten past thr" -> "Ten past "); the whole
    text when final."""
    if final:
        return text
    for i in range(len(text) - 1, -1, -1):
        if text[i].isspace():
            return text[: i + 1]
    return ""


def paginate(lines: list[str], per_page: int) -> list[tuple[str, ...]]:
    return [tuple(lines[i:i + per_page]) for i in range(0, len(lines), per_page)]


def _clamp(x: float, lo: float, hi: float) -> float:
    return max(lo, min(hi, x))


def page_hold_secs(chars: int, cfg: PagerConfig, *, first: bool, last: bool) -> float:
    """How long a page of `chars` characters stays up, from the moment it is shown."""
    secs = chars / cfg.read_cps + (cfg.first_page_lead_secs if first else 0.0)
    if last:
        return _clamp(secs + cfg.last_page_extra_secs, cfg.last_page_min_secs, cfg.last_page_max_secs)
    return _clamp(secs, cfg.min_hold_secs, cfg.max_hold_secs)


def caption_instructions(cfg: PagerConfig) -> str:
    """The extra session instructions for captions mode (voice_agent.py)."""
    per_page = cfg.lines * cfg.cols
    return (
        "\nYour replies are not spoken: they appear as text on your own small screen, one page at a time. "
        f"A page is\n{cfg.lines} lines of {cfg.cols} characters (about {per_page} characters) and stays up "
        "long enough to read; one page is ideal, two\nis the most. Keep every reply under "
        f"{cfg.reply_budget_chars} characters: plain words, short sentences, no emoji, no lists,\n"
        "no line breaks."
    )


# ---- events ---------------------------------------------------------------------------------

@dataclass(frozen=True)
class ShowPage:
    page: int                      # index within its reply
    of: int                        # page count of the reply, 0 while the reply is still streaming
    lines: tuple[str, ...]
    hold_ms: int                   # the host's planned hold from now; the board self-clears at hold + grace
    chirp: bool                    # True on a page turn (one talk chirp per page), False on a refill
    final: bool                    # last page of a final reply

    def to_wire(self) -> dict:
        return {"cmd": "caption", "page": self.page, "of": self.of, "lines": list(self.lines),
                "hold_ms": self.hold_ms, "chirp": self.chirp, "final": self.final}


@dataclass(frozen=True)
class Clear:
    def to_wire(self) -> dict:
        return {"cmd": "caption", "clear": True}


Event = Union[ShowPage, Clear]


# ---- the pager --------------------------------------------------------------------------------

@dataclass
class _Reply:
    text: str = ""
    final: bool = False
    pages: list[tuple[str, ...]] = field(default_factory=list)
    t_shown: list[Optional[float]] = field(default_factory=list)
    t_frozen: list[Optional[float]] = field(default_factory=list)
    overflow_warned: bool = False


class CaptionPager:
    """See the module docstring. `poll(now)` drives everything; the caller
    polls at its tick and after every `update`."""

    def __init__(self, cfg: PagerConfig = PagerConfig()) -> None:
        self.cfg = cfg
        self.replies: list[_Reply] = []
        self.cur: tuple[int, int] = (0, 0)     # (reply index, page index) of the page that should be up
        self._cur_shown = False                # False while cur waits for its first line (no blank page)
        self.last_refill_at = float("-inf")
        self.last_sent: Optional[ShowPage] = None

    # -- public state --
    @property
    def busy(self) -> bool:
        return self.last_sent is not None

    @property
    def visible(self) -> Optional[ShowPage]:
        return self.last_sent

    # -- input --
    def begin_reply(self, now: float) -> None:
        """response.created. Busy: the new reply queues behind the page on screen
        (whose last-page hold is thereby demoted to the normal formula). Idle: nothing
        happens until text arrives. An empty reply that never got text is reused."""
        if self.replies and not self.replies[-1].text and not self.replies[-1].final:
            return
        self.replies.append(_Reply())
        if len(self.replies) == 1:
            self.cur, self._cur_shown = (0, 0), False

    def update(self, now: float, text: str, final: bool) -> None:
        """The full text of the current (newest) reply so far."""
        if not self.replies:
            if not text:
                return
            self.replies = [_Reply()]
            self.cur, self._cur_shown = (0, 0), False
        reply = self.replies[-1]
        if reply.final and not final:
            return                                   # a stray delta after the done: keep the final text
        reply.text = text
        reply.final = final
        self._repaginate(reply, now)
        if final and not reply.pages:
            self._drop_empty_tail()                  # a reply with nothing to show (tool call only)

    def end_reply(self, now: float) -> None:
        """response.done with no text.done seen: finalise whatever the reply has."""
        if self.replies and not self.replies[-1].final:
            self.update(now, self.replies[-1].text, True)

    def reset(self, now: float) -> list[Event]:
        """Barge-in / hush: drop everything; a Clear iff something is on screen."""
        events: list[Event] = [Clear()] if self.last_sent is not None else []
        self._drop_all()
        return events

    # -- output --
    def poll(self, now: float) -> list[Event]:
        """Refills, page turns and the final clear that are due at `now`, in order."""
        events: list[Event] = []
        while True:
            e = self._step(now)
            if e is None:
                return events
            events.append(e)
            if isinstance(e, Clear):
                return events

    # -- internals --
    def _drop_all(self) -> None:
        self.replies = []
        self.cur, self._cur_shown = (0, 0), False
        self.last_refill_at = float("-inf")
        self.last_sent = None

    def _drop_empty_tail(self) -> None:
        """The newest reply is final with no pages: forget it. If it was the page
        the pager was waiting to turn to, the page on screen becomes last again."""
        r = len(self.replies) - 1
        self.replies.pop()
        if self.cur[0] != r:
            return
        if r == 0:
            self._drop_all()
        else:
            self.cur, self._cur_shown = (r - 1, len(self.replies[r - 1].pages) - 1), True

    def _repaginate(self, reply: _Reply, now: float) -> None:
        cfg = self.cfg
        pages = paginate(wrap(complete_prefix(reply.text, reply.final), cfg.cols), cfg.lines)
        if len(pages) > cfg.max_pages:
            pages = pages[: cfg.max_pages]
            last = list(pages[-1])
            tail = last[-1]
            last[-1] = (tail if len(tail) < cfg.cols else tail[: cfg.cols - 1]) + ELLIPSIS
            pages[-1] = tuple(last)
            if not reply.overflow_warned:
                reply.overflow_warned = True
                log.warning("captions: reply longer than %d pages; the tail is dropped", cfg.max_pages)
        reply.pages = pages
        while len(reply.t_shown) < len(pages):
            reply.t_shown.append(None)
            reply.t_frozen.append(None)
        for i in range(len(pages)):
            if reply.t_frozen[i] is None and (i < len(pages) - 1 or reply.final):
                reply.t_frozen[i] = now

    def _is_last_overall(self, r: int, p: int) -> bool:
        reply = self.replies[r]
        return r == len(self.replies) - 1 and reply.final and p == len(reply.pages) - 1

    def _chars(self, r: int, p: int) -> int:
        return sum(len(line) for line in self.replies[r].pages[p])

    def _deadline(self, r: int, p: int) -> float:
        reply = self.replies[r]
        hold = page_hold_secs(self._chars(r, p), self.cfg, first=p == 0, last=False)
        shown, frozen = reply.t_shown[p], reply.t_frozen[p]
        assert shown is not None and frozen is not None
        return max(shown + hold, frozen + self.cfg.freeze_dwell_secs)

    def _clear_at(self, r: int, p: int) -> float:
        reply = self.replies[r]
        hold = page_hold_secs(self._chars(r, p), self.cfg, first=p == 0, last=True)
        shown, frozen = reply.t_shown[p], reply.t_frozen[p]
        assert shown is not None and frozen is not None
        return max(shown + hold, frozen + self.cfg.last_page_extra_secs)

    def _page_changed(self, reply: _Reply, p: int) -> bool:
        sent = self.last_sent
        if sent is None:
            return True
        return (sent.lines != reply.pages[p] or sent.final != self._final_flag(reply, p)
                or sent.of != self._of(reply))

    @staticmethod
    def _of(reply: _Reply) -> int:
        return len(reply.pages) if reply.final else 0

    @staticmethod
    def _final_flag(reply: _Reply, p: int) -> bool:
        return reply.final and p == len(reply.pages) - 1

    def _show(self, r: int, p: int, now: float, chirp: bool) -> ShowPage:
        reply = self.replies[r]
        shown = reply.t_shown[p]
        assert shown is not None
        if reply.t_frozen[p] is not None:
            planned = self._clear_at(r, p) if self._is_last_overall(r, p) else self._deadline(r, p)
        else:
            planned = shown + page_hold_secs(self._chars(r, p), self.cfg, first=p == 0, last=False)
        ev = ShowPage(page=p, of=self._of(reply), lines=reply.pages[p],
                      hold_ms=int(round(1000 * max(0.0, planned - now))), chirp=chirp,
                      final=self._final_flag(reply, p))
        self.last_sent = ev
        self.last_refill_at = now
        self._cur_shown = True
        return ev

    def _step(self, now: float) -> Optional[Event]:
        if not self.replies:
            return None
        r, p = self.cur
        reply = self.replies[r]
        if not self._cur_shown:                      # first show of this page, once it has a line
            if p < len(reply.pages):
                reply.t_shown[p] = now
                return self._show(r, p, now, chirp=True)
            return None
        frozen = reply.t_frozen[p] is not None
        if self._page_changed(reply, p) and (frozen or now - self.last_refill_at >= self.cfg.refill_interval_secs - 1e-9):
            return self._show(r, p, now, chirp=False)   # a refill that freezes the page skips the throttle
        if not frozen:
            return None
        if self._is_last_overall(r, p):
            if now >= self._clear_at(r, p):
                self._drop_all()
                return Clear()
            return None
        if now < self._deadline(r, p):
            return None
        # turn the page: next page of this reply, else page 0 of the next reply
        if p + 1 < len(reply.pages):
            self.cur = (r, p + 1)
        elif r + 1 < len(self.replies):
            self.cur = (r + 1, 0)
        else:
            return None                              # the reply is still streaming and its next page is empty
        self._cur_shown = False
        return self._step(now)                       # shows it now if it has content, else waits
