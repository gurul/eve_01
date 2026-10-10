"""Owner approval for AgentCard's single-use purchase confirmations.

The client owns cart/address binding, durable attempts and provider reconciliation.
This gateway owns the question's lifecycle and serializes confirmations. Stopping a
worker cannot undo a request that has already reached the provider.
"""

from __future__ import annotations

import asyncio
import copy
import re
import threading
from dataclasses import dataclass, field
from typing import Any, Awaitable, Callable, Protocol

from . import consent

CONFIRM_TOOL = "agentcard_confirm"
NONCONFIRM_TOOLS = frozenset({"agentcard_merchants", "agentcard_buy", "agentcard_status"})
_CART_HASH = re.compile(r"[0-9a-f]{16}\Z")
AskUser = Callable[[str, str], Awaitable[str]]


class Service(Protocol):
    def handle(self, name: str, args: dict[str, Any]) -> dict[str, Any]: ...

    def prepare_confirm(self, conversation_id: str, cart_hashes: list[str]) -> dict[str, Any]: ...

    def confirm(self, snapshot: Any, cancelled: threading.Event | None = None) -> dict[str, Any]: ...


@dataclass(eq=False)
class _Confirmation:
    chat_id: int
    cancelled: threading.Event = field(default_factory=threading.Event)
    phase: str = "queued"
    asking: asyncio.Task[str] | None = None
    task: asyncio.Task[dict[str, Any]] | None = None


def _not_submitted(reason: str) -> dict[str, Any]:
    return {"ok": False, "status": "not_submitted", "reason": reason}


class AgentCardTools:
    """One confirmation at a time, from preparation until its worker returns.

    ``ask_user(question, about)`` must be bound to the owner's firm yes/no UI by
    the inlet. This gateway independently requires a whole bare approval. Public
    methods are called on one asyncio loop; cancellation events cross to workers.
    """

    def __init__(self, service: Service) -> None:
        self.service = service
        self._lock = asyncio.Lock()
        self._requests: set[_Confirmation] = set()
        self._closed = False

    def active(self, chat_id: int | None = None) -> bool:
        return any(chat_id is None or r.chat_id == chat_id for r in self._requests)

    def stop(self, chat_id: int | None = None) -> dict[str, Any]:
        """Withdraw matching pending approvals; signal workers before dispatch.

        Confirmation workers are retained until they finish. ``in_flight`` is
        conservative: the worker may already have submitted, even if it has not
        yet produced a receipt. It does not mean the purchase was cancelled.
        """
        stopped = 0
        in_flight = False
        for request in tuple(self._requests):
            if chat_id is not None and request.chat_id != chat_id:
                continue
            if request.phase == "confirming":
                in_flight = True
            elif not request.cancelled.is_set():
                stopped += 1
            request.cancelled.set()
            if request.asking is not None and not request.asking.done():
                request.asking.cancel()
        if in_flight:
            line = ("A purchase request may already be in flight. Stopping cannot undo it; "
                    "check its AgentCard status before trying again.")
            if stopped:
                line = "Stopped pending purchase confirmations. " + line
        elif stopped:
            line = "Stopped. No pending purchase confirmation will be submitted."
        else:
            line = "No purchase confirmation is waiting."
        return {"stopped": stopped, "in_flight": in_flight, "line": line}

    def shutdown(self) -> dict[str, Any]:
        self._closed = True
        return self.stop()

    async def wait_idle(self) -> None:
        """Observe outstanding workers without cancelling a submitted request."""
        tasks = [r.task for r in tuple(self._requests) if r.task is not None]
        if tasks:
            await asyncio.shield(asyncio.gather(*tasks, return_exceptions=True))

    async def handle(self, name: str, args: dict[str, Any], chat_id: int,
                     ask_user: AskUser) -> dict[str, Any]:
        if self._closed:
            return _not_submitted("AgentCard is shutting down; no new request was submitted.")
        if not isinstance(args, dict):
            return _not_submitted("AgentCard arguments must be an object.")
        if name in NONCONFIRM_TOOLS:
            try:
                result = await asyncio.to_thread(self.service.handle, name, copy.deepcopy(args))
                if not isinstance(result, dict):
                    return {"ok": False, "reason": "AgentCard did not return a usable result."}
                return result
            except asyncio.CancelledError:
                raise
            except Exception:  # noqa: BLE001 — errors can contain private provider bodies
                return {"ok": False, "reason": "AgentCard did not return a usable result."}
        if name != CONFIRM_TOOL:
            return _not_submitted("Unknown AgentCard tool.")
        conversation = args.get("conversation_id")
        hashes = args.get("cart_hashes")
        if (set(args) != {"conversation_id", "cart_hashes"}
                or not isinstance(conversation, str) or not conversation.strip()
                or not isinstance(hashes, list) or not 1 <= len(hashes) <= 8
                or any(not isinstance(h, str) or not _CART_HASH.fullmatch(h) for h in hashes)
                or len(set(hashes)) != len(hashes)):
            return _not_submitted("Name the conversation and one to eight distinct exact cart hashes.")
        if not isinstance(chat_id, int) or isinstance(chat_id, bool):
            return _not_submitted("An owner chat is required for purchase approval.")
        # Freeze the model's list before any await or worker handoff. The opaque
        # client snapshot later binds the cart, address and durable state version.
        selected = list(hashes)
        request = _Confirmation(chat_id)
        self._requests.add(request)
        request.task = asyncio.create_task(self._confirm(request, conversation, selected, ask_user))
        try:
            return await asyncio.shield(request.task)
        except asyncio.CancelledError:
            request.cancelled.set()
            if request.asking is not None and not request.asking.done():
                request.asking.cancel()
            # The lifecycle task retains the serialization lock and observes its
            # worker. Otherwise cancellation could admit another confirmation
            # while the first thread is still preparing or sending a purchase.
            raise

    async def _confirm(self, request: _Confirmation, conversation: str, hashes: list[str],
                       ask_user: AskUser) -> dict[str, Any]:
        try:
            async with self._lock:
                if request.cancelled.is_set():
                    return _not_submitted("Purchase confirmation was stopped before submission.")
                request.phase = "preparing"
                prepared = await asyncio.to_thread(self.service.prepare_confirm, conversation, hashes)
                if request.cancelled.is_set():
                    return _not_submitted("Purchase confirmation was stopped before submission.")
                if not isinstance(prepared, dict):
                    return _not_submitted("AgentCard could not prepare a purchase confirmation.")
                if prepared.get("ok") is not True:
                    return prepared
                snapshot = prepared.get("snapshot")
                question, about = prepared.get("question"), prepared.get("about")
                if snapshot is None or not isinstance(question, str) or not question.strip() or not isinstance(about, str):
                    return _not_submitted("AgentCard could not prepare a purchase confirmation.")
                request.phase = "asking"
                request.asking = asyncio.create_task(ask_user(question, about))
                try:
                    answer = await request.asking
                except asyncio.CancelledError:
                    if not request.cancelled.is_set():
                        raise
                    return _not_submitted("Purchase confirmation was stopped before submission.")
                except TimeoutError:
                    return _not_submitted("The owner did not approve in time; no purchase was submitted.")
                if request.cancelled.is_set():
                    return _not_submitted("Purchase confirmation was stopped before submission.")
                if not isinstance(answer, str) or consent.bare_decision(answer) != "allow":
                    return _not_submitted("The owner did not approve; do not retry this purchase automatically.")
                request.phase = "confirming"
                result = await asyncio.to_thread(self.service.confirm, snapshot, cancelled=request.cancelled)
                if not isinstance(result, dict):
                    return {"ok": False, "status": "unknown", "reason": "The purchase outcome is unknown; "
                            "check AgentCard status before attempting another confirmation."}
                return result
        except asyncio.CancelledError:
            request.cancelled.set()
            if request.asking is not None and not request.asking.done():
                request.asking.cancel()
            raise
        except Exception:  # noqa: BLE001 — never log private response or exception text
            if request.phase == "confirming":
                return {"ok": False, "status": "unknown", "reason": "The purchase outcome is unknown; "
                        "check AgentCard status before attempting another confirmation."}
            return _not_submitted("AgentCard could not obtain owner approval; no confirmation was submitted.")
        finally:
            self._requests.discard(request)
