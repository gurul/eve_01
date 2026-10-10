"""Replay of verification/Eve/ShareGate.lean's ``current_violates`` against the real share.py.

The owner-path rules (``resolve_owner_path``) run on the path *resolved* at one moment; the bytes are read
(``fingerprint`` at the question, ``stage_exact`` at the send) by opening that real path again a moment later.
``O_NOFOLLOW`` refuses a link only in the LAST component, so a folder on the way that became a link between the
two reads is followed. The model's trace: the path resolves to ``~/Documents/inbox/notes.txt`` (allowed), then
``inbox`` is a link to ``~/.ssh``; the fingerprint, the question, the owner's Allow and the staged copy are all of
``~/.ssh/notes.txt``, a file the rules forbid. The size, inode and SHA-256 match at the send because they were
taken of the hidden file in the first place.

The environment's move between the resolve and the open is injected by wrapping ``share.resolve_file``: the real
function runs on the folder as a folder, and the folder becomes a link right after it returns (a process of the
owner's, a task, or a sync client acting between two awaits). Nothing else is faked but the osascript runner.

This test FAILS on share.py as of a35b043 and passes once each open walks the real path component by component
with ``O_NOFOLLOW`` (or checks the descriptor's own path), the model's ``Fix``.
"""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any

from eve_bridge import share as S

ON = S.ShareConfig(enabled=True, imessage=True)
SECRET = b"-----BEGIN OPENSSH PRIVATE KEY----- not really\n"


class Runner:
    def __init__(self) -> None:
        self.sent: list[bytes] = []

    async def __call__(self, argv: list[str], stdin: bytes, timeout: float) -> tuple[int, str, str]:
        if "JavaScript" in argv:
            return 0, json.dumps({"count": 0, "people": []}), ""
        self.sent.append(Path(argv[3]).read_bytes())      # what Messages would read: the staged copy
        return 0, "", ""


def test_a_folder_swapped_for_a_link_between_resolve_and_open_never_sends_a_hidden_file(
        tmp_path: Path, monkeypatch: Any) -> None:
    home = (tmp_path / "home").resolve()
    inbox = home / "Documents" / "inbox"
    inbox.mkdir(parents=True)
    (inbox / "notes.txt").write_bytes(b"shopping list\n")
    (home / ".ssh").mkdir()
    (home / ".ssh" / "notes.txt").write_bytes(SECRET)
    parked = home / "Documents" / ".inbox-parked"

    def as_folder() -> None:
        if inbox.is_symlink():
            inbox.unlink()
            parked.rename(inbox)

    def as_link() -> None:
        if not inbox.is_symlink():
            inbox.rename(parked)
            inbox.symlink_to(home / ".ssh")

    real_resolve = S.resolve_file

    def racing_resolve(raw: str, h: Any = None) -> Any:
        as_folder()
        out = real_resolve(raw, h)
        as_link()                                         # the environment moves between resolve and open
        return out

    monkeypatch.setattr(S, "resolve_file", racing_resolve)
    runner = Runner()
    sh = S.Share(ON, runner=runner, home=home, outbox=home / "Pictures" / "eve-outbox")

    async def allow(question: str) -> str:
        return "yes"                                      # a bare Allow on the question shown

    out = asyncio.run(sh.ask_and_share({"path": "~/Documents/inbox/notes.txt", "to": "+1 555 010 0199",
                                        "via": "imessage", "message": None}, allow))
    assert not any(SECRET in b for b in runner.sent), f"a file under ~/.ssh was sent: {out}"


# ---- slot_violates: the gate reads a slot that lags behind the questions ------------------------------------

def test_a_share_never_asks_while_an_older_question_still_waits(tmp_path: Path) -> None:
    """Replay of ShareGate.lean ``slot_violates`` against the real TelegramInlet and Share.

    ``_share_tool`` and ``_ask_alone`` refuse while ``_awaiting_answer`` holds, and that property reads only the
    slot (``_pending_answer``). When the newest question settles, the slot keeps naming its (now done) future
    until the done callback ``_show_question`` runs on a later turn of the loop; in between ``_awaiting_answer``
    is False although an older question is still waiting. The share's prepare finishing in that same tick asks
    the share question on top of the older one, which it then hides: the owner's "yes", meant for the task's
    "Reply yes or no.", is the newest question's answer and sends the file (P3).

    The only control of timing is the order of two calls in one synchronous block: Contacts answers the share's
    lookup, and the owner answers the newer prompt (the line ``_on_tap`` / ``_handle`` run). Fails at a35b043;
    passes once the two gates ask whether ANY question is live (``self._live_question() is not None``)."""
    import sys
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from test_telegram import CFG, OWNER, FakeApi, FakeCreate

    from eve_bridge.telegram import ALLOW_DENY, Inbound, TelegramInlet

    home = (tmp_path / "home").resolve()
    (home / "Documents").mkdir(parents=True)
    (home / "Documents" / "report.txt").write_bytes(b"the report\n")
    contacts = asyncio.Event()
    sent: list[bytes] = []

    async def runner(argv: list[str], stdin: bytes, timeout: float) -> tuple[int, str, str]:
        if "JavaScript" in argv:
            await contacts.wait()
            return 0, json.dumps({"count": 1, "people": [{"name": "Ana", "emails": [], "phones": [
                {"label": "mobile", "value": "+15550100199"}]}]}), ""
        sent.append(Path(argv[3]).read_bytes())
        return 0, "", ""

    sh = S.Share(ON, runner=runner, home=home, outbox=home / "Pictures" / "eve-outbox")

    async def go() -> dict[str, Any]:
        inlet = TelegramInlet(CFG, FakeApi(), FakeCreate(), share=sh)

        async def settle() -> None:
            for _ in range(200):
                await asyncio.sleep(0)

        share_call = asyncio.create_task(inlet._share_tool(
            "share_file", {"path": "~/Documents/report.txt", "to": "Ana", "via": "imessage", "message": None},
            OWNER))
        await settle()                                    # no question waits: the share is looking Ana up
        task_q = asyncio.create_task(inlet._ask_user("Shall I go on? Reply yes or no.", OWNER))
        await settle()
        prompt = asyncio.create_task(inlet._ask_user("Allow rm?", OWNER, choices=ALLOW_DENY, firm=True))
        await settle()
        contacts.set()                                    # Contacts answers ...
        inlet._questions[-1].future.set_result("no")      # ... in the tick the owner answers the newer prompt
        await settle()
        inlet._handle(Inbound(chat_id=OWNER, user_id=OWNER, text="yes"))   # the owner's yes, for the task
        await settle()
        for t in (task_q, prompt):
            t.cancel()
        await asyncio.gather(task_q, prompt, return_exceptions=True)
        return share_call.result() if share_call.done() else {"pending": True}

    out = asyncio.run(go())
    assert not sent, f"a share was asked and sent while the task's question waited: {out}"
