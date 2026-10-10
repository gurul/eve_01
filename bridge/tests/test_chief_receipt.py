"""The chief's oracles (chief_receipt.py): done means evidence the executor did not write.

Gate G2.1 of the chief design (2026-09-29): every done kind has one confirming fixture and one non-confirming
fixture; the executor's sentence alone is unverifiable; one unverifiable check makes the card unverified, never
done.
"""

from __future__ import annotations

import dataclasses
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import chief_card as cc
from eve_bridge import chief_receipt as cr
from eve_bridge.chief_card import Card, Check, Evidence, PhaseResult

READ = ("https://shop.example/desk-a", "https://review.example/desks", "https://shop.example/desk-b")


def job(checks: list[dict[str, Any]], phases: list[str]) -> Card:
    goals = {"research": "standing desks under $600", "assess": "pick one", "act": "order the picked desk",
             "watch": "the desk's price below $500"}
    out = cc.parse({"kind": "job", "title": "Standing desk", "purpose": "", "end_state": "one desk ordered",
                    "done_checks": checks, "never": [], "phases": [{"do": k, "goal": goals[k], "door": None}
                                                                  for k in phases],
                    "deadline": None, "cue": None, "firm": False, "question": None},
                   said="find a standing desk and order it")
    assert isinstance(out, Card), out
    return dataclasses.replace(out, id="c1")


def with_results(card: Card, **results: PhaseResult) -> Card:
    return dataclasses.replace(card, phases=tuple(
        dataclasses.replace(p, result=results[p.do], status="ok") if p.do in results else p for p in card.phases))


def links(*urls: str, status: str = "done") -> PhaseResult:
    return PhaseResult(status, tuple(Evidence("links", u) for u in urls), text="an answer")


def ui(*states: str, status: str = "done", text: str = "") -> PhaseResult:
    return PhaseResult(status, tuple(Evidence("ui_text", s) for s in states), text=text)


def one(card: Card, **kw: Any) -> cr.Verdict:
    (verdict,) = cr.verify(card, **kw)
    return verdict


# ---- one confirming and one non-confirming fixture per done kind --------------------------------

def test_read_links() -> None:
    c = job([{"kind": "read_links", "arg": 3}], ["research"])
    assert one(with_results(c, research=links(*READ))).outcome == "confirmed"
    short = one(with_results(c, research=links(*READ[:2])))
    assert short.outcome == "failed" and short.why == "read 2 pages"
    assert one(with_results(c, research=links(READ[0], READ[0], READ[0] + "."))).outcome == "failed"   # distinct
    assert one(with_results(c, research=links("not a url", "ftp://x.example/a", READ[0]))).outcome == "failed"
    assert one(c).outcome == "unverifiable"                                            # nothing ran yet


def test_cited_pick() -> None:
    c = job([{"kind": "cited_pick", "arg": None}], ["research", "assess"])
    good = with_results(c, research=links(*READ), assess=links(READ[1]))
    assert one(good).outcome == "confirmed"
    unread = one(with_results(c, research=links(*READ), assess=links("https://elsewhere.example/desk")))
    assert unread.outcome == "failed" and "did not read" in unread.why
    assert one(with_results(c, research=links(*READ), assess=links())).outcome == "failed"      # cites nothing
    # a research phase that failed read nothing, so no pick can cite it
    assert one(with_results(c, research=links(*READ, status="failed"), assess=links(READ[1]))).outcome == "failed"


def test_ui_seen() -> None:
    c = job([{"kind": "ui_seen", "arg": "Order confirmed"}], ["act"])
    seen = one(with_results(c, act=ui("Window: Checkout\nOrder   confirmed. Thank you!")))
    assert seen.outcome == "confirmed"
    assert one(with_results(c, act=ui("Window: Cart\n1 item"))).outcome == "unverifiable"
    assert one(with_results(c, act=ui())).outcome == "unverifiable"
    assert one(with_results(c, act=ui("Order confirmed", status="refused"))).outcome == "failed"


def test_watch_armed() -> None:
    c = job([{"kind": "watch_armed", "arg": None}], ["watch"])
    row = PhaseResult("done", (Evidence("watch_row", "w3"),))
    assert one(with_results(c, watch=row)).outcome == "confirmed"
    assert one(with_results(c, watch=row), live_watches={"w3", "w4"}).outcome == "confirmed"
    assert one(with_results(c, watch=row), live_watches={"w4"}).outcome == "failed"      # removed since
    assert one(with_results(c, watch=PhaseResult("done", (Evidence("watch_row", "3"),)))).outcome == "failed"
    assert one(with_results(c, watch=PhaseResult("done", ()))).outcome == "failed"


def test_file_exists(tmp_path: Path) -> None:
    c = job([{"kind": "file_exists", "arg": "~/Desktop/bom.csv"}], ["research"])
    assert one(c, home=tmp_path).outcome == "failed"
    (tmp_path / "Desktop").mkdir()
    (tmp_path / "Desktop" / "bom.csv").write_text("x")
    assert one(c, home=tmp_path).outcome == "confirmed"
    outside = dataclasses.replace(c, done_checks=(Check("file_exists", "/etc/hosts"),))
    assert one(outside, home=tmp_path).outcome == "failed"                               # never outside home


def test_guru_says_done() -> None:
    c = job([], ["research"])
    assert [x.kind for x in c.done_checks] == ["guru_says_done"]
    assert one(c).outcome == "unverifiable"
    yes = one(c, owner_done=True)
    assert yes.outcome == "confirmed" and yes.why == "your call"


def test_every_live_done_kind_has_an_oracle() -> None:
    covered = {"read_links", "cited_pick", "ui_seen", "watch_armed", "file_exists", "guru_says_done"}
    assert covered == set(cc.DONE_KINDS_LIVE)
    for kind in cc.DONE_KINDS_LIVE:                  # none falls through to "no oracle"
        v = cr.check(Check(kind, "1" if kind == "read_links" else "x"), job([], ["research"]))
        assert v.why != "no oracle for this check", kind


# ---- the executor's word is never evidence ------------------------------------------------------

def test_the_executors_sentence_alone_is_unverifiable() -> None:
    c = job([{"kind": "ui_seen", "arg": "order confirmed"}], ["act"])
    claimed = with_results(c, act=ui(text="I ordered the desk. Order confirmed, #12345."))
    assert one(claimed).outcome == "unverifiable"
    assert cr.close(claimed) == "unverified"
    # the research answer text names a link it never read: not a read link
    r = job([{"kind": "read_links", "arg": 1}], ["research"])
    said_only = with_results(r, research=PhaseResult("done", (), text="see https://shop.example/desk-a"))
    assert one(said_only).outcome == "failed" and cr.read_set(said_only) == set()


def test_one_unverifiable_check_makes_the_card_unverified_never_done() -> None:
    c = job([{"kind": "read_links", "arg": 3}, {"kind": "cited_pick", "arg": None},
             {"kind": "ui_seen", "arg": "order confirmed"}], ["research", "assess", "act"])
    all_good = with_results(c, research=links(*READ), assess=links(READ[0]), act=ui("Order confirmed"))
    verdicts = cr.verify(all_good)
    assert [v.outcome for v in verdicts] == ["confirmed"] * 3 and cr.close(all_good) == "done"
    one_short = with_results(c, research=links(*READ), assess=links(READ[0]), act=ui("Shipping details"))
    assert [v.outcome for v in cr.verify(one_short)] == ["confirmed", "confirmed", "unverifiable"]
    assert cr.close(one_short) == "unverified"
    assert cr.status_of(()) == "unverified"                                      # no checks is not done


@pytest.mark.parametrize("status", ["handed_on", "needs_guru"])
def test_a_handed_on_phase_is_unverifiable(status: str) -> None:
    c = job([{"kind": "ui_seen", "arg": "order placed"}], ["act"])
    assert one(with_results(c, act=ui("order placed", status=status))).outcome == "unverifiable"


@pytest.mark.parametrize("status", ["failed", "refused", "over_budget"])
def test_a_phase_that_did_not_happen_fails_its_check(status: str) -> None:
    c = job([{"kind": "read_links", "arg": 1}], ["research"])
    v = one(with_results(c, research=links(*READ, status=status)))
    assert v.outcome == "failed" and status.replace("_", " ") in v.why


# ---- the receipt and the adapters ---------------------------------------------------------------

def test_the_receipt_is_composed_from_the_verdicts() -> None:
    c = job([{"kind": "read_links", "arg": 3}, {"kind": "cited_pick", "arg": None},
             {"kind": "ui_seen", "arg": "Order confirmed"}], ["research", "assess", "act"])
    done = with_results(c, research=links(*READ, "https://shop.example/desk-c"), assess=links(READ[0]),
                        act=ui("Order confirmed"))
    done = dataclasses.replace(done, spent=cc.Spent(0.31, 720))
    assert cr.receipt(done, cr.verify(done)) == (
        "Done: Standing desk, 3 of 3 checks: read 4 pages (confirmed); the pick cites a page I read (confirmed); "
        "'Order confirmed' seen on screen (confirmed). $0.31, 12 min.")
    short = with_results(c, research=links(*READ), assess=links(READ[0]), act=ui("Cart"))
    text = cr.receipt(short, cr.verify(short))
    assert text.startswith("Not confirmed: Standing desk, 2 of 3 checks") and text.endswith("Did it go through?")
    assert "'Order confirmed' not seen on screen (unverifiable)" in text


def test_from_search_takes_the_sources_as_the_read_links() -> None:
    out = {"ok": True, "answer": "Desk A is best (https://made-up.example/x)", "cost_usd": 0.004,
           "sources": [{"title": "A", "url": READ[0]}, {"title": "A again", "url": READ[0]},
                       {"title": "bad", "url": "javascript:alert(1)"}, {"title": "B", "url": READ[1]}]}
    r = cr.from_search(out, secs=4.2)
    assert r.status == "done" and [e.ref for e in r.evidence] == [READ[0], READ[1]]
    assert r.usd == 0.004 and r.secs == 4.2 and "made-up" in r.text
    assert cr.from_search({"ok": False, "reason": "the search did not answer it"}).status == "failed"


def test_from_agent_and_from_watch() -> None:
    r = cr.from_agent("Done.", [{"call_id": "a", "state": "Window: Shop\nOrder confirmed"}, {"state": " "}], usd=0.1,
                      secs=30)
    assert r.status == "done" and [e.kind for e in r.evidence] == ["ui_text"]
    assert cr.from_agent("stopped", [{"state": "Window: Shop"}], ok=False).status == "failed"
    # an act that saw no screen is handed on, never done on its sentence (reviewer after P4, 2026-09-29)
    assert cr.from_agent("Codex ended without a result.", [], act=True).status == "handed_on"
    assert cr.from_agent("I ordered it.", [{"state": " "}], act=True).status == "handed_on"
    assert cr.from_agent("Done.", [{"state": "Window: Shop\nOrder confirmed"}], act=True).status == "done"
    assert cr.from_agent("Desk A is $549.", []).status == "done"                     # control: not an act
    assert cr.from_watch({"ok": True, "id": "w12", "watching_for": "below $500"}).evidence == (
        Evidence("watch_row", "w12"),)
    assert cr.from_watch({"ok": False, "reason": "no such symbol"}).status == "failed"
    assert cr.from_watch({"ok": True, "id": "12"}).status == "failed"


def test_a_stored_result_is_checked_strictly() -> None:
    ok = cc.result_from({"status": "done", "evidence": [{"kind": "links", "ref": READ[0]}], "text": "", "usd": 0,
                         "secs": 1})
    assert ok.evidence == (Evidence("links", READ[0]),)
    for bad in ({"status": "finished"}, {"status": "done", "evidence": [{"kind": "vibes", "ref": "x"}]},
                {"status": "done", "usd": -1}, {"status": "done", "evidence": "x"}):
        with pytest.raises(cc.CardError):
            cc.result_from(bad)


# ---- the reviewer's attacks after P3 (2026-09-29): each was reproduced first, then fixed ----------------------

def test_attack_a_failed_refused_or_skipped_step_makes_the_card_unverified() -> None:
    c = job([{"kind": "read_links", "arg": 1}], ["research", "act"])
    good = with_results(c, research=links(*READ))
    for status in ("failed", "refused", "skipped"):
        bad = dataclasses.replace(good, phases=tuple(dataclasses.replace(p, status=status) if p.do == "act" else p
                                                     for p in good.phases))
        verdicts = cr.verify(bad)
        assert [v.outcome for v in verdicts] == ["confirmed"]
        assert cr.close(bad) == "unverified" and cr.card_status(bad, verdicts) == "unverified"
        text = cr.receipt(bad, verdicts)
        assert text.startswith("Not confirmed:") and f"Step 2 (act) {status}." in text
    ran = dataclasses.replace(good, phases=tuple(dataclasses.replace(p, status="ok") for p in good.phases))
    assert cr.close(ran) == "done"                                              # control


def test_attack_ui_seen_reads_only_the_screen_each_act_ended_on() -> None:
    c = job([{"kind": "ui_seen", "arg": "order confirmed"}], ["act"])
    before = "Window: Shop\nOrder confirmed within 2 days, or your money back"      # a product page, before the act
    after = "Window: Checkout\nPayment declined"
    assert one(with_results(c, act=ui(before, after))).outcome == "unverifiable"
    assert one(with_results(c, act=ui(after, "Window: Checkout\nOrder confirmed #12"))).outcome == "confirmed"


def test_attack_ui_seen_needs_at_least_two_words() -> None:
    args = {"kind": "job", "title": "Desk", "purpose": "", "end_state": "ordered",
            "done_checks": [{"kind": "ui_seen", "arg": "order"}], "never": [],
            "phases": [{"do": "act", "goal": "order the desk", "door": None}], "deadline": None, "cue": None,
            "firm": False, "question": None}
    refused = cc.parse(args, said="order the desk")
    assert isinstance(refused, cc.Refusal) and "two words" in refused.reason
    c = job([{"kind": "ui_seen", "arg": "order placed"}], ["act"])
    one_word = dataclasses.replace(c, done_checks=(Check("ui_seen", "order"),))       # a stored card, hand-edited
    assert one(with_results(one_word, act=ui("Window: Shop\nOrder now"))).outcome == "unverifiable"


def test_attack_file_exists_needs_a_file_below_home_made_or_changed_since_the_card(tmp_path: Path) -> None:
    import os

    args = {"kind": "job", "title": "Lamps", "purpose": "", "end_state": "read",
            "done_checks": [{"kind": "file_exists", "arg": "~"}], "never": [],
            "phases": [{"do": "research", "goal": "desk lamps", "door": None}], "deadline": None, "cue": None,
            "firm": False, "question": None}
    assert isinstance(cc.parse(args, said="look up desk lamps", home=tmp_path), cc.Refusal)
    c = job([{"kind": "file_exists", "arg": "~/Desktop/bom.csv"}], ["research"])
    c = dataclasses.replace(c, created=1_790_000_000.0)
    home_itself = dataclasses.replace(c, done_checks=(Check("file_exists", "~"),))
    assert one(home_itself, home=tmp_path).outcome == "failed"
    (tmp_path / "Desktop" / "bom.csv").mkdir(parents=True)                           # a folder is not a file
    assert one(c, home=tmp_path).outcome == "failed"
    (tmp_path / "Desktop" / "bom.csv").rmdir()
    f = tmp_path / "Desktop" / "bom.csv"
    f.write_text("x")
    os.utime(f, (c.created - 60, c.created - 60))                                    # there before the card
    old = one(c, home=tmp_path)
    assert old.outcome == "unverifiable" and "before" in old.why
    os.utime(f, (c.created + 60, c.created + 60))
    assert one(c, home=tmp_path).outcome == "confirmed"                              # control
