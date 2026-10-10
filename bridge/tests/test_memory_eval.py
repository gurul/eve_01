"""tools/memory_eval.py, the memory eval (memory plan section 5): scoring by code (normalization, choice keys,
abstention, leak keys), the split rule, the seal, the temp-folder guard with a positive control, memory as of the
question, and forget running on a copy, never on the build. Offline, on a tiny synthetic dataset written here,
plus checks of the real fixture (tests/fixtures/memory_eval) against the scorer: right answers pass, wrong fail."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import sys
from datetime import datetime
from pathlib import Path
from typing import Any

import pytest

TOOL = Path(__file__).parents[1] / "tools" / "memory_eval.py"
TZ = "-07:00"


@pytest.fixture
def ev(monkeypatch: pytest.MonkeyPatch) -> Any:
    spec = importlib.util.spec_from_file_location("memory_eval", TOOL)
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    monkeypatch.setitem(sys.modules, "memory_eval", mod)            # its dataclasses look their module up
    spec.loader.exec_module(mod)
    return mod


def _line(day: str, hhmm: str, conv: str, who: str, text: str, ch: str = "telegram") -> str:
    return json.dumps({"ts": f"{day}T{hhmm}:00{TZ}", "ch": ch, "conv": conv, "who": who, "kind": "say",
                       "text": text})


def write_fake(root: Path) -> Path:
    """A three-day synthetic persona (invented names, example.com) with five questions and a seal."""
    (root / "transcripts").mkdir(parents=True)
    (root / "persona.json").write_text(json.dumps({"name": "Mira Testcase", "email": "mira@example.com",
                                                   "city": "Exampleville"}), encoding="utf-8")
    days = {
        "2026-08-01": [_line("2026-08-01", "09:15", "t-1785600000000-ab12", "owner",
                             "My sister Juno is visiting on October 3, 2026."),
                       _line("2026-08-01", "09:16", "t-1785600000000-ab12", "eve", "Noted, I will remember Juno.")],
        "2026-08-02": [_line("2026-08-02", "18:40", "v-1785690000000-cd34", "owner",
                             "I baked a plum tart for the neighbours.", ch="voice"),
                       _line("2026-08-02", "18:41", "v-1785690000000-cd34", "eve", "That sounds lovely.",
                             ch="voice")],
        "2026-08-03": [_line("2026-08-03", "12:00", "t-1785780000000-ef56", "owner",
                             "The gym membership costs 1,200 a year.")],
    }
    for day, rows in days.items():
        (root / "transcripts" / f"{day}.jsonl").write_text("\n".join(rows) + "\n", encoding="utf-8")
    questions = {
        "split_rule": "dev when the first hex digit of sha256(str(id)) is 0-4",
        "questions": [
            {"id": "q1", "category": "temporal", "asked_on": "2026-08-10", "question": "When is Juno visiting?",
             "answer_keys": ["2026-10-03"], "abstain": False, "evidence": [{"day": "2026-08-01", "line": 0}]},
            {"id": "q2", "category": "abstention", "asked_on": "2026-08-10", "question": "What is my dog called?",
             "answer_keys": [], "abstain": True, "evidence": []},
            {"id": "q3", "category": "extraction", "asked_on": "2026-08-10", "question": "What did I bake?",
             "answer_keys": ["plum", "tart"], "abstain": False, "evidence": [{"day": "2026-08-02", "line": 0}]},
            {"id": "q4", "category": "forget", "asked_on": "2026-08-10", "question": "What did I bake?",
             "answer_keys": [], "abstain": True, "evidence": [{"day": "2026-08-02", "line": 0}],
             "forget": {"day": "2026-08-02", "line": 0}, "leak_keys": ["plum"]},
            {"id": "q5", "category": "extraction", "asked_on": "2026-08-10", "question": "Gym cost?",
             "answer_keys": ["1200"], "abstain": False, "evidence": [{"day": "2026-08-03", "line": 0}]},
        ]}
    for q in questions["questions"]:
        q["split"] = "dev" if hashlib.sha256(q["id"].encode()).hexdigest()[0] in "01234" else "holdout"
    (root / "questions.json").write_text(json.dumps(questions, indent=1), encoding="utf-8")
    digest = hashlib.sha256((root / "questions.json").read_bytes()).hexdigest()
    (root / "questions.sha256").write_text(digest + "  questions.json\n", encoding="utf-8")
    return root


@pytest.fixture
def fake(tmp_path: Path) -> Path:
    return write_fake(tmp_path / "memory_eval")


# ---- scoring ----------------------------------------------------------------------------------------------

def test_normalize_dates_case_punctuation_and_thousands(ev: Any) -> None:
    assert ev.normalize("October 3, 2026!") == "2026-10-03"
    assert ev.normalize("on the 3rd of Oct") == "on the 3rd of oct"           # no year known: left as words
    assert ev.normalize("on the 3rd of Oct", 2026) == "on the 2026-10-03"
    assert ev.normalize("Sat 10/03/2026") == "sat 2026-10-03"
    assert ev.normalize("2026-10-3") == "2026-10-03"
    assert ev.normalize("Feb 30, 2026") == "feb 30 2026"                       # not a date: kept, then stripped
    assert ev.normalize("It costs $1,200.") == "it costs 1200"
    assert ev.normalize("Juno's   X-Ray") == "juno s x ray"
    assert ev.normalize("$5.75 at 11:30 p.m., v0.4.") == "5.75 at 11:30 p m v0.4"   # . and : kept between digits


def test_keys_every_element_a_list_is_a_choice(ev: Any) -> None:
    assert ev.keys_present(["2026-10-03"], "She arrives October 3rd, 2026.")
    assert ev.keys_present(["2026-10-03"], "She arrives Oct 3", year=2026)
    assert not ev.keys_present(["2026-10-03"], "She arrives Oct 4", year=2026)
    assert ev.keys_present(["thursday", ["7 pm", "19:00"]], "Thursdays at 19:00")
    assert ev.keys_present(["thursday", ["7 pm", "19:00"]], "Thursday, 7 PM")
    assert not ev.keys_present(["thursday", ["7 pm", "19:00"]], "Thursday at 8 pm")
    assert not ev.keys_present(["thursday", ["7 pm", "19:00"]], "7 pm")       # every element still needed
    assert ev.keys_present(["juno", "1200"], "Juno pays 1,200")
    assert not ev.keys_present(["juno", "1200"], "Juno pays")
    assert ev.keys_present([{"any": ["Juno", "my sister"]}], "your sister, I think: my sister")
    assert not ev.keys_present([], "anything")                                # no keys never passes


def test_keys_numbers_end_where_the_number_ends_and_words_may_run_on(ev: Any) -> None:
    assert ev.keys_present(["4"], "4 classes") and ev.keys_present(["482"], "Skylark Air 482")
    for wrong in ("14 classes", "4.5", "2026-09-04", "40", "10:45", "v4.1"):
        assert not ev.keys_present(["4"], wrong), wrong
    assert ev.keys_present(["12.6"], "$12.60 under") and not ev.keys_present(["12.6"], "12.65")
    assert ev.keys_present(["19"], "$19.00") and not ev.keys_present(["19"], "19.5 hours")
    assert ev.keys_present(["0.4"], "firmware v0.4") and not ev.keys_present(["0.4"], "v0.41")
    assert ev.keys_present(["1.0.0-beta.3"], "build 1.0.0-beta.3 went out")
    assert ev.keys_present(["cashew"], "Cashews.") and ev.keys_present(["drop"], "You dropped it")
    assert not ev.keys_present(["cat"], "a bobcat")                           # a key starts at a word start
    assert ev.keys_present([["10 pm", "22:00"]], "It closes at 10 PM") and ev.keys_present(["6:30"], "6:30 am")


def test_forget_scores_abstain_without_a_leak_key(ev: Any) -> None:
    q = {"abstain": True, "answer_keys": [], "leak_keys": ["4417", "bike lock"], "asked_on": "2026-08-15"}
    assert ev.score(q, {"answer": "", "abstain": True, "source": ""}) == 1
    assert ev.score(q, {"answer": "I no longer have it (was 4417)", "abstain": True, "source": ""}) == 0
    assert ev.score(q, {"answer": "Your bike lock? I can't say", "abstain": True, "source": ""}) == 0
    assert ev.score(q, {"answer": "44170", "abstain": True, "source": ""}) == 1
    assert ev.score(q, {"answer": "4417", "abstain": False, "source": ""}) == 0


def test_abstain_scoring_and_format(ev: Any) -> None:
    abstain_q = {"abstain": True, "answer_keys": []}
    answer_q = {"abstain": False, "answer_keys": ["2026-10-03"], "asked_on": "2026-08-10"}
    yes = ev.parse_reply('```json\n{"answer": "", "abstain": true, "source": "searched"}\n```')
    told = ev.parse_reply('Here: {"answer": "October 3", "abstain": false, "source": "profile"}')
    assert yes == {"answer": "", "abstain": True, "source": "searched"}
    assert ev.score(abstain_q, yes) == 1
    assert ev.score(abstain_q, told) == 0                                      # answered instead of abstaining
    assert ev.score(answer_q, told) == 1                                       # year from asked_on
    assert ev.score(answer_q, yes) == 0                                        # abstained on an answerable one
    assert ev.score(answer_q, {"answer": "2026-10-03", "abstain": True, "source": ""}) == 0
    assert ev.parse_reply("October 3, 2026") is None
    assert ev.score(answer_q, None) == 0                                       # no JSON: format failure
    assert ev.parse_reply('{"answer": "x", "abstain": "no"}') is None          # abstain must be a boolean


# ---- the split rule and the seal --------------------------------------------------------------------------

def test_split_rule_is_the_first_hex_digit_and_a_wrong_field_stops(ev: Any, fake: Path) -> None:
    for i in range(200):
        qid = f"q{i:03d}"
        want = "dev" if hashlib.sha256(qid.encode()).hexdigest()[0] in "01234" else "holdout"
        assert ev.rule_split(qid) == want and ev.split_of({"id": qid}) == want
    assert ev.split_of({"id": "q001", "split": ev.rule_split("q001")}) == ev.rule_split("q001")
    wrong = "dev" if ev.rule_split("q001") == "holdout" else "holdout"
    with pytest.raises(ev.SplitMismatch):
        ev.split_of({"id": "q001", "split": wrong})
    share = sum(ev.rule_split(f"q{i}") == "dev" for i in range(4000)) / 4000
    assert 0.28 < share < 0.345                                                # 5 of 16
    ds = ev.load_dataset(fake)
    dev, hold = ev.select(ds, "dev"), ev.select(ds, "holdout")
    assert {q["id"] for q in dev} | {q["id"] for q in hold} == {"q1", "q2", "q3", "q4", "q5"}
    assert not {q["id"] for q in dev} & {q["id"] for q in hold}
    assert len(ev.select(ds, "holdout", limit=1)) <= 1
    raw = json.loads((fake / "questions.json").read_text(encoding="utf-8"))
    raw["questions"][0]["split"] = "holdout" if raw["questions"][0]["split"] == "dev" else "dev"
    (fake / "questions.json").write_text(json.dumps(raw), encoding="utf-8")
    with pytest.raises(ev.SplitMismatch):
        ev.load_dataset(fake)


def test_seal_and_holdout_refusal(ev: Any, fake: Path, capsys: pytest.CaptureFixture[str]) -> None:
    ok, digest, sealed = ev.seal_check(fake)
    assert ok and digest == sealed
    assert ev.main(["--data", str(fake), "--seal-check"]) == 0
    raw = json.loads((fake / "questions.json").read_text(encoding="utf-8"))
    raw["questions"][0]["answer_keys"] = ["2026-10-04"]                    # same split, new keys
    (fake / "questions.json").write_text(json.dumps(raw), encoding="utf-8")
    assert not ev.seal_check(fake)[0]
    assert ev.main(["--data", str(fake), "--split", "holdout", "--estimate"]) == 2
    assert "SEAL_MISMATCH" in capsys.readouterr().out
    (fake / "questions.sha256").unlink()
    assert not ev.seal_check(fake)[0]                                          # no seal is no pass


def test_data_sha_ignores_questions_and_sees_transcripts(ev: Any, fake: Path) -> None:
    before = ev.data_sha(fake)
    (fake / "questions.json").write_text("{}", encoding="utf-8")
    assert ev.data_sha(fake) == before                                         # dev and holdout share one build
    with open(fake / "transcripts" / "2026-08-03.jsonl", "a", encoding="utf-8") as f:
        f.write(_line("2026-08-03", "13:00", "t-1785780000000-ef56", "owner", "One more line.") + "\n")
    assert ev.data_sha(fake) != before


# ---- the temp-folder guard ---------------------------------------------------------------------------------

def test_temp_guard_accepts_temp_and_refuses_everything_else(ev: Any, tmp_path: Path) -> None:
    assert ev.assert_temp(tmp_path / "memory") == Path(str((tmp_path / "memory").resolve()))
    # positive controls: each of these must be refused
    for bad in (Path.home() / ".config" / "eve-bridge" / "memory", Path.home() / "Documents" / "m",
                Path("/usr/local/memory-eval"), Path(ev.temp_roots()[0])):
        with pytest.raises(ev.UnsafeDir):
            ev.assert_temp(bad)


def test_open_memory_refuses_outside_temp_before_creating_anything(ev: Any) -> None:
    target = Path.home() / ".memory-eval-must-not-exist"
    assert not target.exists()
    with pytest.raises(ev.UnsafeDir):
        ev.open_memory(target, ev.Clock(), index_factory=None)
    with pytest.raises(ev.UnsafeDir):
        ev.copy_memory(target, Path.home() / ".memory-eval-copy-must-not-exist")
    assert not target.exists()
    assert not (Path.home() / ".memory-eval-copy-must-not-exist").exists()


# ---- forget runs on a copy ---------------------------------------------------------------------------------

def _build_with_snapshots(fake: Path, built: Path) -> Path:
    """A stand-in for a cached build: the transcripts, and per-night snapshots whose records name their night."""
    import shutil

    from eve_bridge import records

    shutil.copytree(fake / "transcripts", built / "transcripts")
    for day in ("2026-08-01", "2026-08-02", "2026-08-03"):
        rec = built / "snapshots" / day / "records"
        records.ensure_repo(rec)
        (rec / "profile.md").write_text(f"dreamt through {day}\n", encoding="utf-8")
    return built


def test_memory_as_of_holds_nothing_said_after_the_question(ev: Any, fake: Path, tmp_path: Path) -> None:
    built = _build_with_snapshots(fake, tmp_path / "cache" / "build")
    days = ["2026-08-01", "2026-08-02", "2026-08-03"]
    tz = ev.data_tz(fake / "transcripts", days)
    at = datetime(2026, 8, 2, 18, 40, 30, tzinfo=tz)                       # just after the tart, before the reply
    folder, snap = ev.memory_as_of(built, fake / "transcripts", days, at, tmp_path / "run" / "q-0")
    assert snap == "2026-08-01"                                                # the night of 08-02 has not run
    assert (folder / "records" / "profile.md").read_text(encoding="utf-8") == "dreamt through 2026-08-01\n"
    assert sorted(p.name for p in (folder / "transcripts").glob("*.jsonl")) == ["2026-08-01.jsonl",
                                                                                "2026-08-02.jsonl"]
    kept = (folder / "transcripts" / "2026-08-02.jsonl").read_text(encoding="utf-8")
    assert "plum tart" in kept and "That sounds lovely" not in kept
    later, snap2 = ev.memory_as_of(built, fake / "transcripts", days, ev.asked_at({"asked_on": "2026-08-10"}, tz),
                                   tmp_path / "run" / "q-1")
    assert snap2 == "2026-08-03" and len(list((later / "transcripts").glob("*.jsonl"))) == 3
    with pytest.raises(ev.UnsafeDir):
        ev.memory_as_of(built, fake / "transcripts", days, at, Path.home() / ".memory-eval-must-not-exist")
    assert not (Path.home() / ".memory-eval-must-not-exist").exists()


def test_build_keeps_a_snapshot_per_night_and_questions_see_only_the_past(ev: Any, fake: Path, tmp_path: Path,
                                                                           monkeypatch: pytest.MonkeyPatch) -> None:
    """The real build loop (Dreamer, offline: reconcile and consolidate stubbed), then memory as of a question."""
    from dataclasses import dataclass

    from eve_bridge import records, spend

    @dataclass
    class Change:
        changed: int
        created: int
        corrections: int
        journal: Path

    def reconcile_day(cfg: Any, client: Any, day: str, text: str) -> Change:
        journal = Path(cfg.records_dir) / "days" / f"{day}.md"
        journal.parent.mkdir(parents=True, exist_ok=True)
        journal.write_text(f"# {day}\n\n- {text[:40]}\n", encoding="utf-8")
        return Change(1, 1, 0, journal)

    monkeypatch.setattr(records, "reconcile_day", reconcile_day, raising=False)
    monkeypatch.setattr(records, "consolidate", lambda cfg, client: (0, 0), raising=False)
    monkeypatch.setenv("EVE_SPEND_DIR", str(tmp_path / "spend"))
    ds = ev.load_dataset(fake)
    try:
        built, manifest = ev.build_memory(ds, tmp_path / "cache", client=object(), index_factory=None)
    finally:
        spend.set_dir(None)
    assert manifest["dreamt"] == 3 and manifest["failed_days"] == []
    snaps = sorted(p.name for p in (built / "snapshots").iterdir())
    assert snaps == ["2026-08-01", "2026-08-02", "2026-08-03"]
    assert not (built / "snapshots" / "2026-08-01" / "records" / "days" / "2026-08-02.md").exists()
    assert (built / "snapshots" / "2026-08-02" / "records" / "days" / "2026-08-02.md").exists()
    tz = ev.data_tz(fake / "transcripts", ds.days)
    folder, snap = ev.memory_as_of(built, fake / "transcripts", ds.days,
                                   ev.asked_at({"asked_on": "2026-08-02"}, tz), tmp_path / "run" / "q-0")
    assert snap == "2026-08-01"
    assert not (folder / "records" / "days" / "2026-08-02.md").exists()
    assert not (folder / "transcripts" / "2026-08-03.jsonl").exists()


def test_forget_runs_on_a_copy_and_leaves_the_build(ev: Any, fake: Path, tmp_path: Path) -> None:
    built = _build_with_snapshots(fake, tmp_path / "cache" / "build")
    before = {p.relative_to(built): p.read_bytes() for p in built.rglob("*") if p.is_file()}
    tdir = fake / "transcripts"
    q4 = json.loads((fake / "questions.json").read_text(encoding="utf-8"))["questions"][3]
    queries = ev.forget_queries(q4, tdir)
    assert queries == ["I baked a plum tart for the neighbours.", '"plum"']   # the line, then each leak key
    days = ["2026-08-01", "2026-08-02", "2026-08-03"]
    clock = ev.Clock(ev.asked_at(q4, ev.data_tz(tdir, days)))
    folder, _ = ev.memory_as_of(built, tdir, days, clock.now, tmp_path / "run" / "q-3")
    mem = ev.open_memory(folder, clock, index_factory=None)
    assert mem.search("plum tart")["said"]                                     # there before the forget
    done = ev.run_forgets(mem, queries)
    assert done["ok"] and done["forgot"] >= 1 and done["queries"] == 2
    assert "plum" not in json.dumps(done)
    assert not mem.search("plum")["said"]                                      # gone from the copy
    assert "(forgotten)" in (folder / "transcripts" / "2026-08-02.jsonl").read_text(encoding="utf-8")
    assert {p.relative_to(built): p.read_bytes() for p in built.rglob("*") if p.is_file()} == before
    other, _ = ev.memory_as_of(built, tdir, days, clock.now, tmp_path / "run" / "q-4")
    base = ev.open_memory(other, clock, index_factory=None)
    assert base.search("plum tart")["said"]                                    # the next question still has it
    assert "plum tart" in ev.normalize(ev.stored_text(base))
    assert "plum" not in ev.normalize(ev.stored_text(mem))
    nothing = ev.run_forgets(mem, ["a line that is not there"])
    assert not nothing["ok"]                                                   # a forget that matched nothing fails


def test_evidence_resolves_and_is_matched_in_tool_results(ev: Any, fake: Path) -> None:
    tdir = fake / "transcripts"
    qs = json.loads((fake / "questions.json").read_text(encoding="utf-8"))["questions"]
    assert ev.evidence_texts(qs[0], tdir) == (["My sister Juno is visiting on October 3, 2026."], 0)
    assert ev.evidence_texts(qs[2], tdir) == (["I baked a plum tart for the neighbours."], 0)
    assert ev.evidence_texts({"evidence": [{"day": "2026-08-02", "line": 1}]}, tdir) == (["That sounds lovely."], 0)
    assert ev.evidence_texts({"evidence": [{"day": "2026-08-02", "index": 1}]}, tdir) == (["That sounds lovely."], 0)
    assert ev.evidence_texts({"evidence": ["2026-08-02:1"]}, tdir) == (["That sounds lovely."], 0)
    assert ev.evidence_texts({"evidence": [{"day": "2026-08-03", "line": 1}]}, tdir) == ([], 1)   # past the end
    assert ev.evidence_texts({"evidence": [{"day": "2026-09-01", "line": 0}]}, tdir) == ([], 1)
    said = {"ok": True, "said": [{"day": "2026-08-01", "text": "My sister Juno is visiting on October 3, 2026."}]}
    assert ev.evidence_hit(["My sister Juno is visiting on October 3, 2026."], [said])
    assert not ev.evidence_hit(["My sister Juno is visiting on October 3, 2026."], [{"recalled": ["Juno visits"]}])


def test_rows_carry_no_words_and_the_summary_counts(ev: Any, fake: Path) -> None:
    tdir = fake / "transcripts"
    qs = json.loads((fake / "questions.json").read_text(encoding="utf-8"))["questions"]
    said = {"ok": True, "said": [{"text": "My sister Juno is visiting on October 3, 2026."}]}
    good = ev.Turn(reply='{"answer": "October 3, 2026", "abstain": false, "source": "search"}', rounds=2,
                   calls=[{"name": "memory_search", "args": {"query": "Juno"}, "ms": 40.0, "result": said}],
                   usage={"in": 1000, "cached": 800, "out": 50, "instr": 9000})
    abstained = ev.Turn(reply='{"answer": "", "abstain": true, "source": ""}', rounds=1,
                        calls=[{"name": "memory_search", "args": {}, "ms": 900.0, "result": {"ok": True}}])
    wrong = ev.Turn(reply='{"answer": "a pie", "abstain": false, "source": ""}', rounds=1)
    rows = [ev.make_row(qs[0], "dev", good, *ev.evidence_texts(qs[0], tdir), stored=""),
            ev.make_row(qs[1], "dev", abstained, *ev.evidence_texts(qs[1], tdir), stored=""),
            ev.make_row(qs[2], "dev", abstained, *ev.evidence_texts(qs[2], tdir), stored="I baked a plum tart"),
            ev.make_row(qs[2], "dev", wrong, *ev.evidence_texts(qs[2], tdir), stored="nothing")]
    text = json.dumps(rows)
    for words in ("Juno", "bake", "plum", "October", "dog"):
        assert words.lower() not in text.lower()
    assert [r["score"] for r in rows] == [1, 1, 0, 0]
    assert rows[0]["evidence_hit"] is True and rows[0]["keys_in_results"] is True
    assert rows[2]["failure"] == "retrieval" and rows[3]["failure"] == "storage"
    s = ev.summarize(rows)
    assert s["accuracy"] == 0.5
    assert s["abstention_precision"] == 0.5 and s["abstention_recall"] == 1.0
    assert s["memory_search_p95_ms"] == 900.0 and s["memory_search_calls"] == 3
    assert s["by_category"]["extraction"] == {"n": 2, "accuracy": 0.0}
    assert ev.p95([float(i) for i in range(1, 101)]) == 95.0


def test_estimate_makes_no_call_and_counts_the_data(ev: Any, fake: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge import records

    monkeypatch.setattr(records, "OpenAIReconcileClient", lambda *a, **k: pytest.fail("no client in estimate"))
    ds = ev.load_dataset(fake)
    e = ev.estimate(ds, ds.questions, ask_model="gpt-5.4-nano", about_chars=500, index_on=True, cached=False)
    assert e["days"] == 3 and e["days_with_words"] == 3 and e["dream_calls"] == 6
    assert e["mem0_calls"] == 6 and e["build_usd"] > 0 and e["ask_usd"] > 0
    cached = ev.estimate(ds, ds.questions, ask_model="gpt-5.4-nano", about_chars=500, index_on=True, cached=True)
    assert cached["build_usd"] == 0 and cached["ask_usd"] == e["ask_usd"]


# ---- the ask path, offline: the real TelegramInlet._think with a fake model ----------------------------------

def _call(name: str, args: dict[str, Any], cid: str) -> dict[str, Any]:
    return {"output": [{"type": "function_call", "name": name, "arguments": json.dumps(args), "call_id": cid}],
            "usage": {"input_tokens": 100, "output_tokens": 10, "input_tokens_details": {"cached_tokens": 60}}}


def _text(text: str) -> dict[str, Any]:
    return {"output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}],
            "usage": {"input_tokens": 120, "output_tokens": 20}}


def _brain(ev: Any, fake: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, replies: list[Any]) -> Any:
    import shutil

    from eve_bridge import spend, system_context
    from eve_bridge import telegram as tg

    spend.set_dir(tmp_path / "spend")
    monkeypatch.setattr(system_context, "context", system_context.context)     # pin_clock's patch is undone after
    monkeypatch.setattr(tg, "MAX_TOOL_ROUNDS", ev.MAX_ROUNDS)
    built = tmp_path / "built"
    shutil.copytree(fake / "transcripts", built / "transcripts")
    clock = ev.Clock(ev.asked_at({"asked_on": "2026-08-10"}, ev.data_tz(fake / "transcripts", ["2026-08-01"])))
    ev.pin_clock(clock)
    mem = ev.open_memory(ev.copy_memory(built, tmp_path / "run" / "base"), clock, index_factory=None)
    sent: list[dict[str, Any]] = []

    async def create(payload: dict[str, Any]) -> dict[str, Any]:
        sent.append(payload)                     # past the scripted replies: search again, every round
        return replies.pop(0) if replies else _call("memory_search", {"query": "plum", "days_back": None},
                                                    f"c{len(sent)}")

    return ev.make_inlet(tg.TelegramConfig(), create, mem, clock), create, sent


def test_ask_runs_the_real_loop_records_calls_and_refuses_other_tools(ev: Any, fake: Path, tmp_path: Path,
                                                                      monkeypatch: pytest.MonkeyPatch) -> None:
    import asyncio

    from eve_bridge import spend

    replies = [_call("memory_search", {"query": "plum tart", "days_back": None}, "c1"),
               _call("remember", {"text": "x"}, "c2"),
               _text('{"answer": "a plum tart", "abstain": false, "source": "memory search, 2026-08-02"}')]
    inlet, create, sent = _brain(ev, fake, tmp_path, monkeypatch, replies)
    try:
        turn = asyncio.run(ev.ask(inlet, "What did I bake?", create))
    finally:
        spend.set_dir(None)
    assert turn.error == "" and turn.rounds == 3
    assert [c["name"] for c in turn.calls] == ["memory_search", "remember"]
    assert turn.calls[0]["result"]["said"] and turn.calls[1]["result"] == ev.UNAVAILABLE
    assert turn.usage["in"] == 320 and turn.usage["cached"] == 120
    first = sent[0]
    assert "memory_search" in {t.get("name") for t in first["tools"]}
    roles = [(i.get("role"), i["content"][0]["text"]) for i in first["input"] if i.get("type") == "message"]
    assert roles[-2] == ("user", "What did I bake?") and roles[-1] == ("developer", ev.EVAL_NOTE)
    assert any("2026-08-10T20:00:00-07:00" in text for role, text in roles if role == "developer")   # the clock
    assert turn.instructions == first["instructions"]
    q = {"id": "q3", "category": "extraction", "asked_on": "2026-08-10", "answer_keys": ["plum", "tart"],
         "abstain": False}
    row = ev.make_row(q, "dev", turn, ["I baked a plum tart for the neighbours."], 0, stored="")
    assert row["score"] == 1 and row["evidence_hit"] is True and row["tools"] == ["memory_search", "remember"]


def test_ask_stops_after_max_rounds_with_the_brains_own_summary(ev: Any, fake: Path, tmp_path: Path,
                                                                monkeypatch: pytest.MonkeyPatch) -> None:
    import asyncio

    from eve_bridge import spend

    inlet, create, sent = _brain(ev, fake, tmp_path, monkeypatch, [])
    try:
        turn = asyncio.run(ev.ask(inlet, "What did I bake?", create))
    finally:
        spend.set_dir(None)
    assert len(turn.calls) == ev.MAX_ROUNDS                                    # four tool rounds, then no tools
    assert len(sent) == ev.MAX_ROUNDS + 1 and sent[-1]["tool_choice"] == "none"


# ---- the real fixture against the scorer ------------------------------------------------------------------------

REAL = Path(__file__).parent / "fixtures" / "memory_eval"
real_only = pytest.mark.skipif(not (REAL / "questions.json").is_file(), reason="no real fixture")


def _first(key: Any) -> str:
    return str(key[0]) if isinstance(key, list) else str(key)


@real_only
def test_real_fixture_shape(ev: Any) -> None:
    ds = ev.load_dataset(REAL)                                                 # also checks every split field
    tdir = REAL / "transcripts"
    cats: dict[str, int] = {}
    for q in ds.questions:
        cats[q["category"]] = cats.get(q["category"], 0) + 1
        assert q["split"] == ev.rule_split(q["id"])
        texts, unresolved = ev.evidence_texts(q, tdir)
        assert unresolved == 0, q["id"]
        assert bool(q["abstain"]) == (not q["answer_keys"]), q["id"]
        for e in q["evidence"]:
            assert e["day"] < q["asked_on"], q["id"]                           # asked after every needle
        if q["category"] == "forget":
            assert q["forget"] == q["evidence"][0] and q["leak_keys"]
            line = ev.ref_text(tdir, q["forget"])
            assert ev.keys_present(q["leak_keys"], line, ev.asked_year(q)), q["id"]
    assert len(ds.questions) == 240 and min(cats.values()) >= 20 and len(cats) == 9


@real_only
def test_real_keys_right_answers_pass_and_wrong_answers_fail(ev: Any) -> None:
    ds = ev.load_dataset(REAL)
    answerable = [q for q in ds.questions if not q["abstain"]]
    for q in answerable:
        right = "; ".join(_first(k) for k in q["answer_keys"])
        assert ev.score(q, {"answer": right, "abstain": False, "source": ""}) == 1, q["id"]
        assert ev.score(q, {"answer": right, "abstain": True, "source": ""}) == 0, q["id"]
        assert ev.score(q, {"answer": "", "abstain": False, "source": ""}) == 0, q["id"]
        assert ev.score(q, None) == 0
        # another question's right answer fails here, unless it happens to hold these keys too
        for other in answerable:
            text = "; ".join(_first(k) for k in other["answer_keys"])
            if other["id"] != q["id"] and not ev.keys_present(q["answer_keys"], text, ev.asked_year(q)):
                assert ev.score(q, {"answer": text, "abstain": False, "source": ""}) == 0
    for q in ds.questions:
        if q["abstain"]:
            assert ev.score(q, {"answer": "", "abstain": True, "source": ""}) == 1, q["id"]
            assert ev.score(q, {"answer": "something", "abstain": False, "source": ""}) == 0, q["id"]
        if q.get("leak_keys"):
            leak = q["leak_keys"][0]
            assert ev.score(q, {"answer": f"I'd rather not, but it was {leak}", "abstain": True, "source": ""}) == 0


# Replies as a model would write them: (id, a right one, a wrong one).
PHRASED = [
    ("q001", "You're allergic to cashews.", "You're allergic to peanuts."),
    ("q004", "Skylark Air 482", "Skylark Air 428"),
    ("q007", "Circuit Night is every Thursday at 7 p.m.", "Every Tuesday at 7 pm"),
    ("q018", "$3,000 for the Japan trip", "$2,150"),
    ("q030", "$1,320 in total", "$1,302"),
    ("q036", "4 classes", "14 credits across 5 classes"),
    ("q038", "$12.60 under budget", "$12.06 under"),
    ("q049", "About $23 a week (4 x $5.75)", "About $21 a week"),
    ("q057", "You signed it on August 10, 2026.", "August 29, 2026"),
    ("q065", "You flew to Denver on Aug 14.", "You flew on August 16."),
    ("q080", "It's on a Saturday.", "It's on a Friday."),
    ("q085", "$1,180 a month", "$940 a month"),
    ("q087", "Kellner 118", "Kellner 204"),
    ("q091", "Every 10 minutes", "Every 5 minutes"),
    ("q096", "$39 a month", "$34 a month"),
    ("q100", "October 8", "October 1"),
    ("q107", "Sundays at 6 pm", "Sundays at 4 pm"),
    ("q143", "Firmware v0.4", "Firmware v0.41"),
    ("q144", "1.0.0-beta.3", "1.0.0-beta.2"),
    ("q153", "$287.40", "$300"),
    ("q155", "$19.00 for the two SHT45s", "$9.50"),
    ("q170", "About 19.5 hours", "About 19 hours"),
    ("q174", "11:05", "11:50"),
    ("q179", "10 PM", "11 pm"),
    ("q190", "No, you called it off because of wildfire smoke.", "Yes, you went on September 12."),
    ("q196", "You dropped it and added PHIL 220.", "You're still taking it on Fridays."),
    ("q203", "At your apartment on Birch Lane.", "At Lucky Noodle."),
    ("q211", "3 interviews on October 2.", "5 interviews on October 3."),
]


@real_only
def test_real_keys_on_phrased_replies(ev: Any) -> None:
    qs = {q["id"]: q for q in ev.load_dataset(REAL).questions}
    for qid, right, wrong in PHRASED:
        assert ev.score(qs[qid], {"answer": right, "abstain": False, "source": ""}) == 1, (qid, right)
        assert ev.score(qs[qid], {"answer": wrong, "abstain": False, "source": ""}) == 0, (qid, wrong)
