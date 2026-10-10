"""Every committed accessibility fixture under tests/fixtures/ax re-derives from its raw
walk dump with the current code (byte-equal snapshot), is untruncated and redacted,
names cases whose expected control exists, and sits in exactly one set."""

from __future__ import annotations

import json
import re as _re
from pathlib import Path

import pytest

from eve_bridge.ax_candidates import snapshot_from_dict, snapshot_from_raw

ROOT = Path(__file__).with_name("fixtures") / "ax"
SETS = ("select", "holdout")
FILES = sorted(p for s in SETS for p in (ROOT / s).glob("*.json")) if ROOT.is_dir() else []


def _load(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


@pytest.mark.skipif(not FILES, reason="no fixtures captured yet")
def test_no_fixture_is_in_both_sets() -> None:
    names = [p.stem for p in FILES]
    assert len(names) == len(set(names)), sorted(n for n in names if names.count(n) > 1)


@pytest.mark.parametrize("path", FILES, ids=[f"{p.parent.name}/{p.stem}" for p in FILES])
def test_fixture_rederives_from_its_raw_dump(path: Path) -> None:
    data = _load(path)
    captured = data["captured"]
    assert captured["truncated"] is False and data["raw"]["truncated"] is False
    assert captured["redacted"] is True, "fixtures carry synthetic labels only (README)"
    assert len(captured["ax_candidates_sha"]) == 64
    stored = snapshot_from_dict(data["snapshot"])
    screen = tuple(data["screen"]) if data.get("screen") else None
    derived = snapshot_from_raw(data["raw"], seq=stored.seq, screen=screen)
    assert derived.to_dict() == data["snapshot"], "the snapshot no longer matches its raw walk: re-derive it"
    assert not derived.truncated and derived.elements, "a fixture is a complete, non-empty snapshot"


@pytest.mark.parametrize("path", FILES, ids=[f"{p.parent.name}/{p.stem}" for p in FILES])
def test_fixture_cases_point_at_real_candidates(path: Path) -> None:
    data = _load(path)
    snap = snapshot_from_dict(data["snapshot"])
    assert data["cases"], "a fixture without cases measures nothing"
    for case in data["cases"]:
        assert isinstance(case.get("goal"), str) and case["goal"].strip()
        assert case.get("expected") in ("click", "abstain")
        assert isinstance(case.get("overlap"), bool) and isinstance(case.get("distractor"), bool)
        if case["expected"] == "abstain":
            assert case.get("expected_id") is None
        else:
            c = snap.get(str(case["expected_id"]))
            assert c is not None, f"{path.name}: {case['goal']!r} expects id {case['expected_id']!r}, not in the snapshot"
            assert c.label, "the expected control must be labelled"
        for forbidden in case.get("must_not_offer") or ():
            assert snap.get(str(forbidden)) is not None, f"must_not_offer id {forbidden!r} is not in the snapshot"


# ---- redaction denylist (review 2026-09-21): a fixture may not carry the owner's data --------------


_DENY = [_re.compile(p, _re.IGNORECASE) for p in (
    r"gurucharan", r"lingamallu", r"[\w.+-]+@(?!example\.)[\w-]+\.[a-z]{2,}",   # any non-example.* e-mail
    r"\+?1?\s?\(?\d{3}\)?[\s.-]\d{3}[\s.-]\d{4}",                            # US phone numbers
    r"era\.world", r"claude-501", r"CodePath", r"storeybox")]


def _denied(text: str) -> list[str]:
    return [p.pattern for p in _DENY if p.search(text)]


def test_denylist_catches_a_known_positive_control() -> None:
    bad = 'row "Gurucharan Lingamallu, Apple Account" field "guru.x@gmail.com" text "+1 (425) 555-0100"'
    hits = _denied(bad)
    assert any("gurucharan" in h for h in hits) and any("@" in h for h in hits) and any("d{3}" in h for h in hits)
    assert _denied('row "Sam Rivera, Apple Account" field "sam@example.org" text "phone redacted"') == []


def test_no_fixture_carries_owner_data() -> None:
    import glob
    files = sorted(glob.glob(str(ROOT / "*" / "*.json")))
    assert files, "no fixtures"
    for f in files:
        text = open(f, encoding="utf-8").read()
        assert _denied(text) == [], (f, _denied(text))
