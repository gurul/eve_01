"""Bounded Telegram image intake and private, temporary files for local relays."""
from __future__ import annotations

import base64
import io
import os
import re
import stat
import tempfile
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import httpx
from PIL import Image, UnidentifiedImageError

MAX_BYTES = 10 * 1024 * 1024
MAX_PIXELS = 25_000_000
KEEP_SECS = 24 * 60 * 60
FORMATS = {"JPEG": ("image/jpeg", ".jpg"), "PNG": ("image/png", ".png"),
           "WEBP": ("image/webp", ".webp"), "GIF": ("image/gif", ".gif")}


class ImageError(Exception):
    """Safe to show in chat; never contains a token, URL, or image content."""


@dataclass(frozen=True)
class Attachment:
    file_id: str
    size: int = 0


@dataclass(frozen=True)
class ReceivedImage:
    data: bytes
    mime: str
    suffix: str

    def data_url(self) -> str:
        return f"data:{self.mime};base64," + base64.b64encode(self.data).decode("ascii")


def attachment(message: dict) -> Attachment | None:
    photos = message.get("photo")
    candidates = photos if isinstance(photos, list) else []
    document = message.get("document")
    if isinstance(document, dict) and str(document.get("mime_type", "")).startswith("image/"):
        candidates = [document]
    valid = [p for p in candidates if isinstance(p, dict) and isinstance(p.get("file_id"), str)
             and p["file_id"] and isinstance(p.get("file_size", 0), int)
             and not isinstance(p.get("file_size", 0), bool) and p.get("file_size", 0) >= 0]
    if not valid:
        return None
    # Telegram orders photo sizes from smallest to largest. Prefer measured size when provided.
    chosen = max(enumerate(valid), key=lambda pair: (pair[1].get("file_size", 0), pair[0]))[1]
    return Attachment(chosen["file_id"], chosen.get("file_size", 0))


def validate(data: bytes) -> ReceivedImage:
    if len(data) > MAX_BYTES:
        raise ImageError("That image is too large. Send an image under 10 MB.")
    try:
        with Image.open(io.BytesIO(data)) as im:
            if im.format not in FORMATS or getattr(im, "n_frames", 1) != 1:
                raise ImageError("Send a still JPEG, PNG, WebP, or GIF image.")
            if im.width * im.height > MAX_PIXELS:
                raise ImageError("That image is too large. Send a smaller version under 25 megapixels.")
            mime, suffix = FORMATS[im.format]
            im.verify()
        # verify() alone does not decode JPEG pixel data.
        with Image.open(io.BytesIO(data)) as im:
            im.load()
    except (UnidentifiedImageError, OSError, ValueError, Image.DecompressionBombError) as exc:
        raise ImageError("I couldn't read that image. Send it again as a photo or a still JPEG or PNG.") from exc
    return ReceivedImage(data, mime, suffix)


async def download(client: Any, get_file: Any, token: str, item: Attachment) -> ReceivedImage:
    if item.size > MAX_BYTES:
        raise ImageError("That image is too large. Send an image under 10 MB.")
    result = await get_file("getFile", {"file_id": item.file_id})
    if not isinstance(result, dict):
        raise ImageError("Telegram couldn't provide that image. Please send it again.")
    size = result.get("file_size", 0)
    if not isinstance(size, int) or size < 0 or size > MAX_BYTES:
        raise ImageError("That image is too large. Send an image under 10 MB.")
    path = result.get("file_path", "")
    if (not isinstance(path, str) or not re.fullmatch(r"[A-Za-z0-9_/-]+\.[A-Za-z0-9]+", path)
            or path.startswith("/") or any(p in (".", "..", "") for p in path.split("/"))):
        raise ImageError("Telegram returned an invalid image path. Please send it again.")
    data = bytearray()
    try:
        async with client.stream("GET", f"https://api.telegram.org/file/bot{token}/{path}",
                                 follow_redirects=False, timeout=30) as response:
            if response.status_code != 200:
                raise ImageError("Telegram couldn't download that image. Please send it again.")
            length = response.headers.get("content-length")
            if length and (not length.isdigit() or int(length) > MAX_BYTES):
                raise ImageError("That image is too large. Send an image under 10 MB.")
            async for chunk in response.aiter_bytes(chunk_size=64 * 1024):
                data.extend(chunk)
                if len(data) > MAX_BYTES:
                    raise ImageError("That image is too large. Send an image under 10 MB.")
    except httpx.HTTPError:
        raise ImageError("The image download failed. Please send it again.") from None
    import asyncio
    return await asyncio.to_thread(validate, bytes(data))


def save(image: ReceivedImage, root: Path | None = None) -> Path:
    """Retain for delayed relay reads; expire on the next save, bounded to 100 files."""
    root = root or Path(tempfile.gettempdir()) / f"eve-telegram-images-{os.getuid()}"
    root.mkdir(mode=0o700, exist_ok=True)
    info = root.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ImageError("The private image folder is unavailable.")
    remaining = 0
    for path in root.iterdir():
        info = path.lstat()
        if re.fullmatch(r"[0-9a-f]{32}\.(jpg|png|webp|gif)", path.name) and stat.S_ISREG(info.st_mode):
            if time.time() - info.st_mtime > KEEP_SECS:
                path.unlink()
            else:
                remaining += 1
    if remaining >= 100:
        raise ImageError("The temporary image folder is full. Try again after older images expire.")
    path = root / (uuid.uuid4().hex + image.suffix)
    with path.open("xb") as out:
        os.chmod(path, 0o600)
        out.write(image.data)
    return path.resolve()
