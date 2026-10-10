"""Clef (clef.py): Cloudflare's Jev-compatible decision model behind the visual judge's fallback slot. Mocked only:
no Cloudflare key exists on this Mac, so these replies are the documented shapes, not recorded ones."""

from __future__ import annotations

import base64
import io
import json
import urllib.error

import pytest
from PIL import Image

from eve_bridge import clef

QUESTIONS = {"verified": {"type": "noul", "instructions": "Done?"}}
ANSWERS = {"verified": {"type": "noul", "noul": 0.97}}


class Response(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


def poster(payload, seen=None):
    def post(request, timeout):
        if seen is not None:
            seen.append((request, timeout))
        return Response(json.dumps(payload).encode())
    return post


@pytest.fixture(autouse=True)
def no_spend(monkeypatch):
    monkeypatch.setattr(clef.spend, "record", lambda *a, **kw: None)


def test_request_shape_and_headers():
    seen = []
    p = clef.make_predict("https://example.invalid/run", "tok", "clef",
                          post=poster({"answers": ANSWERS, "usage": {"input_tokens": 9}}, seen))
    out = p({"step": "x"}, QUESTIONS, [Image.new("RGB", (10, 10))], 1.0)
    assert out["answers"] == ANSWERS
    request, timeout = seen[0]
    body = json.loads(request.data)
    assert body["model"] == "clef" and body["questions"] == QUESTIONS and body["state"] == {"step": "x"}
    assert len(body["images"]) == 1 and body["images"][0].startswith("data:image/png;base64,")
    assert request.get_header("Authorization") == "Bearer tok" and timeout <= clef.DEFAULT_TIMEOUT_S


def test_at_most_four_images_and_no_remote_urls():
    p = clef.make_predict("https://example.invalid/u", "t", "clef", post=poster({"answers": ANSWERS}))
    with pytest.raises(clef.ClefError):
        p({}, QUESTIONS, [Image.new("RGB", (4, 4))] * 5, 1.0)
    with pytest.raises(clef.ClefError):
        p({}, QUESTIONS, ["https://example.invalid/x.png"], 1.0)
    big = "data:image/png;base64," + base64.b64encode(b"\0" * (clef.MAX_IMAGE_BYTES + 1)).decode()
    with pytest.raises(clef.ClefError):
        p({}, QUESTIONS, [big], 1.0)


@pytest.mark.parametrize("payload", [{"answers": ANSWERS}, {"result": {"answers": ANSWERS}, "success": True,
                                                              "errors": []}, {"verified": ANSWERS["verified"]}])
def test_bare_and_enveloped_replies_both_unwrap(payload):
    out = clef.make_predict("https://example.invalid/u", "t", "clef", post=poster(payload))({}, QUESTIONS, [], 1.0)
    assert out["answers"] == ANSWERS


@pytest.mark.parametrize("payload", [{"success": False, "errors": [{"message": "x"}], "result": None},
                                     {"result": {"nothing": 1}}, ["answers"]])
def test_failure_replies_are_errors(payload):
    with pytest.raises(clef.ClefError):
        clef.make_predict("https://example.invalid/u", "t", "clef", post=poster(payload))({}, QUESTIONS, [], 1.0)


def test_http_errors_do_not_leak_the_token_or_account():
    def post(request, timeout):
        raise urllib.error.HTTPError(request.full_url, 403, "forbidden", {}, None)
    with pytest.raises(clef.ClefError) as e:
        clef.make_predict("https://api.cloudflare.com/client/v4/accounts/acct-123/ai/run/x", "tok-secret", "clef",
                          post=post)({}, QUESTIONS, [], 1.0)
    assert "tok-secret" not in str(e.value) and "acct-123" not in str(e.value)


def test_slot_is_empty_without_both_keys():
    assert clef.from_env({}) is None
    assert clef.from_env({clef.ACCOUNT_ENV: "a"}) is None
    assert clef.from_env({clef.TOKEN_ENV: "t"}) is None
    judge = clef.from_env({clef.ACCOUNT_ENV: "a", clef.TOKEN_ENV: "t", clef.MODEL_ENV: "clef-flash"})
    assert judge is not None and judge.model == "clef-flash"
    url, _, model, _ = clef.route_config({clef.ACCOUNT_ENV: "a", clef.TOKEN_ENV: "t", clef.MODEL_ENV: "other"})
    assert url.endswith("/accounts/a/ai/run/@cf/cloudflare/clef") and model == "clef"


def test_spend_is_metered_at_clefs_rate(monkeypatch):
    rows = []
    monkeypatch.setattr(clef.spend, "record", lambda *a, **kw: rows.append((a, kw)))
    clef.make_predict("https://example.invalid/u", "t", "clef", post=poster({"answers": ANSWERS, "usage": {"input_tokens": 1_000_000}}))(
        {}, QUESTIONS, [], 1.0)
    assert rows[0][0][0] == "cloudflare" and abs(rows[0][0][3] - 0.24) < 1e-9
