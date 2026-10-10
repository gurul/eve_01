"""tools/chief_eval.py, the E1 capture eval (design 5.2 E1, 2026-09-29): its pre-registered bar, gate by gate at
each boundary; fail-closed on a case that could not be scored; the blind set scored once and only at its committed
hash; and --check-default (G7.3), which holds chief.SHIPPED to the kept verdict. All offline: the live brain is
only ever asked by ``--capture`` itself."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import sys
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import chief

TOOL = Path(__file__).parents[1] / "tools" / "chief_eval.py"


@pytest.fixture
def ev(monkeypatch: pytest.MonkeyPatch) -> Any:
    spec = importlib.util.spec_from_file_location("chief_eval", TOOL)
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    monkeypatch.setitem(sys.modules, "chief_eval", mod)             # its dataclasses look their module up
    spec.loader.exec_module(mod)
    return mod


def _cases(cards: int = 30, no_cards: int = 30) -> list[dict[str, Any]]:
    return ([{"text": f"job {i}", "label": "card", "acceptable_done": ["read_links", "cited_pick"]}
             for i in range(cards)] + [{"text": f"chat {i}", "label": "no_card"} for i in range(no_cards)])


def _rows(ev: Any, cases: list[dict[str, Any]], *, no_card_cards: int = 0, bad_done: int = 0, asking: int = 0,
          missed: int = 0, errors: tuple[int, ...] = ()) -> list[Any]:
    rows, card_seen, no_seen = [], 0, 0
    for i, c in enumerate(cases):
        err = "APIError" if i in errors else ""
        if c["label"] == "card":
            took = card_seen >= missed
            kinds = ("guru_says_done",) if took and card_seen - missed < bad_done else ("read_links",)
            q = took and card_seen - missed < asking
            rows.append(ev.Row(i, "card", took and not err, kinds if took and not err else (), q and not err,
                               "take_on" if took else "", "", err))
            card_seen += 1
        else:
            took = no_seen < no_card_cards
            rows.append(ev.Row(i, "no_card", took and not err, ("guru_says_done",) if took else (), False,
                               "take_on" if took else "", "", err))
            no_seen += 1
    return rows


def test_a_clean_run_ships_and_every_number_is_reported(ev: Any) -> None:
    cases = _cases()
    bar = ev.capture_bar(_rows(ev, cases), cases, 0)
    assert bar["shipped"] and bar["set_ok"] and bar["a"] and bar["b"] and bar["c"] and bar["d"]
    assert (bar["no_card_cards"], bar["done_ok"], bar["carded"], bar["asking"], bar["cards"]) == (0, 30, 30, 0, 30)


def test_a_cards_on_no_card_cases_pass_at_10_percent_and_fail_above(ev: Any) -> None:
    cases = _cases()
    assert ev.capture_bar(_rows(ev, cases, no_card_cards=3), cases, 0)["a"]            # 3 of 30 = 10%
    bar = ev.capture_bar(_rows(ev, cases, no_card_cards=4), cases, 0)                  # 13.3%
    assert not bar["a"] and not bar["shipped"]


def test_b_acceptable_done_kinds_pass_at_90_percent_and_fail_below(ev: Any) -> None:
    cases = _cases()
    assert ev.capture_bar(_rows(ev, cases, bad_done=3), cases, 0)["b"]                 # 27 of 30 = 90%
    assert not ev.capture_bar(_rows(ev, cases, bad_done=4), cases, 0)["b"]


def test_b_counts_only_card_cases_that_got_a_card_and_no_card_at_all_fails(ev: Any) -> None:
    cases = _cases()
    bar = ev.capture_bar(_rows(ev, cases, missed=10), cases, 0)                         # misses are not gated
    assert bar["b"] and bar["carded"] == 20 and len(bar["missed"]) == 10 and bar["shipped"]
    bar = ev.capture_bar(_rows(ev, cases, missed=30), cases, 0)
    assert not bar["b"] and not bar["shipped"]


def test_c_questions_pass_at_25_percent_and_fail_above(ev: Any) -> None:
    cases = _cases(cards=40, no_cards=20)
    assert ev.capture_bar(_rows(ev, cases, asking=10), cases, 0)["c"]                   # 10 of 40 = 25%
    assert not ev.capture_bar(_rows(ev, cases, asking=11), cases, 0)["c"]


def test_d_one_dispatch_without_a_go_fails(ev: Any) -> None:
    cases = _cases()
    bar = ev.capture_bar(_rows(ev, cases), cases, 1)
    assert not bar["d"] and not bar["shipped"]


def test_a_case_that_could_not_be_scored_counts_against_every_gate_it_touches(ev: Any) -> None:
    cases = _cases()
    bar = ev.capture_bar(_rows(ev, cases, errors=(40, 41, 42, 43)), cases, 0)           # four no_card errors
    assert bar["no_card_cards"] == 4 and not bar["a"] and bar["errors"] == [40, 41, 42, 43]
    bar = ev.capture_bar(_rows(ev, cases, errors=(0, 1, 2, 3)), cases, 0)               # four card errors
    assert bar["done_ok"] == 26 and bar["carded"] == 30 and not bar["b"] and bar["asking"] == 4


def test_a_set_under_60_or_under_20_of_a_label_never_ships(ev: Any) -> None:
    for cards, no_cards in ((29, 30), (45, 19)):
        cases = _cases(cards, no_cards)
        bar = ev.capture_bar(_rows(ev, cases), cases, 0)
        assert not bar["set_ok"] and not bar["shipped"]


def _data(tmp_path: Path, cases: list[dict[str, Any]], *, hash_of: bytes | None = None) -> Path:
    body = json.dumps({"cases": cases}).encode()
    (tmp_path / "capture_holdout.json").write_bytes(body)
    digest = hashlib.sha256(hash_of if hash_of is not None else body).hexdigest()
    (tmp_path / "capture_holdout.sha256").write_text(f"{digest}  capture_holdout.json\n")
    return tmp_path


def _kept(ev: Any, data: Path, rows: list[Any], *, shipped: bool, dispatched: int = 0) -> None:
    from dataclasses import asdict

    digest = hashlib.sha256((data / "capture_holdout.json").read_bytes()).hexdigest()
    (data / "capture_result.json").write_text(json.dumps(
        {"scored": "2026-09-29", "holdout_sha256": digest, "model": "m", "dispatched": dispatched, "usd": 0.0,
         "shipped": shipped, "rows": [asdict(r) for r in rows]}))


def test_check_default_before_a_scoring_holds_shipped_false(ev: Any, tmp_path: Path,
                                                            capsys: pytest.CaptureFixture) -> None:
    data = _data(tmp_path, _cases())
    assert ev.check_default(data, shipped=False) == 0
    assert "DEFAULT_CONSISTENT" in capsys.readouterr().out
    assert ev.check_default(data, shipped=True) == 1
    assert "DEFAULT_MISMATCH" in capsys.readouterr().out


def test_check_default_holds_shipped_to_the_bar_recomputed_from_the_kept_rows(ev: Any, tmp_path: Path,
                                                                             capsys: pytest.CaptureFixture) -> None:
    cases = _cases()
    data = _data(tmp_path, cases)
    _kept(ev, data, _rows(ev, cases), shipped=True)
    assert ev.check_default(data, shipped=True) == 0
    assert ev.check_default(data, shipped=False) == 1                                  # the flag must follow it
    _kept(ev, data, _rows(ev, cases, no_card_cards=5), shipped=True)                    # a verdict the rows refute
    capsys.readouterr()
    assert ev.check_default(data, shipped=True) == 1
    assert "RESULT_MISMATCH" in capsys.readouterr().out


def test_check_default_refuses_a_set_that_is_not_the_one_scored(ev: Any, tmp_path: Path,
                                                               capsys: pytest.CaptureFixture) -> None:
    cases = _cases()
    data = _data(tmp_path, cases)
    _kept(ev, data, _rows(ev, cases), shipped=True)
    cases[0]["acceptable_done"] = ["guru_says_done"]                                    # the labels changed after
    (data / "capture_holdout.json").write_text(json.dumps({"cases": cases}))
    assert ev.check_default(data, shipped=True) == 1
    assert "SHA_MISMATCH" in capsys.readouterr().out


def test_the_blind_set_is_scored_once_and_only_at_its_committed_hash(ev: Any, tmp_path: Path,
                                                                     capsys: pytest.CaptureFixture) -> None:
    cases = _cases()
    data = _data(tmp_path, cases, hash_of=b"another set")
    assert ev.capture(data) == 2                                                        # before any brain is built
    assert "SHA_MISMATCH" in capsys.readouterr().out
    data = _data(tmp_path, cases)
    _kept(ev, data, _rows(ev, cases), shipped=True)
    assert ev.capture(data) == 2
    assert "ALREADY_SCORED" in capsys.readouterr().out


def test_the_repo_default_matches_its_kept_verdict(ev: Any, capsys: pytest.CaptureFixture) -> None:
    """G7.3 inside the suite: chief.SHIPPED equals the capture verdict in tests/fixtures/chief (False before one)."""
    assert ev.check_default(ev.DATA_DIR) == 0, capsys.readouterr().out


def test_the_bar_is_written_beside_shipped_with_the_same_numbers(ev: Any) -> None:
    text = Path(chief.__file__).read_text(encoding="utf-8")
    at = text.index("\nSHIPPED = ")
    block = text[text.rindex("PRE-REGISTERED BAR", 0, at):at]
    assert "<= 10%" in block and ">= 90%" in block and "<= 25%" in block and "= 0" in block
    assert (ev.MAX_NO_CARD_CARDS, ev.MIN_DONE_OK, ev.MAX_ASKING, ev.MAX_UNGATED_ONE_WAY) == (0.10, 0.90, 0.25, 0)
    assert (ev.MIN_CASES, ev.MIN_EACH) == (60, 20)
