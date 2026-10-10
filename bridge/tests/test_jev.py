"""jev.py: TypeSafe's Jev behind the predict(state, questions) seam — the transport, with no socket.

Written by the session that wired Jev into the lane, the router and the head (the module itself came from
a parallel session on 2026-09-21 and had no tests). An `opener` stands in for urllib."""

from __future__ import annotations

import io
import json
import urllib.error

import pytest

from eve_bridge import jev


class Reply:
    def __init__(self, body: object) -> None:
        self._body = json.dumps(body).encode() if not isinstance(body, bytes) else body

    def read(self) -> bytes:
        return self._body

    def __enter__(self) -> "Reply":
        return self

    def __exit__(self, *a) -> None:
        return None


QUESTIONS = {"pick": {"type": "choice", "instructions": "which?", "criteria": {"a": "A", "b": "B"}}}
ANSWER = {"answers": {"pick": {"choice": "a", "probabilities": {"a": 0.9, "b": 0.1}, "confidence": 0.8}},
          "usage": {"input_tokens": 40}}


def test_route_config_names_the_key_that_is_missing_and_lets_the_model_be_pinned() -> None:
    assert jev.route_config({"TYPESAFE_API_KEY": "k"}) == ("https://api.typesafe.ai/v1/systemone", "k", "jev-latest")
    url, key, model = jev.route_config({"EVE_JEV_ROUTE": "openrouter", "OPENROUTER_API_KEY": " r ",
                                        "EVE_JEV_MODEL": "typesafe/jev-1.13"})
    assert url.endswith("/api/alpha/decisions") and key == "r" and model == "typesafe/jev-1.13"
    with pytest.raises(jev.JevError, match="TYPESAFE_API_KEY is not set"):
        jev.route_config({})
    with pytest.raises(jev.JevError, match="must be one of"):
        jev.route_config({"EVE_JEV_ROUTE": "carrier-pigeon", "TYPESAFE_API_KEY": "k"})


def test_predict_posts_one_request_and_hands_the_body_back() -> None:
    seen: dict = {}

    def opener(request, timeout):
        seen.update(url=request.full_url, auth=request.get_header("Authorization"), timeout=timeout,
                    body=json.loads(request.data))
        return Reply(ANSWER)

    predict = jev.make_predict("https://x/v1", "secret", "jev-1.13.0", timeout_s=1.5, opener=opener)
    out = predict({"owner_said": "look left"}, QUESTIONS)
    assert out == ANSWER
    assert seen["url"] == "https://x/v1" and seen["auth"] == "Bearer secret" and seen["timeout"] == 1.5
    assert seen["body"] == {"model": "jev-1.13.0", "state": {"owner_said": "look left"}, "questions": QUESTIONS}


def test_a_retryable_status_is_retried_and_everything_else_is_one_line_never_a_traceback() -> None:
    calls = {"n": 0}
    naps: list[float] = []

    def flaky(request, timeout):
        calls["n"] += 1
        if calls["n"] < 3:
            raise urllib.error.HTTPError(request.full_url, 529, "overloaded", {}, io.BytesIO(b""))
        return Reply(ANSWER)

    assert jev.make_predict("https://x", "k", "m", opener=flaky, sleep=naps.append)({}, QUESTIONS) == ANSWER
    assert calls["n"] == 3 and naps == [0.2, 0.4]

    def refuse(request, timeout):
        raise urllib.error.HTTPError(request.full_url, 401, "no", {}, io.BytesIO(b""))
    with pytest.raises(jev.JevError, match="HTTP 401"):
        jev.make_predict("https://x", "k", "m", opener=refuse)({}, QUESTIONS)

    def down(request, timeout):
        raise urllib.error.URLError("no route to host")
    with pytest.raises(jev.JevError, match="unreachable"):
        jev.make_predict("https://x", "k", "m", opener=down)({}, QUESTIONS)
    with pytest.raises(jev.JevError, match="undecodable"):
        jev.make_predict("https://x", "k", "m", opener=lambda r, timeout: Reply(b"<html>"))({}, QUESTIONS)
    with pytest.raises(jev.JevError, match="not an object"):
        jev.make_predict("https://x", "k", "m", opener=lambda r, timeout: Reply([1, 2]))({}, QUESTIONS)


def test_normalize_accepts_answers_at_the_top_level_and_passes_a_strange_body_through() -> None:
    flat = {"pick": ANSWER["answers"]["pick"], "usage": {"input_tokens": 40}}
    assert jev.normalize(flat, QUESTIONS) == ANSWER
    assert jev.normalize(ANSWER, QUESTIONS) is ANSWER
    odd = {"result": "nope"}
    assert jev.normalize(odd, QUESTIONS) is odd                # the lane then reads "no answer": never a click


def test_load_warms_up_and_a_failed_warm_up_is_a_one_line_runtime_error() -> None:
    def opener(request, timeout):
        q = json.loads(request.data)["questions"]
        qid, crit = next(iter(q)), next(iter(q.values()))["criteria"]
        first = next(iter(crit))
        return Reply({"answers": {qid: {"choice": first, "confidence": 0.9,
                                        "probabilities": {k: (1.0 if k == first else 0.0) for k in crit}}}})

    d = jev.load(env={"TYPESAFE_API_KEY": "k"}, opener=opener)
    assert d.available and d.style == "jev" and d.max_len == jev.MAX_LEN
    with pytest.raises(RuntimeError, match="TYPESAFE_API_KEY is not set"):
        jev.load(env={})
    with pytest.raises(RuntimeError, match="warm-up predict failed"):
        jev.load(env={"TYPESAFE_API_KEY": "k"}, opener=lambda r, timeout: Reply({"answers": {}}))


def test_timeout_from_env_never_raises() -> None:
    from eve_bridge import jev as jev_mod

    assert jev_mod.timeout_from_env({}) == jev_mod.DEFAULT_TIMEOUT_S
    assert jev_mod.timeout_from_env({"EVE_JEV_TIMEOUT": " "}) == jev_mod.DEFAULT_TIMEOUT_S
    assert jev_mod.timeout_from_env({"EVE_JEV_TIMEOUT": "abc"}) == jev_mod.DEFAULT_TIMEOUT_S
    assert jev_mod.timeout_from_env({"EVE_JEV_TIMEOUT": "-2"}) == jev_mod.DEFAULT_TIMEOUT_S
    assert jev_mod.timeout_from_env({"EVE_JEV_TIMEOUT": "inf"}) == jev_mod.DEFAULT_TIMEOUT_S
    assert jev_mod.timeout_from_env({"EVE_JEV_TIMEOUT": "5"}) == 5.0
