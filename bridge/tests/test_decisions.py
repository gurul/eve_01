from __future__ import annotations

import copy
from types import SimpleNamespace

import pytest
from PIL import Image

from eve_bridge import decisions as d


def body(questions, target="a"):
    answers = []
    for q in questions:
        if q["type"] == "predicate":
            answers.append({"name": q["name"], "type": "predicate", "probability": 0.01 if q["name"] == "risky" else 0.99})
        else:
            choices = q["choices"]
            answers.append({"name": q["name"], "type": "choice", "choice": target, "confidence": 0.99,
                            "probabilities": [{"value": c["value"], "probability": 0.99 if c["value"] == target
                                               else 0.01 / (len(choices) - 1)} for c in choices]})
    return {"answers": answers, "usage": {"input_tokens": 123, "output_tokens": 0}}


def test_wire_has_inline_images_shared_evidence_and_all_questions():
    seen, metered = [], []
    qs = d.selection_questions({"a": "Week", "b": "Day"})

    def post(url, **kw):
        seen.append((url, kw))
        return SimpleNamespace(status_code=200, json=lambda: body(qs))

    client = d.Client("secret", post=post, meter=metered.append)
    a = client.evaluate({"step": "week"}, [Image.new("RGB", (2000, 1000))], qs)
    assert not a.error and a.values["target"]["id"] == "a" and a.usage["input_tokens"] == 123
    url, request = seen[0]
    assert url == d.ENDPOINT and request["headers"]["Authorization"] == "Bearer secret"
    payload = request["json"]
    assert payload["model"] == "gpt-6-luna" and payload["questions"] == qs
    parts = payload["input"][0]["content"]
    assert '"step": "week"' in parts[0]["text"]
    assert parts[1]["image_url"].startswith("data:image/png;base64,")
    assert len(metered) == 1


@pytest.mark.parametrize("fault", ["refusal", "missing", "unknown", "duplicate", "nan", "negative", "boolean",
                                   "wrong_name", "low_sum", "bad_confidence", "not_top"])
def test_malformed_answers_cannot_authorize_actions(fault):
    qs = d.selection_questions({"a": "Week", "b": "Day"})
    reply = copy.deepcopy(body(qs))
    if fault == "refusal":
        reply["answers"][1]["type"] = "refusal"
    elif fault == "missing":
        reply["answers"].pop()
    elif fault == "unknown":
        reply["answers"][0]["choice"] = "unoffered"
    elif fault == "duplicate":
        reply["answers"][0]["probabilities"][1]["value"] = "a"
    elif fault in ("nan", "negative", "boolean"):
        reply["answers"][1]["probability"] = {"nan": float("nan"), "negative": -1, "boolean": True}[fault]
    elif fault == "wrong_name":
        reply["answers"][1]["name"] = "other"
    elif fault == "low_sum":
        reply["answers"][0]["probabilities"][0]["probability"] = 0.1
    elif fault == "bad_confidence":
        reply["answers"][0]["confidence"] = None
    else:
        reply["answers"][0]["choice"] = "b"
    with pytest.raises(ValueError):
        d.parse(reply, qs)
    # Positive control for the very same parser.
    assert d.parse(body(qs), qs)["target"]["id"] == "a"


def test_http_failure_is_redacted_and_not_retried():
    calls = []

    def post(*args, **kwargs):
        calls.append(1)
        return SimpleNamespace(status_code=403, text="secret screen text")

    a = d.Client("key", post=post).evaluate({}, [], [d.predicate("verified", "Done?")])
    assert a.error == "Decisions HTTP 403" and calls == [1] and a.values == {}


@pytest.mark.parametrize("exception", [RuntimeError, ValueError])
def test_transport_exception_does_not_leak_request_or_key(exception):
    def post(*args, **kwargs):
        raise exception("Bearer secret; private labels")

    a = d.Client("secret", post=post).evaluate({}, [], [])
    assert a.error == exception.__name__


def test_the_daemon_never_builds_a_decisions_client_and_says_so_once(caplog):
    # Off since 2026-10-08: OpenAI's Decisions beta has no OpenRouter route (docs/openrouter.md)
    assert d.from_env({"OPENAI_API_KEY": "k"}) is None
    assert d.from_env({d.ENV: "1"}) is None
    d._warned = False
    with caplog.at_level("WARNING"):
        assert d.from_env({d.ENV: "1", "OPENAI_API_KEY": "k", "OPENROUTER_API_KEY": "or"}) is None
    assert any("visual judge is off" in r.message for r in caplog.records) and d._warned
    caplog.clear()
    with caplog.at_level("WARNING"):
        assert d.from_env({d.ENV: "1"}) is None
    assert not any("visual judge is off" in r.message for r in caplog.records)       # once a process


def test_usage_is_recorded_even_when_answer_is_refused(monkeypatch):
    from eve_bridge import spend

    rows = []
    monkeypatch.setattr(spend, "record", lambda *a, **kw: rows.append((a, kw)))
    reply = {"answers": [{"type": "refusal", "name": "verified"}], "usage": {"input_tokens": 50}}
    a = d.Client("k", post=lambda *a, **kw: SimpleNamespace(status_code=200, json=lambda: reply)).evaluate(
        {}, [], [d.predicate("verified", "Done?")])
    assert a.error and rows[0][0][3] is None and rows[0][1]["tokens"] == {"input_tokens": 50}
