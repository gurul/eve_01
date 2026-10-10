"""scene.py: timestamped views for the voice, with a fake describer, a fake
clock and a fake wall clock. The watcher is stepped by hand (start(run_loop=False)
then tick()), so every staleness and camera-loss rule is deterministic."""

from __future__ import annotations

import asyncio
import base64
import threading
from datetime import datetime, timedelta
from types import SimpleNamespace

import pytest

from eve_bridge.scene import (
    NOTE_MAX_CHARS,
    Observation,
    OpenAISceneClient,
    SceneConfig,
    SceneWatcher,
    configured,
    format_note,
    parse_scene,
)

T0 = datetime(2026, 9, 10, 16, 43, 0)


class Clock:
    def __init__(self) -> None:
        self.t = 100.0

    def __call__(self) -> float:
        return self.t

    def wall(self) -> datetime:
        return T0 + timedelta(seconds=self.t - 100.0)


class FakeClient:
    """Scripted answers. Records the image bytes each describe got."""

    def __init__(self, answers: list[tuple[str, str]], clock: Clock | None = None, takes: float = 0.0) -> None:
        self.answers = list(answers)
        self.images: list[bytes] = []
        self.previous: list[str | None] = []
        self.clock = clock
        self.takes = takes

    def describe(self, image: bytes, mime: str, previous):
        self.images.append(image)
        self.previous.append(previous)
        if self.clock is not None:
            self.clock.t += self.takes            # the answer comes back later than the frame
        if not self.answers:
            raise RuntimeError("no more answers")
        return self.answers.pop(0)


def jpeg_frame(seq: int, tag: bytes | None = None) -> dict:
    data = b"\xff\xd8" + (tag or f"frame{seq}".encode())
    return {"seq": seq, "w": 160, "h": 120, "fmt": "jpeg", "b64": base64.b64encode(data).decode()}


def watcher(client, clock: Clock, **cfg) -> SceneWatcher:
    config = SceneConfig(**{"interval_secs": 3.0, "stale_secs": 15.0, "camera_lost_secs": 3.0,
                            "timeout_secs": 2.0, "refresh_secs": 20.0, **cfg})
    return SceneWatcher(client, config, clock=clock, wall=clock.wall)


# ---- pure helpers ----------------------------------------------------------------------

def test_parse_scene_json_plain_and_none_change() -> None:
    assert parse_scene('{"scene":"A mug on a desk.","change":"mug moved left"}') == (
        "A mug on a desk.", "mug moved left")
    assert parse_scene('{"scene":"A mug.","change":"None."}') == ("A mug.", "")
    assert parse_scene("  A plain   sentence.  ") == ("A plain sentence.", "")
    with pytest.raises(ValueError):
        parse_scene("")
    with pytest.raises(ValueError):
        parse_scene('{"scene":"","change":"x"}')


def test_format_note_carries_seen_time_and_change_and_is_clipped() -> None:
    obs = Observation(seen_at=1.0, seen_wall=T0, text="A person holds a green mug.",
                      change="they picked it up", latency_secs=0.4)
    assert format_note(obs) == "[vision 16:43:00] A person holds a green mug. Change: they picked it up"
    long = Observation(seen_at=1.0, seen_wall=T0, text="x" * 900, change="", latency_secs=0.1)
    assert len(format_note(long)) <= NOTE_MAX_CHARS


def test_configured_reads_env() -> None:
    assert configured({}).model == "gpt-6-luna" and configured({}).enabled
    c = configured({"EVE_SCENE": "0", "EVE_SCENE_MODEL": "gpt-5-mini",
                    "EVE_SCENE_INTERVAL_SECS": "0.1", "EVE_SCENE_STALE_SECS": "abc"})
    assert not c.enabled and c.model == "gpt-5-mini"
    assert c.interval_secs == 1.0          # clamped to the floor
    assert c.stale_secs == 15.0            # not a number: the default


# ---- the watcher -------------------------------------------------------------------------

def test_frames_are_ignored_outside_a_conversation() -> None:
    async def go():
        clock = Clock()
        w = watcher(FakeClient([("A desk.", "")]), clock)
        w.offer(jpeg_frame(1))
        assert w._frame is None                 # not started: nothing is held
        w.start(run_loop=False)
        w.offer(jpeg_frame(2))
        assert w._frame is not None
        await w.stop()
        assert w._frame is None and w.latest is None and w.notes == []
        w.offer(jpeg_frame(3))
        assert w._frame is None                 # stopped again: nothing is held
    asyncio.run(go())


def test_disabled_watcher_never_starts() -> None:
    async def go():
        clock = Clock()
        w = watcher(None, clock)
        w.start(run_loop=False)
        assert not w.enabled and not w.active
        assert (await w.look())["ok"] is False
        w2 = SceneWatcher(FakeClient([]), SceneConfig(enabled=False), clock=clock, wall=clock.wall)
        w2.start(run_loop=False)
        assert not w2.active
    asyncio.run(go())


def test_only_the_newest_frame_is_described_and_note_uses_seen_time() -> None:
    async def go():
        clock = Clock()
        client = FakeClient([("A person at a desk.", "")], clock=clock, takes=2.0)
        w = watcher(client, clock)
        w.start(run_loop=False)
        for seq in (1, 2, 3):
            w.offer(jpeg_frame(seq))
        clock.t += 0.5                        # the frame arrived at 16:43:00, the tick is later
        await w.tick()
        assert client.images == [b"\xff\xd8frame3"]          # latest-only: 1 and 2 never left
        assert w._frame is None                               # released once handed off
        assert w.notes == ["[vision 16:43:00] A person at a desk."]   # seen time, not answer time (+2.5 s)
        assert w.latest is not None and w.latest.latency_secs == pytest.approx(2.0)
        await w.stop()
    asyncio.run(go())


def test_change_is_announced_and_sameness_is_repeated_on_refresh() -> None:
    async def go():
        clock = Clock()
        client = FakeClient([("A desk with a mug.", ""), ("A desk with a mug.", ""),
                             ("A hand lifts the mug.", "the mug was picked up")])
        w = watcher(client, clock, refresh_secs=10.0)
        w.start(run_loop=False)
        w.offer(jpeg_frame(1))
        await w.tick()                                           # first view: always a note
        clock.t += 3.0
        w.offer(jpeg_frame(2))
        await w.tick()                                           # no change: no note
        assert len(w.notes) == 1
        assert client.previous[1] == "A desk with a mug."        # the describer compares
        clock.t += 8.0
        w.offer(jpeg_frame(3))                                   # keep the camera alive...
        clock.t += 0.0
        w._last_describe_at = clock.t                            # ...but not due for a describe yet
        await w.tick()
        # Stamped with the view that confirmed it (16:43:03), not with the time of the tick (16:43:11).
        assert w.notes[-1] == "[vision 16:43:03] No change since 16:43:00: A desk with a mug."
        n = len(w.notes)
        clock.t += 25.0                                          # 16:43:36, a full refresh later
        w.offer(jpeg_frame(3))
        w._last_describe_at = clock.t
        await w.tick()
        assert len(w.notes) == n                 # nothing newer confirmed it: no repeat of a stale line
        clock.t += 3.0                                           # 16:43:39: due again
        w.offer(jpeg_frame(4))
        await w.tick()
        assert w.notes[-1] == "[vision 16:43:39] A hand lifts the mug. Change: the mug was picked up"
        await w.stop()
    asyncio.run(go())


def test_camera_loss_is_said_once_after_a_grace_and_board_loss_counts() -> None:
    async def go():
        clock = Clock()
        cam = SimpleNamespace(ok=True)
        client = FakeClient([("A desk.", "")])
        w = SceneWatcher(client, SceneConfig(camera_lost_secs=3.0), clock=clock, wall=clock.wall,
                         camera_ok=lambda: cam.ok)
        w.start(run_loop=False)
        await w.tick()
        assert w.notes == []                                    # inside the start grace: nothing said
        w.offer(jpeg_frame(1))
        await w.tick()
        assert len(w.notes) == 1
        clock.t += 3.5                                          # no frame for longer than camera_lost_secs
        await w.tick()
        await w.tick()
        lost = [n for n in w.notes if "Camera lost" in n]
        assert lost == ["[vision 16:43:03] Camera lost: you cannot see anything right now. "
                        "Your last view, from 16:43:00, may be out of date."]
        look = await w.look()
        assert look["ok"] is False and "not connected" in look["reason"] and look["view"] == "A desk."
        # The board goes away while frames are fresh: still lost.
        w.offer(jpeg_frame(2))
        cam.ok = False
        assert w.camera_state(clock.t) == "lost"
        await w.stop()
    asyncio.run(go())


def test_camera_never_delivering_a_frame_is_reported_after_the_grace() -> None:
    async def go():
        clock = Clock()
        w = watcher(FakeClient([]), clock)
        w.start(run_loop=False)
        clock.t += 3.1
        await w.tick()
        assert w.notes == ["[vision 16:43:03] Camera lost: you cannot see anything right now."]
        await w.stop()
    asyncio.run(go())


def test_slow_describe_times_out_and_blindness_is_said_once() -> None:
    async def go():
        clock = Clock()
        gate = threading.Event()

        class Slow:
            def describe(self, image, mime, previous):
                gate.wait(2.0)
                return ("late", "")

        w = SceneWatcher(Slow(), SceneConfig(timeout_secs=0.05), clock=clock, wall=clock.wall)
        w.start(run_loop=False)
        w.offer(jpeg_frame(1))
        await w.tick()
        clock.t += 3.0
        w.offer(jpeg_frame(2))
        await w.tick()
        gate.set()
        assert w.failures == 2 and w.latest is None
        assert [n for n in w.notes if "cannot make out" in n] == [
            "[vision 16:43:00] You cannot make out the view right now."]
        await w.stop()
    asyncio.run(go())


def test_look_reuses_a_fresh_view_and_refreshes_a_stale_one() -> None:
    async def go():
        clock = Clock()
        client = FakeClient([("A desk.", ""), ("A cat on the desk.", "a cat jumped up")])
        w = watcher(client, clock, interval_secs=30.0)
        w.start(run_loop=False)
        w.offer(jpeg_frame(1))
        await w.tick()
        clock.t += 1.0
        fresh = await w.look()
        assert fresh == {"ok": True, "view": "A desk.", "seen_at": "16:43:00", "age_secs": 1.0, "stale": False}
        assert len(client.images) == 1                          # no second call for a 1 s old view

        clock.t += 9.0                                          # 10 s old: look asks for a new one
        w.offer(jpeg_frame(2))

        async def stepper():
            for _ in range(50):
                await asyncio.sleep(0)
                await w.tick()
        step = asyncio.create_task(stepper())
        refreshed = await w.look()
        await step
        assert len(client.images) == 2                          # interval was 30 s: look forced it
        assert refreshed["ok"] and refreshed["view"] == "A cat on the desk." and refreshed["seen_at"] == "16:43:10"
        await w.stop()
    asyncio.run(go())


def test_look_outside_a_conversation_says_so() -> None:
    async def go():
        clock = Clock()
        w = watcher(FakeClient([]), clock)
        out = await w.look()
        assert out == {"ok": False, "reason": "vision only runs during a conversation"}
    asyncio.run(go())


def test_a_full_cycle_writes_nothing_to_disk(tmp_path, monkeypatch) -> None:
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("HOME", str(tmp_path))

    async def go():
        clock = Clock()
        w = watcher(FakeClient([("A desk.", ""), ("A desk.", "")]), clock)
        w.start(run_loop=False)
        for i in range(4):
            w.offer(jpeg_frame(i))
            clock.t += 3.0
            await w.tick()                  # two describes answer, two fail: both paths run
        assert w.describes == 2 and w.failures == 2
        out = await w.look()                # the forced describe fails too: bounded, and says so
        assert out["ok"] is False
        await w.stop()
    asyncio.run(go())
    assert list(tmp_path.rglob("*")) == []


# ---- the network client (no network: the SDK call is replaced) ----------------------------------

def test_request_is_not_stored_and_sends_one_low_detail_image() -> None:
    c = OpenAISceneClient("gpt-5.4-nano", api_key="sk-test")
    body = c.request(b"\xff\xd8abc", "image/jpeg", "A desk.")
    assert body["store"] is False
    assert body["model"] == "gpt-5.4-nano"
    assert body["reasoning"] == {"effort": "low"}
    assert body["max_output_tokens"] >= 1024
    parts = body["input"][0]["content"]
    assert [p["type"] for p in parts] == ["input_text", "input_image"]
    assert parts[1]["detail"] == "low" and parts[1]["image_url"].startswith("data:image/jpeg;base64,")
    assert "Previous description: A desk." in parts[0]["text"]
    fmt = body["text"]["format"]
    assert fmt["type"] == "json_schema" and fmt["strict"] is True


def test_describe_parses_the_sdk_answer() -> None:
    c = OpenAISceneClient("gpt-5.4-nano", api_key="sk-test")
    seen: dict = {}

    def create(**kw):
        seen.update(kw)
        return SimpleNamespace(output_text='{"scene":"A mug.","change":""}', status="completed",
                               incomplete_details=None)

    c._client = SimpleNamespace(responses=SimpleNamespace(create=create))
    assert c.describe(b"\xff\xd8x", "image/jpeg", None) == ("A mug.", "")
    assert seen["store"] is False
