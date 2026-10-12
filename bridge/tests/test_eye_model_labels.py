"""The eye label set (eye_model.CRITERIA), its per-label holds, and tools/jev_eyes_eval.py's scoring on fakes."""

import asyncio
import json
import sys
from pathlib import Path

import pytest

from eve_bridge import eye_model
from eve_bridge.eye_model import CRITERIA, EYE_CHOICE, LABELS, QUESTION, TTL_MAX_MS, TTL_MIN_MS, TTL_MS
from eve_bridge.live_expressions import LiveExpressions, expression_ttl_ms
from eve_bridge.phone_call import PhoneCalls

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import jev_eyes_eval as ev  # noqa: E402

ORIGINAL = ("calm", "happy", "curious", "affection", "surprised", "sad", "worried", "skeptical", "frustrated",
            "excited", "wink")
ADDED = ("laughing", "sleepy", "confused", "thinking", "sheepish", "relieved", "proud", "flattered")
DESIGN = json.loads((Path(__file__).resolve().parents[2] / "docs/expressions-design.json").read_text())


def test_labels_are_the_original_eleven_then_the_eight_in_wire_order():
    assert LABELS == ORIGINAL + ADDED
    assert tuple(label["name"] for label in DESIGN["labels"]) == ADDED
    assert "startled" not in LABELS


def test_new_criteria_are_the_designs_text_and_the_old_ones_are_untouched():
    for label in DESIGN["labels"]:
        assert CRITERIA[label["name"]] == label["criterion"]
    assert CRITERIA["wink"] == "playful teasing, a cheeky aside or asking for a wink"
    assert CRITERIA["calm"] == "neutral facts, routine questions or instructions"
    assert CRITERIA["affection"] == "love, gratitude, friendship or reassurance"
    assert CRITERIA["curious"] == "wondering, exploring or wanting to learn"
    assert "joke" not in CRITERIA["wink"]                     # laughing alone owns humour


def test_every_question_carries_every_label():
    assert list(EYE_CHOICE["eye"]["criteria"]) == list(LABELS)
    assert list(QUESTION) == list(LABELS)


def test_ttls_follow_the_design_and_clamp():
    design = {"laughing": 3000, "sleepy": 5000, "confused": 3500, "thinking": 3500, "sheepish": 4000,
              "relieved": 3500, "proud": 4000, "flattered": 4000}
    for label in DESIGN["labels"]:                            # the design's prose says "TTL 3.5 s."
        assert f"TTL {design[label['name']] / 1000:g} s" in label["shape"]
    assert set(TTL_MS) == set(LABELS)
    assert all(TTL_MS[label] == 4000 for label in ORIGINAL)
    assert {label: TTL_MS[label] for label in ADDED} == design
    assert (TTL_MIN_MS, TTL_MAX_MS) == (500, 6000)
    assert expression_ttl_ms("sleepy", 0) == 5000
    assert expression_ttl_ms("laughing", 0.5) == 2500
    assert expression_ttl_ms("happy", 3.9) == 500             # never below the floor
    assert expression_ttl_ms("happy", 0) == 4000


def test_the_picker_sends_the_labels_own_ttl():
    sleepy = [0.0] * len(LABELS)
    sleepy[LABELS.index("sleepy")] = 1.0

    class Model:
        def predict(self, state):
            return {"probabilities": sleepy, "ms": 1.0}

    async def run():
        sent = []

        async def send(cmd):
            sent.append(cmd)
            return True

        service = LiveExpressions(send, model_factory=Model, enabled=True, lazy=True, clock=lambda: 100.0)
        service.offer("user", "Goodnight, eve.")
        task = asyncio.create_task(service.run())
        for _ in range(200):
            if sent:
                break
            await asyncio.sleep(0.01)
        task.cancel()
        return sent

    sent = asyncio.run(run())
    assert sent and sent[0]["label"] == "sleepy" and sent[0]["ttl_ms"] == 5000


@pytest.mark.parametrize("label", ADDED)
def test_the_phone_event_carries_each_new_label(label):
    class Call:
        can_eyes, talking = True, False

        class ws:
            closed = False

        def __init__(self):
            self.sent = []

        async def send_expression(self, msg):
            self.sent.append(msg)
            return True

    calls = PhoneCalls(check_owner=lambda *_: True, brain=None, voice=None)
    calls.active = call = Call()
    ok = asyncio.run(calls.send_expression(
        {"cmd": "expression", "id": 4, "label": label, "ttl_ms": TTL_MS[label], "chirp": False}))
    assert ok and call.sent == [{"type": "expression", "id": 4, "label": label, "ttl_ms": TTL_MS[label]}]


def test_every_label_is_a_valid_pick():
    for i, label in enumerate(LABELS):
        values = [0.0] * len(LABELS)
        values[i] = 1.0
        assert LiveExpressions.label({"probabilities": values}) == (label, 1.0)


# --- tools/jev_eyes_eval.py ----------------------------------------------------------------------------------


def test_fold_covers_every_label_onto_the_six_and_none_new_to_startled():
    assert set(ev.FOLD) == set(LABELS) and set(ev.FOLD.values()) == set(ev.SIX)
    assert {ev.FOLD[label] for label in ADDED} <= {"happy", "affection", "curious", "calm"}
    assert [label for label, six in ev.FOLD.items() if six == "startled"] == ["worried"]
    assert {label: ev.FOLD[label] for label in ADDED} == {
        "laughing": "happy", "proud": "happy", "relieved": "happy", "flattered": "affection",
        "confused": "curious", "thinking": "curious", "sleepy": "calm", "sheepish": "calm"}
    assert ev.LEGACY_LABELS == frozenset(ORIGINAL)


class Fake:
    """Answers by a table from the state's last line; raises for "boom"; fixed latency per call."""

    def __init__(self, table, ms=100.0):
        self.table, self.ms, self.states = table, ms, []

    def predict(self, state):
        self.states.append(state)
        last = state.splitlines()[-1].split(": ", 1)[1]
        if last == "boom":
            raise RuntimeError("down")
        return {"label": self.table[last], "ms": self.ms}


def test_legacy_scoring_folds_and_the_verdict_holds_both_bars():
    sets = {"scenarios-test": [{"id": "a", "label": "happy", "text": "ha"}, {"id": "b", "label": "calm", "text": "x"}],
            "confirmation": [{"id": "c", "label": "curious", "text": "eh"}, {"id": "d", "label": "calm", "text": "boom"}]}
    result = ev.score("jev", Fake({"ha": "laughing", "x": "thinking", "eh": "confused"}), sets)
    assert result["sets"]["scenarios-test"]["accuracy"] == 0.5          # laughing→happy hit, thinking→curious miss
    assert result["sets"]["confirmation"]["accuracy"] == 0.5            # confused→curious hit, the error a miss
    assert result["sets"]["confirmation"]["errors"] == 1
    assert ev.legacy_verdict(result, 50, 50) == (True, [])
    ok, reasons = ev.legacy_verdict(result, 57.5, 50)
    assert not ok and reasons == ["scenarios-test 50.0% < 57.5%"]


def test_blind_state_is_built_as_production_builds_it():
    turns = [{"who": "owner", "text": "I  finally\nshipped it"}, {"who": "eve", "text": "Nice   work!"}]
    assert ev.blind_state(turns) == "User: I finally shipped it\nEve: Nice work!"
    context = eye_model.ConversationContext()                          # the live worker's own path
    context.state("user", "I finally shipped it", 0)
    assert ev.blind_state(turns) == context.state("assistant", "Nice work!", 1)
    assert ev.blind_state([{"who": "owner", "text": "hi"}]) == "User: hi"


def _blind_rows():
    rows = []
    for i in range(5):
        rows.append({"id": f"l{i}", "label": "laughing", "turns": [{"who": "owner", "text": f"lol{i}"}]})
    for i in range(5):
        rows.append({"id": f"s{i}", "label": "sleepy", "turns": [{"who": "owner", "text": f"zz{i}"}]})
    rows.append({"id": "p0", "label": "proud", "turns": [{"who": "owner", "text": "boom"}]})
    return rows


def test_blind_scoring_math_and_bar():
    table = {f"lol{i}": "laughing" for i in range(5)} | {f"zz{i}": "sleepy" for i in range(3)}
    table |= {"zz3": "calm", "zz4": "calm"}
    report = ev.score_blind(Fake(table, ms=120.0), _blind_rows())
    assert report["n"] == 11 and report["errors"] == 1
    assert report["accuracy"] == pytest.approx(8 / 11)
    assert report["recall"]["laughing"] == {"n": 5, "hits": 5, "recall": 1.0}
    assert report["recall"]["sleepy"]["recall"] == pytest.approx(0.6)
    assert report["recall"]["proud"] == {"n": 1, "hits": 0, "recall": 0.0}
    assert report["confusion"][0] == {"gold": "sleepy", "eye": "calm", "count": 2}
    assert {"gold": "proud", "eye": "(error)", "count": 1} in report["confusion"]
    assert report["ms_p50"] == 120.0 and report["ms_p95"] == 120.0
    # proud's recall of 0 does not count (one case); errors 1/11 = 9% does
    ok, reasons = ev.blind_verdict(report, 65, 55, 400)
    assert not ok and reasons == ["errors 1/11 > 2%"]
    report["error_rate"] = 0.0
    assert ev.blind_verdict(report, 65, 55, 400) == (True, [])
    assert ev.blind_verdict(report, 75, 55, 400)[1] == ["top-1 72.7% < 75%"]
    assert ev.blind_verdict(report, 65, 61, 400)[1] == ["sleepy recall 60.0% < 61% (n=5)"]
    assert ev.blind_verdict(report, 65, 55, 100)[1] == ["p50 120 ms > 100 ms"]


def test_blind_set_must_be_well_formed(tmp_path):
    good = tmp_path / "good.json"
    good.write_text(json.dumps({"cases": _blind_rows()}))
    assert len(ev.load_blind(good)) == 11
    for bad in ({"cases": []}, {"cases": [{"id": "x", "label": "startled", "turns": [{"who": "owner", "text": "a"}]}]},
                {"cases": [{"id": "x", "label": "calm", "turns": [{"who": "robot", "text": "a"}]}]}):
        path = tmp_path / "bad.json"
        path.write_text(json.dumps(bad))
        with pytest.raises(SystemExit):
            ev.load_blind(path)


def test_blind_mode_prints_the_marker_and_exits_by_the_bar(tmp_path, monkeypatch, capsys):
    path = tmp_path / "set.json"
    path.write_text(json.dumps({"cases": [r for r in _blind_rows() if r["label"] != "proud"]}))
    table = {f"lol{i}": "laughing" for i in range(5)} | {f"zz{i}": "sleepy" for i in range(5)}
    monkeypatch.setattr(ev, "JevEyeModel", lambda: Fake(table))
    args = ["--blind", str(path), "--min-acc", "65", "--min-recall", "55", "--max-p50-ms", "400"]
    assert ev.main(args) == 0 and "EYES_BAR_OK" in capsys.readouterr().out
    monkeypatch.setattr(ev, "JevEyeModel", lambda: Fake(table, ms=900.0))
    assert ev.main(args) == 1 and "EYES_BAR_OK" not in capsys.readouterr().out


def test_dev_set_is_well_formed_and_covers_each_new_label():
    path = Path(__file__).resolve().parent / "fixtures/emotion_dev_v2.json"
    rows = ev.load_blind(path)
    counts = {label: sum(r["label"] == label for r in rows) for label in ADDED}
    assert all(n >= 2 for n in counts.values()), counts
