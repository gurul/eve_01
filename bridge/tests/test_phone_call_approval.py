"""Recording-time approval identity: actual Call -> Telegram -> purchase adapter, entirely offline."""

from __future__ import annotations

import asyncio
import json
import threading
from typing import Any

import pytest
from test_phone_call import Brain, Voice
from test_telegram import OWNER, FakeApi, FakeCreate, Rig

from eve_bridge import phone_call


class Socket:
    """In-memory call wire; no microphone, socket, model or purchase API."""

    closed = False

    def __init__(self) -> None:
        self.frames: asyncio.Queue[tuple[int, bytes]] = asyncio.Queue()
        self.sent: list[dict[str, Any]] = []

    def command(self, kind: str) -> None:
        self.frames.put_nowait((phone_call.OP_TEXT, json.dumps({"type": kind}).encode()))

    async def recv(self) -> tuple[int, bytes]:
        return await self.frames.get()

    async def send_json(self, data: dict[str, Any]) -> None:
        self.sent.append(data)

    async def send_bytes(self, data: bytes) -> None:
        pass


class HeldVoice(Voice):
    """The production upload STT worker is held until the test changes the question."""

    def __init__(self, *, live: bool = False) -> None:
        super().__init__()
        self.entered = {i: threading.Event() for i in (1, 2)}
        self.release = {i: threading.Event() for i in (1, 2)}
        self.finished = {i: threading.Event() for i in (1, 2)}
        self.words = {1: "yes", 2: "yes"}
        self.ears = HeldEars(self) if live else None

    def live_ears(self) -> Any:
        return self.ears

    def transcribe(self, pcm: bytes) -> str:
        i = pcm[0]
        self.entered[i].set()
        try:
            assert self.release[i].wait(3), "test did not release STT"
            return self.words[i]
        finally:
            self.finished[i].set()

    def unblock(self) -> None:
        for event in self.release.values():
            event.set()


class HeldEars:
    """Hold live STT too: capture context must be independent of the transcription provider."""

    broken = False

    def __init__(self, voice: HeldVoice) -> None:
        self.voice, self.i = voice, 0

    async def open(self) -> None:
        pass

    async def begin(self) -> None:
        self.i = 0

    async def feed(self, pcm: bytes) -> None:
        self.i = pcm[0]

    async def finish(self, timeout: float) -> str:
        i = self.i
        self.voice.entered[i].set()
        await until(self.voice.release[i].is_set)
        self.voice.finished[i].set()
        return self.voice.words[i]

    async def close(self) -> None:
        pass


class Service:
    """No requests: only a record of what the real owner-approval adapter allowed."""

    def __init__(self) -> None:
        self.confirmed: list[Any] = []

    def tools(self) -> list[Any]:
        return []

    def instructions(self) -> str:
        return "Purchase only after the owner's explicit approval."

    def prepare_confirm(self, conversation_id: str, cart_hashes: list[str]) -> dict[str, Any]:
        return {"ok": True, "snapshot": conversation_id, "question": "Buy paper for $5.00?",
                "about": "Approval requested for a $5.00 cart."}

    def confirm(self, snapshot: Any, *, cancelled: threading.Event) -> dict[str, Any]:
        assert not cancelled.is_set()
        self.confirmed.append(snapshot)
        return {"ok": True, "status": "order_placed"}


async def until(predicate: Any) -> None:
    async with asyncio.timeout(3):
        while not predicate():
            await asyncio.sleep(0.001)


class Harness:
    def __init__(self, *, live: bool = False) -> None:
        self.service, self.voice, self.ws = Service(), HeldVoice(live=live), Socket()
        self.rig = Rig(FakeApi(), FakeCreate(), agentcard=self.service)
        self.call = phone_call.Call(self.ws, self.rig.inlet, self.voice)
        self.run = asyncio.create_task(self.call.run())
        self.jobs: list[asyncio.Task[Any]] = []

    def question(self) -> Any:
        return self.rig.inlet._open_question(asyncio.get_running_loop().create_future(), OWNER,
                                             strict=True, words=frozenset({"yes", "no"}), firm=True)

    async def purchase(self) -> tuple[Any, asyncio.Task[Any]]:
        previous = self.rig.inlet.answer_context()
        job = asyncio.create_task(self.rig.inlet._tool("agentcard_confirm", {
            "conversation_id": "purchase-B", "cart_hashes": ["0123456789abcdef"]}, OWNER))
        self.jobs.append(job)
        await until(lambda: self.rig.inlet.answer_context() is not previous)
        question = self.rig.inlet.answer_context()
        assert question is not None and question.firm
        return question, job

    async def begin(self) -> None:
        flushes = sum(m["type"] == "flush" for m in self.ws.sent)
        self.ws.command("talk")
        await until(lambda: sum(m["type"] == "flush" for m in self.ws.sent) > flushes)

    async def release(self, i: int) -> None:
        self.ws.frames.put_nowait((phone_call.OP_BIN, bytes([i, 0]) * phone_call.SAMPLE_RATE))
        self.ws.command("done")
        await until(self.voice.entered[i].is_set)

    async def finish(self, i: int) -> None:
        before = self.call.presses
        self.voice.release[i].set()
        await until(lambda: self.call.presses > before)
        # hear runs synchronously before the released-press task finishes; answer handling is queued by Telegram.
        await until(lambda: not self.call.tasks)
        await asyncio.sleep(0)

    async def close(self) -> None:
        self.voice.unblock()
        if not self.run.done():
            self.ws.command("end")
        await asyncio.wait_for(self.run, 3)
        for job in self.jobs:
            if not job.done():
                job.cancel()
        await asyncio.gather(*self.jobs, return_exceptions=True)
        await self.rig.inlet._shutdown()


@pytest.mark.parametrize("initial_question", [True, False])
@pytest.mark.parametrize("change_before_release", [True, False])
@pytest.mark.parametrize("live", [True, False])
def test_old_spoken_yes_cannot_approve_new_purchase(initial_question: bool, change_before_release: bool,
                                                   live: bool) -> None:
    async def run() -> None:
        h = Harness(live=live)
        try:
            old = h.question() if initial_question else None
            await h.begin()  # capture A, or capture the absence of any question
            if change_before_release:
                new, purchase = await h.purchase()
                await h.release(1)
            else:
                await h.release(1)  # STT is running, but has no text yet
                new, purchase = await h.purchase()
            await h.finish(1)
            assert not new.future.done() and not purchase.done() and not h.service.confirmed
            assert old is None or not old.future.done()
            # Positive control: a fresh press under B really can authorize this same purchase.
            await h.begin()
            await h.release(2)
            await h.finish(2)
            result = await asyncio.wait_for(purchase, 3)
            assert new.future.result() == "yes" and result["status"] == "order_placed"
            assert h.service.confirmed == ["purchase-B"]
        finally:
            await h.close()

    asyncio.run(run())


def test_context_is_captured_before_begin_press_yields() -> None:
    async def run() -> None:
        h = Harness()
        entered, resume = asyncio.Event(), asyncio.Event()
        interrupt = h.call.interrupt

        async def held_interrupt() -> None:
            entered.set()
            await resume.wait()
            await interrupt()

        h.call.interrupt = held_interrupt
        try:
            h.question()
            h.ws.command("talk")
            await asyncio.wait_for(entered.wait(), 3)
            new, purchase = await h.purchase()
            resume.set()
            await h.release(1)
            await h.finish(1)
            assert not new.future.done() and not purchase.done() and not h.service.confirmed
        finally:
            resume.set()
            await h.close()

    asyncio.run(run())


def test_overlapping_transcriptions_keep_each_presses_question_identity() -> None:
    async def run() -> None:
        h = Harness()
        try:
            old = h.question()
            await h.begin()
            await h.release(1)
            new, purchase = await h.purchase()
            await h.begin()
            await h.release(2)
            # The first transcript arrives while the second press is held in STT. It must still carry A.
            h.voice.release[1].set()
            await until(lambda: h.call.presses == 1)
            assert not old.future.done() and not new.future.done() and not h.service.confirmed
            h.voice.release[2].set()
            result = await asyncio.wait_for(purchase, 3)
            assert result["status"] == "order_placed" and h.service.confirmed == ["purchase-B"]
            assert not old.future.done()
        finally:
            await h.close()

    asyncio.run(run())


def test_duplicate_talk_does_not_rebind_recording_to_a_new_question() -> None:
    async def run() -> None:
        h = Harness()
        try:
            h.question()
            await h.begin()
            new, purchase = await h.purchase()
            h.ws.command("talk")  # no new press; this frame arrives before the audio and done below
            await h.release(1)
            await h.finish(1)
            assert not new.future.done() and not purchase.done() and not h.service.confirmed
        finally:
            await h.close()

    asyncio.run(run())


def test_hangup_cancels_a_pending_transcript_without_approving_purchase() -> None:
    async def run() -> None:
        h = Harness()
        try:
            question, purchase = await h.purchase()
            await h.begin()
            await h.release(1)
            h.ws.command("end")
            assert await asyncio.wait_for(h.run, 3) == "you hung up"
            h.voice.release[1].set()  # upload worker cannot be stopped; its result must never enter hear
            await until(h.voice.finished[1].is_set)
            await asyncio.sleep(0)
            assert not question.future.done() and not purchase.done() and not h.service.confirmed
            assert h.call.presses == 0 and not h.call.tasks
        finally:
            await h.close()

    asyncio.run(run())


def test_stop_still_withdraws_purchase_when_question_changed_during_stt() -> None:
    async def run() -> None:
        h = Harness()
        try:
            h.question()
            h.voice.words[1] = "stop"
            await h.begin()
            await h.release(1)
            _new, purchase = await h.purchase()
            await h.finish(1)
            result = await asyncio.wait_for(purchase, 3)
            assert not result["ok"] and not h.service.confirmed
        finally:
            await h.close()

    asyncio.run(run())


def test_legacy_brain_keeps_one_argument_hear() -> None:
    async def run() -> None:
        brain, ws, voice = Brain(reply=""), Socket(), HeldVoice()
        call = phone_call.Call(ws, brain, voice)
        job = asyncio.create_task(call.run())
        try:
            ws.command("talk")
            ws.frames.put_nowait((phone_call.OP_BIN, b"\x01\x00" * phone_call.SAMPLE_RATE))
            ws.command("done")
            await until(voice.entered[1].is_set)
            voice.release[1].set()
            await until(lambda: brain.heard == ["yes"])
        finally:
            voice.unblock()
            ws.command("end")
            await asyncio.wait_for(job, 3)

    asyncio.run(run())
