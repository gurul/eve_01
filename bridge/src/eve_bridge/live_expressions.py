"""Owner-enabled eye cues from conversation text. One worker, one pending event, no sound or motion.

The eye is picked by Jev (``EVE_EXPRESSION_BACKEND=jev``, the default since 2026-09-24) or by the
original local Laya checkpoint (``laya``; the trained heads did worse on fresh scenarios). Top-choice
selection here is an explicit policy, NOT a claim that a confidence gate passed. Failures emit nothing;
the board expires cues.
"""

from __future__ import annotations

import asyncio
import json
import logging
import math
import os
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from .eye_model import LABELS, WARM_STATE, ConversationContext, JevEyeModel, LiveEyeModel

log = logging.getLogger(__name__)

DEFAULT_MODEL = Path("~/.config/eve-bridge/models/laya-multilingual-mlx").expanduser()
BACKENDS = ("jev", "laya")
DEFAULT_BACKEND = "jev"


def backend_from_env(env=None) -> str:
    """``jev`` or ``laya``; anything else (or nothing) is the default. One reader, so a typo is loud once."""
    value = ((os.environ if env is None else env).get("EVE_EXPRESSION_BACKEND") or "").strip().lower()
    if value and value not in BACKENDS:
        log.warning("expressions: EVE_EXPRESSION_BACKEND=%r is not one of %s; using %s", value[:20], BACKENDS,
                    DEFAULT_BACKEND)
    return value if value in BACKENDS else DEFAULT_BACKEND


class LiveExpressions:
    def __init__(
        self,
        send,
        *,
        connected=lambda: True,
        muted=lambda: False,
        phase=lambda: "idle",
        path=None,
        model_factory=None,
        clock=time.monotonic,
    ):
        self.send, self.connected, self.muted, self.phase = send, connected, muted, phase
        self.clock = clock
        self.path = Path(
            path or os.environ.get("EVE_EXPRESSIONS_FILE", "~/.config/eve-bridge/expressions.json")
        ).expanduser()
        self.enabled = False
        try:
            self.enabled = json.loads(self.path.read_text()).get("enabled") is True
        except (OSError, ValueError, AttributeError):
            pass
        self.backend = backend_from_env()
        self.model_path = Path(os.environ.get("EVE_EXPRESSION_MODEL", str(DEFAULT_MODEL))).expanduser()
        self.factory = model_factory or self._load
        self.model = None
        self.ready = False
        self.error = ""
        self.pending = None
        self.seq = max(1, int(time.time() * 1000) & 0xFFFFFFFF)  # survives host restarts
        self.generation = 0
        self.last_text = None
        self.context = ConversationContext()
        self.last_sent_at = -math.inf
        self.last = None
        self.board = None
        self.board_history = []
        self.sent = self.dropped = 0
        self.wake = asyncio.Event()
        self.executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="expression")
        self.closed = False

    def _load(self):
        model = LiveEyeModel(self.model_path) if self.backend == "laya" else JevEyeModel()
        model.predict(WARM_STATE)                          # ready means a real answer came back
        return model

    def _next_id(self):
        self.seq = (self.seq + 1) & 0xFFFFFFFF or 1
        return self.seq

    def offer(self, who: str, text: str) -> int | None:
        if (
            not self.enabled
            or self.closed
            or who not in ("user", "assistant", "diary", "demo")
            or not isinstance(text, str)
        ):
            return None
        text = " ".join(text.split())[-800:]
        if not text or self.last_text == (who, text):
            return None
        self.last_text = (who, text)
        if self.pending is not None:
            self.dropped += 1
        event = self._next_id()
        self.generation += 1
        self.pending = (
            event,
            who,
            self.context.state(who, text, self.clock()),
            self.clock(),
            self.generation,
        )
        self.wake.set()
        return event

    async def set_enabled(self, on: bool):
        self.enabled = bool(on)
        self.generation += 1
        self.pending = None
        self.last_text = None
        self.context = ConversationContext()
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".tmp")
        tmp.write_text(json.dumps({"enabled": self.enabled}) + "\n")
        tmp.replace(self.path)
        self.wake.set()
        if not on and self.connected():
            await self.send({"cmd": "expression", "id": self._next_id(), "clear": True})

    def observe(self, event: dict):
        if not isinstance(event.get("id"), int):
            return
        self.board = dict(event)
        self.board_history.append(dict(event))
        del self.board_history[:-24]

    def status(self):
        return {
            "enabled": self.enabled,
            "ready": self.ready,
            "backend": self.backend,
            "model": getattr(self.model, "model", "") if self.backend == "jev" else str(self.model_path),
            "policy": "experimental top choice; not calibrated",
            "error": self.error,
            "sent": self.sent,
            "dropped": self.dropped,
            "last": self.last,
            "board": self.board,
            "board_history": list(self.board_history),
            "phase": self.phase(),
        }

    @staticmethod
    def label(answer):
        values = answer.get("probabilities") if isinstance(answer, dict) else None
        if (
            not isinstance(values, list)
            or len(values) != len(LABELS)
            or any(
                isinstance(x, bool)
                or not isinstance(x, (float, int))
                or not math.isfinite(x)
                or not 0 <= x <= 1
                for x in values
            )
            or abs(sum(values) - 1) > 1e-4
        ):
            raise ValueError("invalid expression probabilities")
        selected = LABELS[max(range(len(values)), key=values.__getitem__)]
        return selected, max(values)

    async def run(self):
        loop = asyncio.get_running_loop()
        try:
            while not self.closed:
                if self.enabled and self.model is None:
                    try:
                        self.model = await loop.run_in_executor(self.executor, self.factory)
                        self.ready, self.error = True, ""
                        log.info("expressions: %s ready (top-choice policy)",
                                 "local Laya" if self.backend == "laya" else f"Jev ({getattr(self.model, 'model', '?')})")
                    except Exception as exc:  # optional worker must never crash the daemon
                        self.ready, self.error = False, f"{type(exc).__name__}: {exc}"[:180]
                        log.warning("expressions: %s", self.error)
                        await asyncio.sleep(10)
                        continue
                if not self.enabled or self.pending is None:
                    self.wake.clear()
                    await self.wake.wait()
                    continue
                delay = 1.2 - (self.clock() - self.last_sent_at)
                if delay > 0:
                    await asyncio.sleep(delay)
                    continue  # take the newest pending event after the delay
                item, self.pending = self.pending, None
                event, who, text, offered, generation = item
                if self.clock() - offered > 4 or not self.connected():
                    self.dropped += 1
                    continue
                state = text
                try:
                    answer = await loop.run_in_executor(self.executor, self.model.predict, state)
                    label, p_top = self.label(answer)
                    age = self.clock() - offered
                    if not self.enabled or generation != self.generation or age > 4 or not self.connected():
                        self.dropped += 1
                        continue
                    ttl = max(500, min(4000, int((4 - age) * 1000)))
                    cmd = {"cmd": "expression", "id": event, "label": label, "ttl_ms": ttl, "chirp": False}
                    if await self.send(cmd) is False:
                        raise RuntimeError("board send failed")
                    self.last_sent_at = self.clock()
                    self.sent += 1
                    self.last = {
                        "id": event,
                        "source": who,
                        "label": label,
                        "p_top": p_top,
                        "model_ms": answer.get("ms"),
                        "event_to_send_ms": (self.clock() - offered) * 1000,
                        "chirp_requested": False,
                    }
                    self.error = ""
                    log.info(
                        "expressions: id=%s source=%s label=%s model_ms=%s",
                        event,
                        who,
                        label,
                        answer.get("ms"),
                    )
                except Exception as exc:
                    self.error = f"{type(exc).__name__}: {exc}"[:180]
                    self.dropped += 1
                    log.warning("expressions: %s", self.error)
        finally:
            self.closed = True
            self.executor.shutdown(wait=False, cancel_futures=True)
