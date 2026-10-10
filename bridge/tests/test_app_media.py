"""eve's own pictures and files in the Eve Link app (owner, 2026-10-09: "It can't send attachments" / "Like
images" / "Fix"): every photo, screenshot, file and app picture goes through TelegramInlet._send_media, which keeps a
copy in the link door's upload store and writes eve's ``media`` transcript line, so /api/link/messages returns it as
eve's message with attachments, whether Telegram is on (FakeApi) or app-only (MutedBotApi). And
``GET /api/link/file/<id>``: the link token only, 404 for anything that is not a live upload's id.

The real Mini App server on a loopback port and the real TelegramInlet with test_telegram.py's fakes."""
from __future__ import annotations

import ast
import asyncio
import inspect
import io
import os
import re
import time
from pathlib import Path
from typing import Any, Optional

import pytest
from PIL import Image
from test_link_chat import FakeInlet, events, http, messages, serve, store_at, turn
from test_telegram import OWNER, FakeApi, FakeCreate, FakeMemory, Rig, call, say

from eve_bridge import chat_log, link_chat, link_uploads, telegram, transcripts
from eve_bridge.link_uploads import UploadStore
from eve_bridge.telegram import BotApiError, MutedBotApi


def jpeg(w: int = 64, h: int = 48) -> bytes:
    out = io.BytesIO()
    Image.new("RGB", (w, h), (20, 120, 200)).save(out, "JPEG")
    return out.getvalue()


PDF = b"%PDF-1.4\n1 0 obj << >> endobj\ntrailer << >>\n%%EOF\n"


class Api(FakeApi):
    """FakeApi with the document and app-picture sends, recorded; ``fail`` makes every media send raise."""

    def __init__(self, *batches: Any) -> None:
        super().__init__(*batches)
        self.documents: list[tuple[int, str, str]] = []
        self.app_photos: list[tuple[int, int, str, list[tuple[str, str]]]] = []
        self.fail: Optional[Exception] = None

    async def send_photo(self, chat_id: int, path: Path, caption: str = "") -> None:
        if self.fail is not None:
            raise self.fail
        assert Path(path).exists(), "the file must still be there when Telegram gets it"
        await super().send_photo(chat_id, path, caption)

    async def send_document(self, chat_id: int, path: Path, caption: str = "") -> None:
        if self.fail is not None:
            raise self.fail
        self.documents.append((chat_id, str(path), caption))

    async def send_photo_web_apps(self, chat_id: int, jpeg: bytes, caption: str, buttons: Any) -> None:
        if self.fail is not None:
            raise self.fail
        self.app_photos.append((chat_id, len(jpeg), caption, list(buttons)))


class Notifier:
    def __init__(self) -> None:
        self.offered: list[tuple[str, dict[str, Any]]] = []

    def offer_send(self, method: str, data: dict[str, Any]) -> None:
        self.offered.append((method, data))

    async def close(self) -> None:
        pass


def rig_with(tmp_path: Path, api: Any, *responses: Any, store: bool = True, **kw: Any) -> tuple[Rig, FakeMemory]:
    memory = FakeMemory(tmp_path / "m")
    rig = Rig(api, FakeCreate(*responses), memory=memory, **kw)
    rig.inlet._chat_id = OWNER
    if store:
        rig.inlet.link_uploads = UploadStore(tmp_path / "up")
    return rig, memory


def media_lines(memory: FakeMemory) -> list[dict[str, Any]]:
    return [ln for ln in memory.said() if ln["kind"] == "media"]


async def wait_for(cond: Any, rounds: int = 400) -> None:
    for _ in range(rounds):
        if cond():
            return
        await asyncio.sleep(0.01)
    raise AssertionError("never happened")


def camera(tmp_path: Path, caption: str = "A blue square on the desk.") -> Any:
    picture = tmp_path / "001421-0.jpg"
    picture.write_bytes(jpeg())

    async def on_photo(note: str) -> dict[str, Any]:
        return {"ok": True, "path": str(picture), "caption": caption}

    return on_photo


# ---- take_photo, Telegram on and app-only ------------------------------------------------------------------

@pytest.mark.parametrize("muted", [False, True])
def test_take_photo_reaches_the_app_as_eves_message_with_telegram_on_or_muted(tmp_path: Path, muted: bool) -> None:
    notifier = Notifier()
    api: Any = MutedBotApi(notifier=notifier) if muted else Api()
    rig, memory = rig_with(tmp_path, api, call("take_photo", {"note": "desk"}), say("Here's your desk."),
                           on_photo=camera(tmp_path))

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, uploads=rig.inlet.link_uploads, settle_secs=0.01)
        rig.inlet.hear("send me a photo of my desk", on_call=False)          # typed on the Mac: not an app turn
        await wait_for(lambda: any(ln["who"] == "eve" and ln["kind"] == "say" for ln in memory.said()))
        # (1) the Bot API is still called: Telegram's photo, or the notifier's "eve sent a picture"
        if muted:
            assert [m for m, _ in notifier.offered if m == "sendPhoto"] == ["sendPhoto"]
        else:
            assert api.photos == [(OWNER, str(tmp_path / "001421-0.jpg"), "A blue square on the desk.")]
        # (2) the app's copy: eve's media line with the attachment, then the reply, in that order
        [line] = media_lines(memory)
        assert line["who"] == "eve" and line["text"] == "A blue square on the desk."
        [att] = line["att"]
        assert att["kind"] == "image" and att["name"] == "001421-0.jpg"
        page = await messages(port, token)
        rows = [(m["role"], m["kind"], m["text"]) for m in page["messages"] if m["kind"] != "tool"]
        assert rows == [("owner", "say", "send me a photo of my desk"),
                        ("eve", "media", "A blue square on the desk."),
                        ("eve", "say", "Here's your desk.")]
        shown = page["messages"][1]["attachments"][0]
        assert shown["id"] == att["id"] and shown["thumb"].startswith("data:image/jpeg;base64,")
        assert "attachments" not in page["messages"][2]
        # full size, the very bytes the camera wrote
        status, headers, body = await http(port, "GET", f"/api/link/file/{att['id']}", token=token)
        assert status == 200 and headers["content-type"] == "image/jpeg" and body == jpeg()
        assert headers["cache-control"] == "no-store" and int(headers["content-length"]) == len(body)
        # the model never reads it: not in the RAM history, and the kind is in no prompt block, seed or index
        assert all(text != "A blue square on the desk." for _, text in rig.inlet.turns)
        assert "media" not in transcripts.RENDER_KINDS and "media" not in chat_log.SEED_KINDS
        assert "A blue square on the desk." not in memory.transcripts.today_block(10_000).replace(
            '"shows": "A blue square on the desk."', "")
        await server.close()

    asyncio.run(go())


def test_an_app_turns_photo_is_one_line_in_order_and_not_in_the_stream(tmp_path: Path) -> None:
    """The live case (00:14, 2026-10-09): the app turn ran take_photo and the owner saw nothing. The stream carries the
    reply's words only; the picture is the poll's line, before the reply, and never twice."""
    notifier = Notifier()
    rig, memory = rig_with(tmp_path, MutedBotApi(notifier=notifier), call("take_photo", {"note": ""}),
                           say("Here it is."), on_photo=camera(tmp_path, "A desk."))

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, uploads=rig.inlet.link_uploads, settle_secs=0.01)
        status, _, body = await turn(port, token, "take a photo", "c-photo")
        assert status == 200 and events(body) == (["Here it is."], True, 0)
        page = await messages(port, token)
        rows = [(m["role"], m["kind"], m["via"], bool(m.get("attachments"))) for m in page["messages"] if m["kind"] != "tool"]
        assert rows == [("owner", "say", "app", False), ("eve", "media", "app", True), ("eve", "say", "app", False)]
        assert notifier.offered == []          # on the owner's screen already: no ntfy push for it
        await server.close()

    asyncio.run(go())


# ---- screenshots, files, documents, app pictures -------------------------------------------------------------

def test_a_screenshot_is_kept_for_the_app_before_its_file_is_deleted(tmp_path: Path) -> None:
    shot = tmp_path / "screen-1.png"
    Image.new("RGB", (80, 50), (0, 0, 0)).save(shot, "PNG")
    api = Api()
    rig, memory = rig_with(tmp_path, api, screen=lambda: shot)

    async def go() -> None:
        result = await rig.inlet._send_screen(OWNER, "the screen now")
        assert result == {"ok": True, "sent": True}
        assert not shot.exists()                                        # deleted after sending, as before
        [line] = media_lines(memory)
        assert line["text"] == "the screen now" and line["att"][0]["kind"] == "image"
        assert line["att"][0]["name"] == "Screen.png"
        kept = rig.inlet.link_uploads.get(line["att"][0]["id"])
        assert kept is not None and kept.path.read_bytes()[:8] == b"\x89PNG\r\n\x1a\n"
        assert len(api.photos) == 1

    asyncio.run(go())


def test_send_file_a_pdf_is_a_document_in_telegram_and_a_pdf_in_the_app(tmp_path: Path,
                                                                          monkeypatch: pytest.MonkeyPatch) -> None:
    home = tmp_path / "home"
    (home / "Documents").mkdir(parents=True)
    (home / "Documents" / "lease.pdf").write_bytes(PDF)
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: home))
    api = Api()
    rig, memory = rig_with(tmp_path, api)

    async def go() -> None:
        result = await rig.inlet._send_file(OWNER, "~/Documents/lease.pdf", "Your lease")
        assert result["ok"] and result["sent"] == "lease.pdf"
        assert api.documents == [(OWNER, str((home / "Documents" / "lease.pdf").resolve()), "Your lease")]
        [line] = media_lines(memory)
        assert line["text"] == "Your lease" and line["att"][0] == {**line["att"][0], "name": "lease.pdf", "kind": "pdf"}
        server, port, token = await serve(tmp_path, rig.inlet, uploads=rig.inlet.link_uploads)
        status, headers, body = await http(port, "GET", f"/api/link/file/{line['att'][0]['id']}", token=token)
        assert status == 200 and headers["content-type"] == "application/pdf" and body == PDF
        page = await messages(port, token)
        assert page["messages"][0]["attachments"] == [{**line["att"][0]}]          # a chip: no thumb for a PDF
        await server.close()

    asyncio.run(go())


def test_a_file_the_upload_door_would_refuse_is_still_kept_as_a_file(tmp_path: Path) -> None:
    archive = tmp_path / "photos.zip"
    archive.write_bytes(b"PK\x03\x04" + b"\x00" * 64)
    rig, memory = rig_with(tmp_path, Api())

    async def go() -> None:
        await rig.inlet._send_media(OWNER, archive, "", document=True)
        [line] = media_lines(memory)
        assert line["text"] == "" and line["att"][0]["kind"] == "file" and line["att"][0]["name"] == "photos.zip"
        kept = rig.inlet.link_uploads.get(line["att"][0]["id"])
        assert kept is not None and kept.kind == "file" and kept.mime == "application/zip" and kept.path.suffix == ".zip"
        server, port, token = await serve(tmp_path, rig.inlet, uploads=rig.inlet.link_uploads)
        page = await messages(port, token)
        assert page["messages"][0]["role"] == "eve" and page["messages"][0]["attachments"][0]["kind"] == "file"
        await server.close()

    asyncio.run(go())


def test_a_document_over_the_cap_is_a_line_naming_it(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(link_uploads, "MAX_BYTES", 1000)
    big = tmp_path / "video.mov"
    big.write_bytes(b"\x00" * (3 * 1024 * 1024))
    api = Api()
    rig, memory = rig_with(tmp_path, api)

    async def go() -> None:
        await rig.inlet._send_media(OWNER, big, "The clip", document=True)
        [line] = media_lines(memory)
        assert line["text"] == "The clip\nvideo.mov (3 MB) is too large to show here." and "att" not in line
        assert len(api.documents) == 1                                   # Telegram still gets it
        up = tmp_path / "up"
        assert not up.exists() or not any(up.iterdir())                 # nothing kept

    asyncio.run(go())


def test_a_built_apps_picture_goes_with_its_buttons_and_into_the_app(tmp_path: Path) -> None:
    api = Api()
    rig, memory = rig_with(tmp_path, api)

    async def go() -> None:
        await rig.inlet._send_app_photo(OWNER, jpeg(), "Habits is ready.", [("Open Habits", "https://example.test/a")])
        assert api.app_photos == [(OWNER, len(jpeg()), "Habits is ready.", [("Open Habits", "https://example.test/a")])]
        [line] = media_lines(memory)
        assert line["text"] == "Habits is ready." and line["att"][0]["kind"] == "image"

    asyncio.run(go())


# ---- failures ------------------------------------------------------------------------------------------------

def test_a_telegram_failure_is_raised_only_when_the_app_has_no_copy(tmp_path: Path) -> None:
    picture = tmp_path / "p.jpg"
    picture.write_bytes(jpeg())

    async def go() -> None:
        api = Api()
        api.fail = BotApiError(0, "ConnectError")
        rig, memory = rig_with(tmp_path / "a", api)
        await rig.inlet._send_media(OWNER, picture, "kept")              # the app has it: no raise
        assert len(media_lines(memory)) == 1
        bare, _ = rig_with(tmp_path / "b", api, store=False)             # no Eve Link door: as before
        with pytest.raises(BotApiError):
            await bare.inlet._send_media(OWNER, picture, "lost")

    asyncio.run(go())


def test_without_a_link_door_nothing_is_kept_and_telegram_gets_it_as_before(tmp_path: Path) -> None:
    rig, memory = rig_with(tmp_path, Api(), call("take_photo", {"note": ""}), say("Done."),
                           on_photo=camera(tmp_path), store=False)

    async def go() -> None:
        rig.inlet.hear("photo please", on_call=False)
        await wait_for(lambda: bool(rig.api.sent))
        assert len(rig.api.photos) == 1 and media_lines(memory) == []
        assert not (tmp_path / "up").exists()

    asyncio.run(go())


def test_the_link_door_lends_its_store_to_the_inlet(tmp_path: Path) -> None:
    rig, _ = rig_with(tmp_path, Api(), store=False)

    async def go() -> None:
        store = UploadStore(tmp_path / "door")
        server, port, token = await serve(tmp_path, rig.inlet, uploads=store)
        await messages(port, token)
        assert rig.inlet.link_uploads is store
        await server.close()

    asyncio.run(go())


# ---- GET /api/link/file/<id> ---------------------------------------------------------------------------------

def test_the_file_door_needs_the_token_and_serves_live_ids_only(tmp_path: Path) -> None:
    async def go() -> None:
        now = {"t": 5_000_000.0}
        store = UploadStore(tmp_path / "up", clock=lambda: now["t"])
        up = store.keep(jpeg(), "a.jpg")
        (tmp_path / "secret.txt").write_text("not yours")
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)), uploads=store)
        path = f"/api/link/file/{up.id}"
        assert (await http(port, "GET", path))[0] == 401
        assert (await http(port, "GET", path, token="x" * 43))[0] == 401
        assert (await http(port, "GET", path, token=token, origin="https://evil.example"))[0] == 403
        assert (await http(port, "POST", path, {}, token=token))[0] == 405
        status, headers, body = await http(port, "GET", path, token=token)          # positive control
        assert status == 200 and body == jpeg() and headers["x-content-type-options"] == "nosniff"
        for bad in ("0" * 32, up.id.upper(), up.id + ".meta.json", up.id[:-1], "..%2Fsecret.txt",
                    "%2e%2e%2f%2e%2e%2fsecret.txt", "../../secret.txt", "../secret.txt", "", up.id + "/x"):
            status, _, body = await http(port, "GET", "/api/link/file/" + bad, token=token)
            assert status == 404 and b"not yours" not in body, bad
        for p in (tmp_path / "up").iterdir():
            os.utime(p, (now["t"], now["t"]))
        now["t"] += link_uploads.KEEP_SECS + 1
        status, _, body = await http(port, "GET", path, token=token)
        assert status == 404 and b"gone" in body                                   # expired
        await server.close()

    asyncio.run(go())


def test_a_kept_file_is_also_an_owner_upload_s_door(tmp_path: Path) -> None:
    """The owner's own attachments open full size the same way."""
    async def go() -> None:
        store = UploadStore(tmp_path / "up")
        up = store.save(b"milk\neggs\n", "list.txt")
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)), uploads=store)
        status, headers, body = await http(port, "GET", f"/api/link/file/{up.id}", token=token)
        assert status == 200 and headers["content-type"] == "text/plain" and body == b"milk\neggs\n"
        await server.close()

    asyncio.run(go())


def test_keep_takes_what_save_refuses_but_not_empty_or_too_large(tmp_path: Path,
                                                                  monkeypatch: pytest.MonkeyPatch) -> None:
    store = UploadStore(tmp_path / "up")
    assert store.keep(b"MZ\x90\x00" * 4, "tool.exe").kind == "file"
    assert store.keep(b"MZ", "no-extension").path.suffix == ".bin"
    assert store.keep(jpeg(), "x.jpg").kind == "image"
    with pytest.raises(link_uploads.UploadError):
        store.keep(b"", "empty.txt")
    monkeypatch.setattr(link_uploads, "MAX_BYTES", 10)
    with pytest.raises(link_uploads.UploadError):
        store.keep(b"a" * 11, "big.bin")
    kept = [p for p in (tmp_path / "up").iterdir() if p.name.endswith(".meta.json")]
    assert len(kept) == 3 and all(b'"from": "eve"' in p.read_bytes() for p in kept)


def test_link_chat_names_every_kind_the_store_has() -> None:
    assert set(link_chat.ATT_KINDS) == set(link_uploads.KINDS)
    assert "media" in link_chat.KINDS and "media" in transcripts.KINDS


MEDIA_SENDS = re.compile(r"\.api\.(send_photo_web_apps|send_photo|send_document)\b")


def media_sends_outside_send_media(source: str) -> list[str]:
    """Each Bot API media send (a call or a method passed on) in ``source`` that is not inside
    ``TelegramInlet._send_media`` (the lines its ``def`` spans, by the parsed tree)."""
    tree = ast.parse(source)
    inside: list[tuple[int, int]] = [(n.lineno, n.end_lineno or n.lineno) for n in ast.walk(tree)
                                     if isinstance(n, ast.AsyncFunctionDef) and n.name == "_send_media"]
    out = []
    for no, text in enumerate(source.splitlines(), 1):
        if MEDIA_SENDS.search(text) and not any(a <= no <= b for a, b in inside):
            out.append(f"{no}: {text.strip()}")
    return out


def test_every_bot_api_media_send_in_telegram_py_goes_through_send_media() -> None:
    """A new ``self.api.send_photo`` (or ``send_document``, ``send_photo_web_apps``) anywhere else in telegram.py
    would reach Telegram but never the app: the owner would see nothing with Telegram app-only."""
    source = Path(telegram.__file__).read_text(encoding="utf-8")
    assert media_sends_outside_send_media(source) == []
    # positive control: the scan finds a direct send outside it, and all three of _send_media's own
    assert media_sends_outside_send_media(source + "\n\nasync def stray(api):\n    await self.api.send_photo(1, p)\n")
    body = inspect.getsource(telegram.TelegramInlet._send_media)
    assert {m.group(1) for m in MEDIA_SENDS.finditer(body)} == {"send_photo", "send_document", "send_photo_web_apps"}


# ---- the file door's type policy, eve's own cap, the quiet clock ----------------------------------------

@pytest.mark.parametrize("name, data, mime, disposition", [
    ("page.html", b"<html><script>alert(1)</script></html>", "application/octet-stream", "attachment"),
    ("logo.svg", b'<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>',
     "application/octet-stream", "attachment"),
    ("run.js", b"alert(1)", "application/octet-stream", "attachment"),
    ("feed.xml", b"<a/>", "application/octet-stream", "attachment"),
    ("photos.zip", b"PK\x03\x04" + b"\x00" * 16, "application/zip", "attachment"),
    ("notes.txt", b"milk", "text/plain", "attachment"),
    ("lease.pdf", PDF, "application/pdf", "inline"),
    ("desk.jpg", None, "image/jpeg", "inline"),
])
def test_the_file_door_serves_only_a_photo_or_pdf_inline_and_never_an_active_type(
        tmp_path: Path, name: str, data: Optional[bytes], mime: str, disposition: str) -> None:
    async def go() -> None:
        store = UploadStore(tmp_path / "up")
        body_in = data if data is not None else jpeg()
        up = store.keep(body_in, name)
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)), uploads=store)
        status, headers, body = await http(port, "GET", f"/api/link/file/{up.id}", token=token)
        assert status == 200 and body == body_in
        assert headers["content-type"] == mime and headers["content-disposition"] == disposition
        assert headers["x-content-type-options"] == "nosniff"
        await server.close()

    asyncio.run(go())


def test_served_maps_every_active_or_malformed_type_to_a_download() -> None:
    for kind, mime in [("file", "text/html"), ("file", "image/svg+xml"), ("file", "application/xhtml+xml"),
                       ("file", "text/javascript"), ("file", "application/x-javascript"), ("file", "text/xml"),
                       ("file", "application/rss+xml"), ("image", "image/svg+xml"), ("pdf", "text/html"),
                       ("file", ""), ("file", "text/html; charset=utf-8"), ("file", "TEXT/HTML")]:
        assert link_uploads.served(kind, mime) == ("application/octet-stream", "attachment"), (kind, mime)
    assert link_uploads.served("image", "image/png") == ("image/png", "inline")
    assert link_uploads.served("pdf", "application/pdf") == ("application/pdf", "inline")
    assert link_uploads.served("doc", link_uploads.OFFICE_SUFFIXES[".docx"]) == (
        link_uploads.OFFICE_SUFFIXES[".docx"], "attachment")            # "openxmlformats" is not an XML type
    assert link_uploads.served("text", "text/markdown") == ("text/markdown", "attachment")


def test_eves_copies_have_their_own_cap_and_never_refuse_the_owners_upload(tmp_path: Path) -> None:
    store = UploadStore(tmp_path / "up", max_kept=2, max_eve_kept=3)
    first = [store.keep(jpeg(), f"shot-{i}.jpg") for i in range(3)]
    for i, up in enumerate(first):                       # oldest first, whatever the clock's resolution
        for p in (tmp_path / "up").glob(up.id + "*"):
            os.utime(p, (time.time() - 100 + i, time.time() - 100 + i))
    newest = store.keep(jpeg(), "shot-3.jpg")
    assert store.get(first[0].id) is None                # the oldest copy made room
    assert all(store.get(u.id) is not None for u in (*first[1:], newest))
    # the owner's two uploads still go in beside eve's three, and the third is refused as before
    store.save(b"one", "a.txt")
    store.save(b"two", "b.txt")
    with pytest.raises(link_uploads.UploadError) as refused:
        store.save(b"three", "c.txt")
    assert refused.value.status == 503
    # and eve's next copy deletes eve's oldest, never an owner upload
    owners = [p for p in (tmp_path / "up").glob("*.txt")]
    store.keep(jpeg(), "shot-4.jpg")
    assert len(owners) == 2 and all(p.exists() for p in owners)
    assert store.get(first[1].id) is None


def test_eves_copies_make_room_by_bytes_too(tmp_path: Path) -> None:
    store = UploadStore(tmp_path / "up", max_eve_bytes=250)
    a = store.keep(b"\x01" * 100, "a.bin")
    for p in (tmp_path / "up").glob(a.id + "*"):
        os.utime(p, (time.time() - 100, time.time() - 100))
    b = store.keep(b"\x02" * 100, "b.bin")
    assert store.get(a.id) is not None
    c = store.keep(b"\x03" * 100, "c.bin")              # 300 > 250: the oldest goes
    assert store.get(a.id) is None and store.get(b.id) is not None and store.get(c.id) is not None


def test_a_media_line_leaves_an_open_chats_quiet_clock_alone(tmp_path: Path) -> None:
    picture = tmp_path / "p.jpg"
    picture.write_bytes(jpeg())
    rig, memory = rig_with(tmp_path, Api())
    now = {"t": 1000.0}
    rig.inlet._clock = lambda: now["t"]

    async def go() -> None:
        assert rig.inlet._record("owner", "say", "hi")
        conv, talked = rig.inlet._conv, rig.inlet._last_turn_at
        assert talked == 1000.0
        now["t"] = 1500.0
        await rig.inlet._send_media(OWNER, picture, "a task's screen")
        assert rig.inlet._conv == conv and rig.inlet._last_turn_at == talked        # the clock as it was
        rig.inlet._close_chat()
        assert rig.inlet._conv is None and rig.inlet._last_turn_at is None
        now["t"] = 2000.0
        await rig.inlet._send_media(OWNER, picture, "the chief's picture")           # opens its own conversation
        assert rig.inlet._conv is not None and rig.inlet._conv != conv
        assert rig.inlet._last_turn_at == 2000.0                                      # so it still closes
        assert len(media_lines(memory)) == 2

    asyncio.run(go())
