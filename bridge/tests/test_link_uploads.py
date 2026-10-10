"""link_uploads.py and the attachment half of link_chat.py: POST /api/link/upload (auth, size cap, the types it
takes and refuses), the ids a turn may name (strict, no path climbing), the 24 h expiry, and that a turn with an
attachment reaches the brain as a Telegram photo does (vision input) or as the file (PDF, text), with the owner's
transcript line and /api/link/messages carrying the attachment.

The real Mini App server on a loopback port, a raw HTTP client, and the real TelegramInlet with test_telegram.py's
fakes (FakeCreate is the brain: it records each request)."""
from __future__ import annotations

import asyncio
import io
import json
import os
import stat
from pathlib import Path
from typing import Any, Optional

import pytest
from PIL import Image
from test_link_chat import FakeInlet, events, http, messages, real_rig, serve, store_at
from test_telegram import OWNER, say

from eve_bridge import link_chat, link_uploads
from eve_bridge.link_uploads import UploadError, UploadStore


def jpeg(w: int = 64, h: int = 48) -> bytes:
    out = io.BytesIO()
    Image.new("RGB", (w, h), (200, 30, 30)).save(out, "JPEG")
    return out.getvalue()


PDF = b"%PDF-1.4\n1 0 obj << >> endobj\ntrailer << >>\n%%EOF\n"


async def upload(port: int, token: Optional[str], data: bytes, name: str = "a.jpg", *,
                 origin: Optional[str] = None, length: Optional[int] = None) -> tuple[int, dict[str, Any]]:
    """POST /api/link/upload as the app's URLSession sends it: raw bytes, X-Filename percent-encoded."""
    from urllib.parse import quote

    reader, writer = await asyncio.open_connection("127.0.0.1", port)
    head = (f"POST {link_chat.UPLOAD_PATH} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/octet-stream\r\n"
            f"Content-Length: {len(data) if length is None else length}\r\nX-Filename: {quote(name)}\r\n")
    if token is not None:
        head += f"Authorization: Bearer {token}\r\n"
    if origin is not None:
        head += f"Origin: {origin}\r\n"
    writer.write(head.encode() + b"\r\n")
    with_body = length is None or length <= len(data)
    if with_body:
        writer.write(data)
    await writer.drain()
    raw = await asyncio.wait_for(reader.read(), 20)
    writer.close()
    top, _, rest = raw.partition(b"\r\n\r\n")
    return int(top.split()[1]), json.loads(rest or b"{}")


async def turn_with(port: int, token: str, text: str, ids: list[Any], client_id: str = "") -> tuple[int, bytes]:
    status, _, body = await http(port, "POST", link_chat.TURN_PATH,
                                 {"text": text, "client_id": client_id, "attachments": ids}, token=token)
    return status, body


# ---- the store ---------------------------------------------------------------------------------------------

def test_the_store_keeps_files_private_and_names_them_by_id_only(tmp_path: Path) -> None:
    store = UploadStore(tmp_path / "up")
    up = store.save(jpeg(), "../../etc/passwd\x00.jpg")
    assert link_uploads.ID_RE.match(up.id) and up.kind == "image" and up.mime == "image/jpeg"
    assert up.path.parent == (tmp_path / "up").resolve() and up.path.name == up.id + ".jpg"
    assert up.name == "passwd.jpg"                                     # shown only, never a path
    assert stat.S_IMODE((tmp_path / "up").stat().st_mode) == 0o700
    for p in (tmp_path / "up").iterdir():
        assert stat.S_IMODE(p.stat().st_mode) == 0o600
    assert store.get(up.id) == up
    assert store.thumb_url(up.id).startswith("data:image/jpeg;base64,")


def test_a_shared_or_foreign_folder_is_refused(tmp_path: Path) -> None:
    root = tmp_path / "up"
    root.mkdir(mode=0o755)
    os.chmod(root, 0o755)
    with pytest.raises(UploadError):
        UploadStore(root).save(jpeg(), "a.jpg")


@pytest.mark.parametrize("bad", ["", "abc", "../" + "0" * 29, "0" * 31 + "/", "0" * 32 + "x", "A" * 32,
                                 "0" * 32 + ".json", None, 7, ["0" * 32]])
def test_ids_are_32_lowercase_hex_and_nothing_else(tmp_path: Path, bad: Any) -> None:
    store = UploadStore(tmp_path / "up")
    store.save(jpeg(), "a.jpg")
    assert store.get(bad) is None


def test_an_id_never_stored_is_none_and_a_stray_file_in_the_folder_is_swept(tmp_path: Path) -> None:
    store = UploadStore(tmp_path / "up")
    store.save(PDF, "x.pdf")
    stray = tmp_path / "up" / "notes.txt"
    stray.write_text("x")
    assert store.get("0" * 32) is None
    store.sweep()
    assert not stray.exists()


def test_uploads_expire_after_a_day_and_are_deleted(tmp_path: Path) -> None:
    now = {"t": 1_000_000.0}
    store = UploadStore(tmp_path / "up", clock=lambda: now["t"])
    up = store.save(jpeg(), "a.jpg")
    for p in (tmp_path / "up").iterdir():
        os.utime(p, (now["t"], now["t"]))
    now["t"] += link_uploads.KEEP_SECS - 60
    assert store.get(up.id) is not None                                # positive control: still alive
    now["t"] += 120
    assert store.get(up.id) is None
    assert list((tmp_path / "up").iterdir()) == []


@pytest.mark.parametrize("data,name,kind", [
    (jpeg(), "x.jpg", "image"), (PDF, "doc.pdf", "pdf"), ("héllo, eve\n".encode(), "n.md", "text"),
    (b"a,b\n1,2\n", "t.csv", "text"), (b"PK\x03\x04rest", "r.docx", "doc"), (b"{\\rtf1 hi}", "r.rtf", "doc"),
])
def test_what_it_takes(data: bytes, name: str, kind: str) -> None:
    assert link_uploads.classify(data, name)[0] == kind


@pytest.mark.parametrize("data,name", [
    (b"MZ\x90\x00binary", "tool.exe"), (b"#!/bin/sh\nrm -rf ~\n", "run.sh"), (b"PK\x03\x04", "app.zip"),
    (b"\x00\x01\x02", "x.txt"), (b"\xff\xfe\xfa", "x.txt"), (b"\x00\x00\x00\x18ftypheic", "IMG.HEIC"),
    (b"\xff\xd8\xffnot really a jpeg", "x.jpg"), (b"plain words", "x.pdf"), (b"", "x.txt"),
])
def test_what_it_refuses(data: bytes, name: str) -> None:
    with pytest.raises(UploadError):
        link_uploads.classify(data, name)



@pytest.mark.parametrize("data,name,suffix", [
    (b'{"rent": 2400}\n', "budget.json", ".json"), (b"# Plan\n", "plan.markdown", ".md"),
    (b"a: 1\n", "c.yml", ".yml"), (b"# Plan\n", "plan.md", ".md"),
])
def test_every_text_type_it_takes_can_be_read_back_and_survives_a_sweep(tmp_path: Path, data: bytes, name: str,
                                                                        suffix: str) -> None:
    """Review, 2026-10-08: a .json upload collided with its own metadata file (<id>.json) and failed, and a
    .markdown one was kept under a suffix nothing could read back, then swept by the next upload."""
    store = UploadStore(tmp_path / "up")
    up = store.save(data, name)
    store.save(PDF, "other.pdf")                                         # a second upload sweeps the folder
    got = store.get(up.id)
    assert got is not None and got.kind == "text" and got.name == name and got.path.suffix == suffix
    assert got.path.read_bytes() == data
    assert store.sweep() == 2


def test_a_file_deleted_while_the_sweep_lists_the_folder_is_skipped(tmp_path: Path,
                                                                     monkeypatch: pytest.MonkeyPatch) -> None:
    """get() deletes an expired upload in another thread while save() sweeps: the sweep must not fail the upload."""
    store = UploadStore(tmp_path / "up")
    store.save(PDF, "a.pdf")
    real_iterdir = Path.iterdir

    def racing(self: Path):
        paths = list(real_iterdir(self))
        if self == store.root:
            paths[0].unlink()                                            # gone between the listing and the lstat
        return iter(paths)

    monkeypatch.setattr(Path, "iterdir", racing)
    assert store.sweep() in (0, 1)


# ---- the door ----------------------------------------------------------------------------------------------

def test_upload_needs_the_link_token_and_no_origin(tmp_path: Path) -> None:
    async def go() -> None:
        store = UploadStore(tmp_path / "up")
        server, port, token = await serve(tmp_path, FakeInlet(), uploads=store)
        assert (await upload(port, None, jpeg()))[0] == 401
        assert (await upload(port, "wrong-token-" + "x" * 30, jpeg()))[0] == 401
        assert (await upload(port, token, jpeg(), origin="https://evil.example"))[0] == 403
        status, got = await upload(port, token, jpeg(), "Beach day.jpg")       # positive control
        assert status == 200 and got["ok"] and got["kind"] == "image" and got["name"] == "Beach day.jpg"
        assert store.get(got["id"]) is not None
        status, _, _ = await http(port, "GET", link_chat.UPLOAD_PATH, token=token)
        assert status == 405
        await server.close()

    asyncio.run(go())


def test_upload_size_cap_and_type_refusal(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    async def go() -> None:
        store = UploadStore(tmp_path / "up")
        server, port, token = await serve(tmp_path, FakeInlet(), uploads=store)
        status, got = await upload(port, token, b"", length=link_uploads.MAX_BYTES + 1)
        assert status == 413 and not got["ok"]                          # refused on the header, before reading
        monkeypatch.setattr(link_uploads, "MAX_BYTES", 1000)
        status, _ = await upload(port, token, b"a" * 1001, "big.txt", length=1001)
        assert status == 413
        status, got = await upload(port, token, b"MZ\x90\x00" * 10, "tool.exe")
        assert status == 415 and "photos, PDFs" in got["error"]
        status, _ = await upload(port, token, b"", "empty.txt")
        assert status == 400
        status, _ = await upload(port, token, b"%PDF-1.4 small", "ok.pdf")   # positive control under the cap
        assert status == 200
        assert len([p for p in (tmp_path / "up").iterdir() if p.suffix == ".json"]) == 1
        await server.close()

    asyncio.run(go())


def test_a_turn_names_only_known_live_ids(tmp_path: Path) -> None:
    async def go() -> None:
        now = {"t": 2_000_000.0}
        store = UploadStore(tmp_path / "up", clock=lambda: now["t"])
        inlet = FakeInlet()
        server, port, token = await serve(tmp_path, inlet, uploads=store)
        good = store.save(jpeg(), "a.jpg")
        for bad in (["../../../etc/passwd"], ["0" * 32], [good.id] * 2, [good.id] * 5, "not-a-list", [7]):
            status, body = await turn_with(port, token, "look", bad)  # type: ignore[arg-type]
            assert status == 400, (bad, body)
        status, body = await turn_with(port, token, "", [])
        assert status == 400                                            # no words and nothing attached
        assert inlet.heard == []
        for p in (tmp_path / "up").iterdir():
            os.utime(p, (now["t"], now["t"]))
        now["t"] += link_uploads.KEEP_SECS + 1
        status, body = await turn_with(port, token, "look", [good.id])
        assert status == 400 and b"expired" in body
        assert inlet.heard == []
        await server.close()

    asyncio.run(go())


def test_attachments_reach_the_inlet_and_an_empty_text_is_fine_with_one(tmp_path: Path) -> None:
    async def go() -> None:
        store = UploadStore(tmp_path / "up")
        got: list[Any] = []

        class Inlet(FakeInlet):
            def hear(self, text: str, *, on_call: bool = True, app: Optional[str] = None, grade: str = "auto",
                     attachments: Any = ()) -> None:
                got.append((text, list(attachments)))
                super().hear(text, on_call=on_call, app=app, grade=grade)

        inlet = Inlet()
        server, port, token = await serve(tmp_path, inlet, uploads=store, settle_secs=0.01)
        up = store.save(PDF, "q.pdf")
        status, body = await turn_with(port, token, "", [up.id], "c-1")
        assert status == 200 and events(body)[1]
        assert got == [("", [up])]
        await server.close()

    asyncio.run(go())


def test_photos_and_pdfs_over_the_inline_cap_together_are_refused_before_the_brain(
        tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """They go to the model inside the request: past MAX_INLINE_BYTES the turn is a 413 with a line to show."""
    async def go() -> None:
        store = UploadStore(tmp_path / "up")
        got: list[Any] = []

        class Inlet(FakeInlet):
            def hear(self, text: str, *, on_call: bool = True, app: Optional[str] = None, grade: str = "auto",
                     attachments: Any = ()) -> None:
                got.append([u.name for u in attachments])
                super().hear(text, on_call=on_call, app=app, grade=grade)

        inlet = Inlet()
        server, port, token = await serve(tmp_path, inlet, uploads=store, settle_secs=0.01)
        a, b = store.save(PDF, "a.pdf"), store.save(jpeg(), "b.jpg")
        text = store.save(b"plain words\n" * 50, "notes.txt")
        monkeypatch.setattr(link_chat, "MAX_INLINE_BYTES", a.size + b.size - 1)
        status, body = await turn_with(port, token, "look", [a.id, b.id])
        assert status == 413 and b"send fewer at once" in body and got == []
        status, body = await turn_with(port, token, "look", [a.id, text.id], "c-ok")   # a text file is not inlined
        assert status == 200 and got == [["a.pdf", "notes.txt"]]
        await server.close()

    asyncio.run(go())


# ---- with the real chat: what the brain gets ---------------------------------------------------------------

def user_input(request: dict[str, Any]) -> list[dict[str, Any]]:
    """The content parts of the last user item in a model request."""
    items = [i for i in request["input"] if isinstance(i, dict) and i.get("role") == "user"]
    return items[-1]["content"]


def test_an_image_turn_reaches_the_brain_as_an_image_like_a_telegram_photo(tmp_path: Path) -> None:
    rig, memory = real_rig(tmp_path, say("A red square."))

    async def go() -> None:
        store = UploadStore(tmp_path / "up")
        server, port, token = await serve(tmp_path, rig.inlet, uploads=store, settle_secs=0.01)
        _, got = await upload(port, token, jpeg(), "red.jpg")
        status, body = await turn_with(port, token, "what is this?", [got["id"]], "c-img")
        assert status == 200 and events(body) == (["A red square."], True, 0)
        assert rig.api.sent == []                                       # answered in the app
        parts = user_input(rig.create.requests[0])
        assert parts[0] == {"type": "input_text", "text": "what is this?"}
        assert [p["type"] for p in parts[1:]] == ["input_image"]
        assert parts[1]["image_url"].startswith("data:image/jpeg;base64,")
        owner = [ln for ln in memory.said() if ln["who"] == "owner"]
        assert owner[0]["kind"] == "image" and owner[0]["text"] == "what is this?" and owner[0]["via"] == "app"
        assert owner[0]["att"] == [{"id": got["id"], "name": "red.jpg", "kind": "image"}]
        page = await messages(port, token)
        first = page["messages"][0]
        assert first["role"] == "owner" and first["attachments"][0]["name"] == "red.jpg"
        assert first["attachments"][0]["thumb"].startswith("data:image/jpeg;base64,")
        assert "attachments" not in page["messages"][1]
        await server.close()

    asyncio.run(go())


def test_a_pdf_and_a_text_file_reach_the_brain_as_the_file_and_its_words(tmp_path: Path) -> None:
    rig, memory = real_rig(tmp_path, say("Got both."))

    async def go() -> None:
        store = UploadStore(tmp_path / "up")
        server, port, token = await serve(tmp_path, rig.inlet, uploads=store, settle_secs=0.01)
        _, pdf = await upload(port, token, PDF, "lease.pdf")
        _, txt = await upload(port, token, b"milk\neggs\n", "list.txt")
        status, body = await turn_with(port, token, "", [pdf["id"], txt["id"]], "c-files")
        assert status == 200 and events(body)[0] == ["Got both."]
        parts = user_input(rig.create.requests[0])
        assert parts[0]["text"] == "Please look at this file."
        assert parts[1]["type"] == "input_file" and parts[1]["filename"] == "lease.pdf"
        assert parts[1]["file_data"].startswith("data:application/pdf;base64,")
        assert parts[2]["type"] == "input_text" and "milk\neggs" in parts[2]["text"]
        assert not any(p["type"] == "input_image" for p in parts)
        owner = [ln for ln in memory.said() if ln["who"] == "owner"][0]
        assert owner["kind"] == "say" and [a["kind"] for a in owner["att"]] == ["pdf", "text"]
        page = await messages(port, token)
        assert [a["name"] for a in page["messages"][0]["attachments"]] == ["lease.pdf", "list.txt"]
        assert all("thumb" not in a for a in page["messages"][0]["attachments"])
        await server.close()

    asyncio.run(go())


def test_an_expired_attachment_shows_as_gone_in_the_history(tmp_path: Path) -> None:
    async def go() -> None:
        now = {"t": 3_000_000.0}
        store = UploadStore(tmp_path / "up", clock=lambda: now["t"])
        transcripts = store_at(tmp_path)
        up = store.save(jpeg(), "old.jpg")
        transcripts.append("telegram", transcripts.new_conv("telegram"), "owner", "image", "look", via="app",
                           att=link_chat.att_record([up]))
        inlet = FakeInlet(transcripts)
        server, port, token = await serve(tmp_path, inlet, uploads=store)
        first = (await messages(port, token))["messages"][0]
        assert first["attachments"][0].get("thumb") and not first["attachments"][0].get("gone")
        for p in (tmp_path / "up").iterdir():
            os.utime(p, (now["t"], now["t"]))
        now["t"] += link_uploads.KEEP_SECS + 1
        first = (await messages(port, token))["messages"][0]
        assert first["attachments"] == [{"id": up.id, "name": "old.jpg", "kind": "image", "gone": True}]
        await server.close()

    asyncio.run(go())


def test_a_bad_att_on_a_transcript_line_is_left_out() -> None:
    raw = [{"id": "../x", "name": "a", "kind": "image"}, {"id": "0" * 32, "name": "b", "kind": "exe"},
           {"id": "1" * 32, "name": "ok.pdf", "kind": "pdf"}, "junk"]
    assert link_chat._att(raw) == [{"id": "1" * 32, "name": "ok.pdf", "kind": "pdf"}]
    assert link_chat._att("nope") == []


def test_with_the_claude_relay_on_an_app_image_is_saved_and_its_path_typed(tmp_path: Path,
                                                                           monkeypatch: pytest.MonkeyPatch) -> None:
    rig, _ = real_rig(tmp_path)
    typed: list[str] = []

    async def fake_type(chat_id: int, prompt: str, message_id: int = 0) -> bool:
        typed.append(prompt)
        return True

    async def go() -> None:
        store = UploadStore(tmp_path / "up")
        up = store.save(jpeg(), "screen.jpg")
        doc = store.save(b"PK\x03\x04data", "plan.docx")
        saved_root = tmp_path / "tg"
        real_save = link_uploads.telegram_images.save
        monkeypatch.setattr(link_uploads.telegram_images, "save", lambda im, root=None: real_save(im, saved_root))
        rig.inlet.claude = True
        monkeypatch.setattr(rig.inlet, "_type_to_claude", fake_type)
        rig.inlet.hear("fix this", on_call=False, app="t-1", attachments=[up, doc])
        for _ in range(200):
            await asyncio.sleep(0.01)
            if typed:
                break
        assert len(typed) == 1
        assert "Image received from the owner through Eve Link, saved on this Mac: " + str(saved_root) in typed[0]
        assert str(doc.path) in typed[0] and typed[0].startswith("fix this")

    asyncio.run(go())


def test_an_attachment_is_not_taken_as_the_answer_to_a_waiting_question(tmp_path: Path) -> None:
    rig, _ = real_rig(tmp_path)

    async def go() -> None:
        store = UploadStore(tmp_path / "up")
        up = store.save(jpeg(), "a.jpg")
        rig.inlet._image_waits = lambda: True                        # a question waits on a typed answer
        rig.inlet.hear("yes", on_call=False, attachments=[up])
        for _ in range(100):
            await asyncio.sleep(0.01)
            if rig.api.sent:
                break
        assert rig.create.requests == []
        assert rig.api.sent and "pending question" in rig.api.sent[0][1]
        assert rig.api.sent[0][0] == OWNER

    asyncio.run(go())


def test_an_owner_lines_attachment_note_is_not_sent_to_the_app() -> None:
    """Owner, 2026-10-09: "I see two messages sent on my end". The relay kept "[image attached]" in the line; the
    app matched its own bubble by the words alone, missed, and showed the photo twice."""
    from eve_bridge import link_chat

    att = [{"id": "a" * 32, "name": "IMG.jpg", "kind": "image"}]
    base = {"ts": "2026-10-09T08:41:24-07:00", "ch": "telegram", "who": "owner", "via": "app", "att": att}
    for said, sent in (("Why is memory so ass [image attached]", "Why is memory so ass"),
                       ("[image attached]", ""),
                       ("look [2 images, file notes.pdf attached]", "look")):
        row = link_chat._row("2026-10-09", 0, {**base, "kind": "relay", "text": said})
        assert row is not None and row["text"] == sent and row["attachments"]
    # words that only look like it stay: with no attachment, and in eve's lines
    plain = link_chat._row("2026-10-09", 1, {**base, "kind": "say", "att": None, "text": "ok [image attached]"})
    assert plain is not None and plain["text"] == "ok [image attached]"
    mid = link_chat._row("2026-10-09", 2, {**base, "kind": "say", "text": "[image attached] is what it said"})
    assert mid is not None and mid["text"] == "[image attached] is what it said"
