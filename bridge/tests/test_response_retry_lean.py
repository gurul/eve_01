"""The traces from verification/Eve/ResponseRetry.lean, replayed against the real make_stream_creator (a fake
OpenAI client). Every `fixed_*` theorem is run against the fixed code."""

from __future__ import annotations

import asyncio
import ssl

import pytest
from test_response_creators import FakeClient, text_of, tls_drop

from eve_bridge.computer_agent import make_stream_creator


async def _no_wait(_secs: float) -> None:
    return None


def _stream(client):
    return make_stream_creator(client=client, sleep=_no_wait)


def test_response_retry_fixed_replays_counterexample():
    """[drop, text, done]: succeeded, not failed, nothing read out twice."""
    client = FakeClient([tls_drop()], ['It is ', 'four.'])
    heard: list[str] = []
    out = asyncio.run(_stream(client)({}, heard.append))
    assert text_of(out) == 'It is four.' and heard == ['It is ', 'four.']


def test_response_retry_fixed_survives_two_drops():
    """[drop, drop, text, done]: the 2026-09-30 trace (a drop, then a drop on the retry) now succeeds."""
    client = FakeClient([tls_drop()], [tls_drop()], ['It is ', 'four.'])
    heard: list[str] = []
    out = asyncio.run(_stream(client)({}, heard.append))
    assert text_of(out) == 'It is four.' and heard == ['It is ', 'four.']


def test_response_retry_fixed_never_repeats():
    """[text, drop, text]: failed, not duplicated — the naive retry would have read the sentence out again."""
    client = FakeClient(['It is ', tls_drop()], ['It is ', 'four.'])
    heard: list[str] = []
    with pytest.raises(ssl.SSLError):
        asyncio.run(_stream(client)({}, heard.append))
    assert heard == ['It is ']


def test_response_retry_three_drops_fail():
    """[drop, drop, drop]: failed with droppedEvery — the only other way a turn may fail."""
    client = FakeClient([tls_drop()], [tls_drop()], [tls_drop()], ['never'])
    with pytest.raises(ssl.SSLError):
        asyncio.run(_stream(client)({}, lambda t: None))
    assert client.responses.calls == 3
