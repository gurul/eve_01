"""The chief's proof: when a card is done, decided by code from evidence the executor did not write.

Every executor hands back one shape, ``PhaseResult`` (defined in chief_card.py beside the Phase that carries it,
re-exported here): a closed status, a list of evidence, and ``text``, the executor's own words. The oracles below
read only the evidence, and each says one word from plan_executor's closed set (plan_executor.py:20-25):

* ``confirmed``     the evidence shows it.
* ``unverifiable``  nothing could be read back, or what was read does not show it. Never counted as success.
* ``failed``        the evidence shows it did not happen, or the phase itself failed, was refused or ran out of
                    budget.

A card is ``done`` only when every one of its checks is confirmed AND every one of its phases ran ("ok"); one
unverifiable or failed check, or one step that failed, was refused or was skipped, makes it ``unverified``, never
done (design principle 4, "done means evidence"). The checks alone were not enough: a desk job checked only by
read_links and cited_pick closed done while its order had failed (reviewer, 2026-09-29). ``text`` never decides
anything: "I ordered it, confirmation #123" with no evidence is unverifiable.

The checks (chief_card.DONE_KINDS_LIVE) and what each reads:

* ``read_links(n)``  the distinct http(s) links the card's research phases read: the search's sources, the set
                     search_router.keep_read_links trusts (search_router.py:394, 470). n or more confirms.
* ``cited_pick``     the links the assess phase's pick cites, every one of them in that read set. A pick that
                     cites a link nobody read fails (the watcher's link lesson, WatchLink.lean).
* ``ui_seen(text)``  the UI-state text a Mac executor captured, never its closing sentence: Codex's
                     ``ui_evidence`` (codex_computer.py:385; an empty list means "not verified", 577-578). Only the
                     LAST state of each act is read, the screen the act ended on: a page seen before the act
                     ("Button: Order now" on the product page) proves nothing about it. The text is at least two
                     words (chief_card.MIN_UI_WORDS). Limit:
                     Codex's evidence is any js tool output holding "Window:" (codex_computer.py:381-385), which the
                     agent's own script could print; that is codex_computer's to close, not this oracle's.
* ``watch_armed``    the watcher's own row id, w<N> (watch.py:2642-2648, returned by Watcher.add, 2704); when the
                     caller lends the live watch ids, the row must be in them.
* ``file_exists``    a regular file below the home folder (never the home folder itself), looked at by this code,
                     made or changed at or after the card was: a file that was already there proves nothing.
* ``guru_says_done`` the owner's own Done tap or "done c<N>", matched by code. The receipt labels it "your call".

The receipt is composed by code from the verdicts, so it cannot claim more than they do.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping, Optional, Sequence

from .chief_card import (
    MAX_EVIDENCE,
    MAX_EVIDENCE_REF_CHARS,
    MAX_RESULT_TEXT_CHARS,
    MIN_UI_WORDS,
    Card,
    Check,
    Evidence,
    PhaseResult,
    home_path,
    is_watch_id,
)

__all__ = ["OUTCOMES", "PhaseResult", "Evidence", "Verdict", "read_set", "check", "verify",
           "close", "status_of", "card_status", "steps_not_run", "receipt", "from_search", "from_agent", "from_watch"]

OUTCOMES = ("confirmed", "unverifiable", "failed")
FAILED_RESULTS = ("refused", "failed", "over_budget")      # the phase itself did not happen
_URL = re.compile(r"^https?://[^\s/]+\.[^\s]+$", re.I)


@dataclass(frozen=True)
class Verdict:
    check: Check
    outcome: str                 # OUTCOMES
    why: str                     # one line for the receipt: what was read


def _link(ref: str) -> str:
    text = ref.strip().rstrip(".,;:!?")
    return text if _URL.match(text) else ""


def _results(card: Card, kind: str) -> list[PhaseResult]:
    return [p.result for p in card.phases if p.do == kind and p.result is not None]


def read_set(card: Card) -> set[str]:
    """The links the card's research phases read (done results only)."""
    out: set[str] = set()
    for r in _results(card, "research"):
        if r.status == "done":
            out.update(x for x in (_link(e.ref) for e in r.evidence if e.kind == "links") if x)
    return out


def _gate(check: Check, results: list[PhaseResult], kind: str) -> Optional[Verdict]:
    if not results:
        return Verdict(check, "unverifiable", f"no {kind} result yet")
    done = [r for r in results if r.status == "done"]
    if done:
        return None
    if any(r.status in FAILED_RESULTS for r in results):
        worst = next(r.status for r in results if r.status in FAILED_RESULTS)
        return Verdict(check, "failed", f"the {kind} phase {worst.replace('_', ' ')}")
    return Verdict(check, "unverifiable", f"the {kind} phase was handed on")


def check(check: Check, card: Card, *, owner_done: bool = False, live_watches: Optional[set[str]] = None,
          home: Optional[Path] = None) -> Verdict:
    """One check against the card's evidence. ``owner_done``: the owner tapped Done (code matched it).
    ``live_watches``: the watcher's current ids, when the caller has them. ``home``: the home folder (tests)."""
    if check.kind == "guru_says_done":
        return Verdict(check, "confirmed", "your call") if owner_done else Verdict(check, "unverifiable",
                                                                                   "waiting for your word")
    if check.kind == "file_exists":
        path = home_path(check.arg, home)
        if path is None:
            return Verdict(check, "failed", "not a file in the home folder")
        try:
            st = path.stat()
        except OSError:
            return Verdict(check, "failed", "the file is not there")
        if not path.is_file():
            return Verdict(check, "failed", "that is not a file")
        if st.st_mtime < card.created:
            return Verdict(check, "unverifiable", "the file is from before the job")
        return Verdict(check, "confirmed", "the file is there")
    if check.kind == "read_links":
        results = _results(card, "research")
        gate = _gate(check, results, "research")
        if gate:
            return gate
        n, got = int(check.arg or "1"), len(read_set(card))
        why = f"read {got} page{'s' if got != 1 else ''}"
        return Verdict(check, "confirmed" if got >= n else "failed", why)
    if check.kind == "cited_pick":
        results = _results(card, "assess")
        gate = _gate(check, results, "assess")
        if gate:
            return gate
        cited = {x for r in results if r.status == "done" for x in (_link(e.ref) for e in r.evidence
                                                                    if e.kind == "links") if x}
        if not cited:
            return Verdict(check, "failed", "the pick cites no page")
        unread = cited - read_set(card)
        if unread:
            return Verdict(check, "failed", f"the pick cites {len(unread)} page(s) I did not read")
        return Verdict(check, "confirmed", "the pick cites a page I read")
    if check.kind == "ui_seen":
        results = _results(card, "act")
        gate = _gate(check, results, "act")
        if gate:
            return gate
        want = " ".join(check.arg.casefold().split())
        if len(want.split()) < MIN_UI_WORDS:
            return Verdict(check, "unverifiable", f"'{check.arg}' is too short to prove anything")
        states = []
        for r in results:
            if r.status != "done":
                continue
            seen = [e.ref for e in r.evidence if e.kind == "ui_text" and e.ref.strip()]
            if seen:
                states.append(seen[-1])                    # the screen this act ended on
        if not states:
            return Verdict(check, "unverifiable", "no screen state was read back")
        if any(want in " ".join(s.casefold().split()) for s in states):
            return Verdict(check, "confirmed", f"'{check.arg}' seen on screen")
        return Verdict(check, "unverifiable", f"'{check.arg}' not seen on screen")
    if check.kind == "watch_armed":
        results = _results(card, "watch")
        gate = _gate(check, results, "watch")
        if gate:
            return gate
        rows = [e.ref.strip() for r in results if r.status == "done" for e in r.evidence if e.kind == "watch_row"]
        rows = [x for x in rows if is_watch_id(x)]
        if not rows:
            return Verdict(check, "failed", "no watch was set")
        if live_watches is not None and not any(x in live_watches for x in rows):
            return Verdict(check, "failed", f"watch {rows[0]} is not on the list")
        return Verdict(check, "confirmed", f"watch {rows[0]} is set")
    return Verdict(check, "unverifiable", "no oracle for this check")


def verify(card: Card, *, owner_done: bool = False, live_watches: Optional[set[str]] = None,
           home: Optional[Path] = None) -> tuple[Verdict, ...]:
    return tuple(check(c, card, owner_done=owner_done, live_watches=live_watches, home=home)
                 for c in card.done_checks)


def status_of(verdicts: Sequence[Verdict]) -> str:
    """"done" only when there is at least one verdict and every one is confirmed; else "unverified". The checks
    only: a card's status is ``card_status``."""
    return "done" if verdicts and all(v.outcome == "confirmed" for v in verdicts) else "unverified"


def steps_not_run(card: Card) -> list[tuple[int, str, str]]:
    """(n, kind, status) of each phase that did not run to "ok": failed, refused, skipped, or never finished."""
    return [(p.n, p.do, p.status) for p in card.phases if p.status != "ok"]


def card_status(card: Card, verdicts: Sequence[Verdict]) -> str:
    """"done" only when every check is confirmed and every phase ran ("ok"); else "unverified"."""
    return "done" if status_of(verdicts) == "done" and not steps_not_run(card) else "unverified"


def close(card: Card, *, owner_done: bool = False, live_watches: Optional[set[str]] = None,
          home: Optional[Path] = None) -> str:
    """The card's status once its phases have run: "done" or "unverified"."""
    return card_status(card, verify(card, owner_done=owner_done, live_watches=live_watches, home=home))


def _minutes(secs: float) -> str:
    return f"{max(1, round(secs / 60))} min" if secs >= 30 else f"{secs:.0f} s"


def receipt(card: Card, verdicts: Sequence[Verdict]) -> str:
    """"Done: standing desk, 3 of 3 checks: read 4 pages (confirmed); … $0.31, 12 min." Composed from the
    verdicts; an unverified card names what could not be confirmed and asks."""
    ok = sum(1 for v in verdicts if v.outcome == "confirmed")
    status = card_status(card, verdicts)
    head = "Done" if status == "done" else "Not confirmed"
    parts = "; ".join(f"{v.why} ({v.outcome})" for v in verdicts)
    steps = "".join(f" Step {n} ({do}) {st}." for n, do, st in steps_not_run(card))
    cost = f" ${card.spent.usd:.2f}, {_minutes(card.spent.wall_s)}." if card.spent.usd or card.spent.wall_s else ""
    text = f"{head}: {card.title}, {ok} of {len(verdicts)} checks: {parts}.{steps}{cost}"
    if status != "done":
        text += " Did it go through?"
    return text


# ---- adapters: an executor's output in, the one shape out ---------------------------------------

def _cap(text: Any, limit: int) -> str:
    return str(text or "")[:limit]


def _usd(value: Any) -> float:
    return float(value) if isinstance(value, (int, float)) and not isinstance(value, bool) and value > 0 else 0.0


def from_search(out: Mapping[str, Any], *, secs: float = 0.0) -> PhaseResult:
    """search_router.answer's result ({"ok", "answer", "sources": [{"url"}], "cost_usd"}) as a research result.
    The sources are the links read; the answer is text."""
    if not isinstance(out, Mapping) or not out.get("ok"):
        reason = out.get("reason") if isinstance(out, Mapping) else ""
        return PhaseResult("failed", (), _cap(reason, MAX_RESULT_TEXT_CHARS), 0.0, max(0.0, secs))
    links = []
    for s in out.get("sources") or []:
        url = _link(str(s.get("url") or "")) if isinstance(s, Mapping) else ""
        if url and url not in links:
            links.append(url)
    evidence = tuple(Evidence("links", u[:MAX_EVIDENCE_REF_CHARS]) for u in links[:MAX_EVIDENCE])
    return PhaseResult("done", evidence, _cap(out.get("answer"), MAX_RESULT_TEXT_CHARS), _usd(out.get("cost_usd")),
                       max(0.0, secs))


def from_agent(final: str, ui_evidence: Sequence[Mapping[str, Any]], *, ok: bool = True, usd: float = 0.0,
               secs: float = 0.0, act: bool = False) -> PhaseResult:
    """A Mac executor's run: its closing sentence as text, its captured UI states as evidence.
    ``ok`` False (the run failed, was stopped, or its agent reported an error) makes the result failed whatever it
    said. ``act``: an act that ran with no UI state seen is ``handed_on``, never done: Codex returns its failure
    sentence instead of raising (codex_computer.py:585-588), and the sentence alone is no evidence (reviewer after
    P4, 2026-09-29: such a run closed a card done)."""
    states = []
    for item in ui_evidence or []:
        state = str(item.get("state") or "") if isinstance(item, Mapping) else ""
        if state.strip():
            states.append(Evidence("ui_text", state[:MAX_EVIDENCE_REF_CHARS]))
    status = "failed" if not ok else "handed_on" if act and not states else "done"
    return PhaseResult(status, tuple(states[-MAX_EVIDENCE:]), _cap(final, MAX_RESULT_TEXT_CHARS), _usd(usd),
                       max(0.0, secs))


def from_watch(out: Mapping[str, Any]) -> PhaseResult:
    """Watcher.add's result ({"ok", "id": "w<N>", …}) as a watch result."""
    if isinstance(out, Mapping) and out.get("ok") and is_watch_id(str(out.get("id") or "")):
        return PhaseResult("done", (Evidence("watch_row", str(out["id"])),), _cap(out.get("watching_for"), 200))
    reason = out.get("reason") if isinstance(out, Mapping) else ""
    return PhaseResult("failed", (), _cap(reason, MAX_RESULT_TEXT_CHARS))
