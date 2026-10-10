"""The OpenAI seam retries a TLS drop on a fresh connection (computer_agent.make_response_creator /
make_stream_creator), and a turn the retries could not save says so plainly (telegram.DROPPED_LINE).

Live, 2026-09-28: three phone-call turns died on a bare ``ssl.SSLError`` the SDK does not retry. Live,
2026-09-30 11:11: the one retry that fixed that went out 120 ms later on the same client (the same connection
pool) and failed the same way, and the owner got "Something went wrong on my side". These tests fake the
client so no network is touched.
"""

from __future__ import annotations

import asyncio
import logging
import ssl
from typing import Any

import pytest
from test_telegram import FakeApi, FakeCreate, Rig, run_rig, say, update

from eve_bridge import telegram
from eve_bridge.computer_agent import (
    TLS_BACKOFF,
    TLS_RETRIES,
    is_connection_drop,
    make_response_creator,
    make_stream_creator,
)


class Reply:
    def __init__(self, body: Any) -> None:
        self.body = body

    def model_dump(self, exclude_none: bool = False) -> dict[str, Any]:
        if isinstance(self.body, dict):
            return self.body
        return {"output": [{"type": "message", "content": [{"type": "output_text", "text": self.body}]}]}


def tls_drop() -> ssl.SSLError:
    err = ssl.SSLError(1, "[SSL: SSLV3_ALERT_BAD_RECORD_MAC] sslv3 alert bad record mac")
    err.reason = "SSLV3_ALERT_BAD_RECORD_MAC"
    return err


class FakeResponses:
    """``create`` and ``stream`` play a script: an exception to raise, or a reply (``create``) / a list of
    text deltas (``stream``). ``owner`` is told which client each call went out on."""

    def __init__(self, *script: Any, script_list: Any = None, owner: Any = None) -> None:
        self.script = script_list if script_list is not None else list(script)
        self.calls = 0
        self.owner = owner

    async def create(self, **request: Any) -> Reply:
        self.calls += 1
        if self.owner is not None:
            self.owner.used.append(self.owner.number)
        step = self.script.pop(0)
        if callable(step):
            step = await step()
        if isinstance(step, BaseException):
            raise step
        return Reply(step)

    def stream(self, **request: Any) -> "FakeStream":
        self.calls += 1
        if self.owner is not None:
            self.owner.used.append(self.owner.number)
        return FakeStream(self.script.pop(0))


class Delta:
    type = "response.output_text.delta"

    def __init__(self, delta: str) -> None:
        self.delta = delta


class FakeStream:
    """A step is a list: text deltas, and an exception raised where it appears in the list."""

    def __init__(self, step: Any) -> None:
        self.step = step if isinstance(step, list) else [step]

    async def __aenter__(self) -> "FakeStream":
        return self

    async def __aexit__(self, *exc: Any) -> None:
        return None

    def __aiter__(self) -> "FakeStream":
        self._it = iter(self.step)
        return self

    async def __anext__(self) -> Delta:
        try:
            item = next(self._it)
        except StopIteration:
            raise StopAsyncIteration from None
        if isinstance(item, BaseException):
            raise item
        return Delta(item)

    async def get_final_response(self) -> Reply:
        return Reply("".join(s for s in self.step if isinstance(s, str)))


class FakeClient:
    """One fixed client (the old seam's tests): every call goes out on it."""

    def __init__(self, *script: Any) -> None:
        self.responses = FakeResponses(*script)


class PooledClient:
    """One client the factory made: its own connection pool, as far as the seam can tell."""

    def __init__(self, factory: "Factory", number: int) -> None:
        self.number = number
        self.used = factory.used
        self.closed = False
        self.responses = FakeResponses(script_list=factory.script, owner=self)
        factory.clients.append(self)

    async def close(self) -> None:
        self.closed = True


class Factory:
    """Makes a new client per call; all of them share one script, in order."""

    def __init__(self, *script: Any) -> None:
        self.script = list(script)
        self.clients: list[PooledClient] = []
        self.used: list[int] = []            # which client each request went out on

    def __call__(self) -> PooledClient:
        return PooledClient(self, len(self.clients))


class Sleeps:
    def __init__(self) -> None:
        self.waits: list[float] = []

    async def __call__(self, secs: float) -> None:
        self.waits.append(secs)


def text_of(response: dict[str, Any]) -> str:
    return response["output"][0]["content"][0]["text"]


# ---- the retry -------------------------------------------------------------------------------------------

def test_the_backoff_is_short_and_bounded() -> None:
    assert TLS_BACKOFF == (0.3, 1.0) and TLS_RETRIES == 2


def test_a_tls_drop_is_retried_on_a_new_client_after_a_pause() -> None:
    made, sleeps = Factory(tls_drop(), "hello"), Sleeps()
    create = make_response_creator(client_factory=made, sleep=sleeps)
    assert text_of(asyncio.run(create({}))) == "hello"
    assert made.used == [0, 1]                       # the retry went out on a NEW client, not the dropped one
    assert sleeps.waits == [0.3]
    assert made.clients[0].closed and not made.clients[1].closed
    # and the next call keeps the new client: the dropped pool is not used again
    made.script.append("again")
    assert text_of(asyncio.run(create({}))) == "again" and made.used == [0, 1, 1]


def test_two_drops_then_an_answer() -> None:
    made, sleeps = Factory(tls_drop(), tls_drop(), "hi"), Sleeps()
    create = make_response_creator(client_factory=made, sleep=sleeps)
    assert text_of(asyncio.run(create({}))) == "hi"
    assert made.used == [0, 1, 2] and sleeps.waits == [0.3, 1.0]
    assert [c.closed for c in made.clients] == [True, True, False]


def test_a_drop_every_time_gives_up_after_three_tries() -> None:
    made, sleeps = Factory(tls_drop(), tls_drop(), tls_drop(), "never"), Sleeps()
    create = make_response_creator(client_factory=made, sleep=sleeps)
    with pytest.raises(ssl.SSLError):
        asyncio.run(create({}))
    assert made.used == [0, 1, 2] and sleeps.waits == [0.3, 1.0]
    assert made.script == ["never"]


def test_other_errors_are_not_retried() -> None:
    # the positive control: the retry is for a dropped connection only, not for a model or API error
    made, sleeps = Factory(RuntimeError("boom"), "never"), Sleeps()
    create = make_response_creator(client_factory=made, sleep=sleeps)
    with pytest.raises(RuntimeError):
        asyncio.run(create({}))
    assert made.used == [0] and sleeps.waits == [] and len(made.clients) == 1 and not made.clients[0].closed


def test_a_fixed_client_still_retries() -> None:
    # tests and callers that pass one client: no factory, so the retry reuses it (the pause still applies)
    client, sleeps = FakeClient(tls_drop(), "hello"), Sleeps()
    create = make_response_creator(client=client, sleep=sleeps)
    assert text_of(asyncio.run(create({}))) == "hello"
    assert client.responses.calls == 2 and sleeps.waits == [0.3]


def test_a_dropped_client_is_closed_only_after_its_other_calls_finish() -> None:
    # two turns share the seam: one is mid-request on client 0 when the other's request on it drops
    release = asyncio.Event()

    async def slow() -> str:
        await release.wait()
        return "slow answer"

    made = Factory(slow, tls_drop(), "fast answer")
    create = make_response_creator(client_factory=made, sleep=Sleeps())

    async def go() -> tuple[str, str]:
        a = asyncio.ensure_future(create({}))
        await asyncio.sleep(0)
        b = await create({})                         # drops on client 0, retried on client 1
        assert not made.clients[0].closed            # client 0 is still carrying the slow call
        release.set()
        return text_of(await a), text_of(b)

    assert asyncio.run(go()) == ("slow answer", "fast answer")
    assert made.used == [0, 0, 1] and made.clients[0].closed and not made.clients[1].closed


# ---- the streamed call -----------------------------------------------------------------------------------

def test_a_stream_that_drops_before_any_text_is_retried_on_a_new_client() -> None:
    made, sleeps = Factory([tls_drop()], ["It's ", "a desk."]), Sleeps()
    create = make_stream_creator(client_factory=made, sleep=sleeps)
    heard: list[str] = []
    out = asyncio.run(create({}, heard.append))
    assert heard == ["It's ", "a desk."] and text_of(out) == "It's a desk."
    assert made.used == [0, 1] and sleeps.waits == [0.3] and made.clients[0].closed


def test_a_stream_that_drops_after_text_was_read_out_is_not_retried() -> None:
    # a retry would read "It's " out twice; the turn fails instead and the caller says so
    made, sleeps = Factory(["It's ", tls_drop()], ["never"]), Sleeps()
    create = make_stream_creator(client_factory=made, sleep=sleeps)
    heard: list[str] = []
    with pytest.raises(ssl.SSLError):
        asyncio.run(create({}, heard.append))
    assert heard == ["It's "] and made.used == [0] and sleeps.waits == []


def test_a_stream_that_drops_every_time_gives_up_after_three_tries() -> None:
    made, sleeps = Factory([tls_drop()], [tls_drop()], [tls_drop()], ["never"]), Sleeps()
    create = make_stream_creator(client_factory=made, sleep=sleeps)
    with pytest.raises(ssl.SSLError):
        asyncio.run(create({}, lambda t: None))
    assert made.used == [0, 1, 2] and sleeps.waits == [0.3, 1.0]


# ---- what the owner is told --------------------------------------------------------------------------------

def test_a_connection_drop_is_told_apart_from_other_failures() -> None:
    import httpx
    from openai import APIConnectionError

    request = httpx.Request("POST", "https://api.openai.com/v1/responses")
    assert is_connection_drop(tls_drop())
    assert is_connection_drop(ConnectionResetError())
    assert is_connection_drop(APIConnectionError(request=request))
    assert not is_connection_drop(RuntimeError("boom"))
    assert not is_connection_drop(ValueError("bad json"))


def rig_with_seam(made: Factory) -> Rig:
    async def no_sleep(_secs: float) -> None:
        await asyncio.sleep(0)

    create = make_response_creator(client_factory=made, sleep=no_sleep)
    return Rig(FakeApi([update("Send me the links")]), create)          # type: ignore[arg-type]


def test_a_turn_whose_first_request_drops_is_answered() -> None:
    made = Factory(tls_drop(), say("Here they are."))
    rig = rig_with_seam(made)
    run_rig(rig)
    assert [t for _, t in rig.api.sent] == ["Here they are."]
    assert made.used == [0, 1]


def test_a_turn_the_retries_cannot_save_says_the_connection_dropped(caplog: pytest.LogCaptureFixture) -> None:
    made = Factory(*[tls_drop() for _ in range(TLS_RETRIES + 1)])
    rig = rig_with_seam(made)
    with caplog.at_level(logging.WARNING):
        run_rig(rig)
    assert [t for _, t in rig.api.sent] == [telegram.DROPPED_LINE]
    assert made.used == [0, 1, 2]
    assert "turn failed: SSLError" in caplog.text
    assert caplog.text.count("TLS dropped mid-request") == TLS_RETRIES


def test_any_other_failure_keeps_the_generic_line() -> None:
    rig = Rig(FakeApi([update("hello?")]), FakeCreate(RuntimeError("503")))
    run_rig(rig)
    assert [t for _, t in rig.api.sent] == [telegram.FAILED_LINE]

