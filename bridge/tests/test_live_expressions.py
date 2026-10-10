import asyncio
import threading
from contextlib import suppress

import pytest

from eve_bridge.eye_model import LABELS, ConversationContext
from eve_bridge.live_expressions import LiveExpressions


class Model:
    def predict(self, text):
        return {"probabilities": [0, 1] + [0] * (len(LABELS) - 2), "ms": 1.0}


async def until(predicate):
    for _ in range(200):
        if predicate():
            return
        await asyncio.sleep(0.01)
    raise AssertionError("worker did not reach expected state")


async def stop(task):
    task.cancel()
    with suppress(asyncio.CancelledError):
        await task


@pytest.mark.parametrize(
    "muted,phase,chirp", [(False, "speaking", False), (True, "speaking", False), (False, "listening", False)]
)
def test_queue_and_arbitration(tmp_path, muted, phase, chirp):
    async def run():
        sent = []

        async def send(cmd):
            sent.append(cmd)
            return True

        service = LiveExpressions(
            send,
            path=tmp_path / "settings.json",
            model_factory=Model,
            muted=lambda: muted,
            phase=lambda: phase,
        )
        await service.set_enabled(True)
        service.offer("assistant", "An earlier obsolete sentence.")
        event = service.offer("assistant", "This is such wonderful news!")
        task = asyncio.create_task(service.run())
        try:
            await until(lambda: len(sent) == 1)
            assert sent[0]["id"] == event and sent[0]["label"] == "happy"
            assert sent[0]["chirp"] is chirp
            assert 500 <= sent[0]["ttl_ms"] <= 4000
            assert service.dropped == 1
            assert service.offer("assistant", "This is such wonderful news!") is None
            await service.set_enabled(False)
            assert sent[-1]["clear"] is True
            assert service.offer("user", "hello") is None
        finally:
            await stop(task)

    asyncio.run(run())


def test_disabled_during_inference_drops_result(tmp_path):
    async def run():
        started, release = threading.Event(), threading.Event()

        class Slow(Model):
            def predict(self, text):
                started.set()
                release.wait(2)
                return super().predict(text)

        sent = []

        async def send(cmd):
            sent.append(cmd)

        service = LiveExpressions(send, path=tmp_path / "settings.json", model_factory=Slow)
        await service.set_enabled(True)
        service.offer("user", "Wonderful news!")
        task = asyncio.create_task(service.run())
        try:
            await until(started.is_set)
            await service.set_enabled(False)
            release.set()
            await until(lambda: service.dropped == 1)
            assert len(sent) == 1 and sent[0]["clear"]
        finally:
            release.set()
            await stop(task)

    asyncio.run(run())


def test_stale_and_send_failure(tmp_path):
    async def run():
        now = [1.0]

        async def send(cmd):
            return False

        service = LiveExpressions(
            send, path=tmp_path / "settings.json", model_factory=Model, clock=lambda: now[0]
        )
        await service.set_enabled(True)
        service.offer("diary", "Something interesting here.")
        now[0] = 10
        task = asyncio.create_task(service.run())
        try:
            await until(lambda: service.dropped == 1)
            service.offer("user", "A new interesting thing!")
            await until(lambda: service.dropped == 2)
            assert service.sent == 0 and service.last is None and "send failed" in service.error
        finally:
            await stop(task)

    asyncio.run(run())


@pytest.mark.parametrize("values", [[], [1] * 6, [float("nan"), 0, 0, 0, 0, 0], [True, 0, 0, 0, 0, 0]])
def test_invalid_results(values):
    with pytest.raises(ValueError):
        LiveExpressions.label({"probabilities": values})


def test_every_expression_is_accepted_without_startle():
    assert "startled" not in LABELS
    for i, label in enumerate(LABELS):
        values = [0] * len(LABELS)
        values[i] = 1
        assert LiveExpressions.label({"probabilities": values}) == (label, 1)


def test_context_has_speaker_roles_and_expires():
    context = ConversationContext()
    context.state("user", "My dog died.", 0)
    state = context.state("assistant", "I'm sorry to hear that.", 1)
    assert state.endswith("Eve: I'm sorry to hear that.")
    assert "User: My dog died." in state
    assert "My dog died" not in context.state("assistant", "What next?", 91)


@pytest.mark.parametrize("muted", [False, True])
def test_laya_keeps_original_caption_chirps(tmp_path, muted):
    from types import SimpleNamespace

    from eve_bridge.daemon import Daemon

    async def run():
        sent = []

        async def send(cmd):
            sent.append(cmd)

        daemon = SimpleNamespace(
            ble=SimpleNamespace(connected=True, send=send),
            _sound=SimpleNamespace(muted=muted),
            _expressions=SimpleNamespace(enabled=True, ready=True),
        )
        Daemon._on_caption(daemon, {"cmd": "caption", "chirp": True, "lines": ["Hello!"]})
        await asyncio.sleep(0)
        assert sent[0]["chirp"] is (not muted)

    asyncio.run(run())


# ---- Jev picks the eye (owner, 2026-09-24: "use jev for that too") --------------------------------------

def _jev_opener(probabilities):
    """A jev.make_predict opener that answers the eye choice with the given probabilities, no socket."""
    import io
    import json as _json
    from contextlib import contextmanager

    seen = []

    @contextmanager
    def opener(request, timeout=0):
        seen.append(_json.loads(request.data))
        top = max(probabilities, key=probabilities.get) if probabilities else ""
        body = {"answers": {"eye": {"choice": top, "probabilities": probabilities,
                                    "confidence": probabilities.get(top, 0.0)}}, "usage": {"input_tokens": 500}}
        yield io.BytesIO(_json.dumps(body).encode())

    return opener, seen


def test_jev_eye_model_asks_one_choice_over_the_eleven_labels_and_answers_in_label_order():
    from eve_bridge.eye_model import CRITERIA, JevEyeModel

    opener, seen = _jev_opener({"surprised": 0.9, "curious": 0.1})       # Jev may omit the zero labels
    model = JevEyeModel({"OPENROUTER_API_KEY": "k", "EVE_JEV_ROUTE": "openrouter"}, opener=opener)
    answer = model.predict("User: WHAT?! the server just went down??")
    assert answer["label"] == "surprised" and answer["ms"] >= 0
    assert len(answer["probabilities"]) == len(LABELS) and abs(sum(answer["probabilities"]) - 1) < 1e-9
    assert answer["probabilities"][LABELS.index("surprised")] == pytest.approx(0.9)
    assert LiveExpressions.label(answer) == ("surprised", pytest.approx(0.9))   # the worker accepts it as is
    (request,) = seen
    assert list(request["questions"]) == ["eye"] and request["questions"]["eye"]["type"] == "choice"
    assert request["questions"]["eye"]["criteria"] == CRITERIA and request["state"].startswith("User: WHAT")


def test_jev_eye_model_refuses_an_answer_without_probabilities_and_needs_a_key():
    from eve_bridge.eye_model import JevEyeModel
    from eve_bridge.jev import JevError

    opener, _ = _jev_opener({})
    model = JevEyeModel({"OPENROUTER_API_KEY": "k", "EVE_JEV_ROUTE": "openrouter"}, opener=opener)
    with pytest.raises(ValueError):
        model.predict("User: hi")
    with pytest.raises(JevError):                                       # no key: loud at construction, not per turn
        JevEyeModel({"EVE_JEV_ROUTE": "openrouter"}, opener=opener)


@pytest.mark.parametrize("value,expect", [("", "jev"), ("jev", "jev"), ("laya", "laya"), (" Laya ", "laya"),
                                          ("mlx", "jev")])
def test_the_expression_backend_is_jev_unless_laya_is_spelled_out(value, expect, tmp_path, monkeypatch):
    from eve_bridge.live_expressions import backend_from_env

    assert backend_from_env({"EVE_EXPRESSION_BACKEND": value}) == expect
    monkeypatch.setenv("EVE_EXPRESSION_BACKEND", value)
    service = LiveExpressions(lambda cmd: None, path=tmp_path / "settings.json", model_factory=Model)
    assert service.backend == expect and service.status()["backend"] == expect
