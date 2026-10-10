"""Photos: the pictures eve keeps of things it finds cool.

The diary already turns a camera frame into a thought (diary.py). A few of
those views are worth more than a sentence, and for those eve asks the
board for one full-resolution frame ({"cmd":"snap"} → 320x240 at JPEG
quality 85, look.cpp) and keeps it.

This module is only the shelf: where a photo goes, what it is called, how
many are kept, and how a kept photo is referenced from the diary. What is
worth photographing is decided in diary.py (the cool factor) — keeping the
two apart means the retention rules are testable without a model, a board
or a network, like the rest of the explorer.

Layout under the notes directory (``EVE_NOTES_DIR``):

    photos/YYYY-MM-DD/HHMMSS-<record id>.jpg

so a day's pictures sit beside that day's diary file and the name sorts by
time. The path recorded in ``memory.jsonl`` and written into the day's
Markdown is always **relative to the notes directory**
(``photos/2026-09-06/141207-42.jpg``), so the directory can be moved or
mirrored (the macOS widget reads it out of the App Group container) without
rewriting a single record.

In the Markdown the photo is an image line indented under its diary line:

    - 14:12 yaw=+20 pitch=40 — Someone left the good headphones on my desk.
      ![](photos/2026-09-06/141207-42.jpg)

The diary line itself is untouched, so every existing reader still parses it
(the Swift widget's NoteLine.parse and notes_widget.py both need a leading
``- HH:MM``); the image line has no time, so both skip it, and a Markdown
viewer shows the picture in place.

Retention: photos are capped by count and by total bytes, oldest first, so
an unattended robot cannot fill a disk. Empty day directories are removed
with their last photo.
"""

from __future__ import annotations

import logging
import os
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any, Iterable, Optional

log = logging.getLogger(__name__)

PHOTOS_DIR = "photos"
DEFAULT_MAX_PHOTOS = 300
DEFAULT_MAX_MB = 100.0
JPEG_MAGIC = b"\xff\xd8"


@dataclass(frozen=True)
class PhotoConfig:
    max_photos: int = DEFAULT_MAX_PHOTOS
    max_bytes: int = int(DEFAULT_MAX_MB * 1024 * 1024)


def _env_int(name: str, default: int, environ: Any) -> int:
    raw = (environ.get(name) or "").strip()
    if not raw:
        return default
    try:
        v = int(float(raw))
    except ValueError:
        log.warning("photos: %s=%r is not a number; using %d", name, raw, default)
        return default
    return max(0, v)


def configured(environ: Any = None) -> PhotoConfig:
    """``EVE_PHOTOS_MAX`` (count) and ``EVE_PHOTOS_MAX_MB`` (total size)."""
    env = os.environ if environ is None else environ
    mb = (env.get("EVE_PHOTOS_MAX_MB") or "").strip()
    try:
        max_mb = float(mb) if mb else DEFAULT_MAX_MB
    except ValueError:
        log.warning("photos: EVE_PHOTOS_MAX_MB=%r is not a number; using %g", mb, DEFAULT_MAX_MB)
        max_mb = DEFAULT_MAX_MB
    return PhotoConfig(
        max_photos=_env_int("EVE_PHOTOS_MAX", DEFAULT_MAX_PHOTOS, env),
        max_bytes=int(max(0.0, max_mb) * 1024 * 1024),
    )


def photos_root(notes_dir: Path) -> Path:
    return notes_dir / PHOTOS_DIR


def relative_path(when: datetime, record_id: int) -> str:
    """The notes-dir-relative path a photo taken now for this record gets."""
    return f"{PHOTOS_DIR}/{when:%Y-%m-%d}/{when:%H%M%S}-{record_id}.jpg"


def photo_line(rel_path: str) -> str:
    """The Markdown image line that goes under the diary line."""
    return f"  ![]({rel_path})"


def save(notes_dir: Path, when: datetime, record_id: int, jpeg: bytes,
         config: Optional[PhotoConfig] = None) -> Optional[str]:
    """Write one JPEG and prune. Returns its notes-dir-relative path, or None
    when the bytes are not a JPEG, photos are switched off (max 0), or the
    write failed — a photo is never worth losing a thought over."""
    cfg = config if config is not None else configured()
    if cfg.max_photos <= 0 or cfg.max_bytes <= 0:
        return None
    if not jpeg or jpeg[:2] != JPEG_MAGIC:
        log.warning("photos: not a JPEG (%d bytes) — not saved", len(jpeg))
        return None
    rel = relative_path(when, record_id)
    path = notes_dir / rel
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        try:
            os.chmod(photos_root(notes_dir), 0o700)
        except OSError:
            pass
        path.write_bytes(jpeg)
    except OSError as e:
        log.warning("photos: cannot write %s: %s", path, e)
        return None
    prune(notes_dir, cfg)
    return rel


def all_photos(notes_dir: Path) -> list[Path]:
    """Every kept photo, oldest first by name (the name sorts by date + time)."""
    root = photos_root(notes_dir)
    if not root.is_dir():
        return []
    out: list[Path] = []
    for day in sorted(p for p in root.iterdir() if p.is_dir()):
        out.extend(sorted(f for f in day.iterdir() if f.suffix == ".jpg"))
    return out


def prune(notes_dir: Path, config: Optional[PhotoConfig] = None) -> list[Path]:
    """Delete the oldest photos until both caps hold. Returns what went."""
    cfg = config if config is not None else configured()
    files = all_photos(notes_dir)
    sizes = {}
    for f in files:
        try:
            sizes[f] = f.stat().st_size
        except OSError:
            sizes[f] = 0
    total = sum(sizes.values())
    removed: list[Path] = []
    for f in files:
        if len(files) - len(removed) <= cfg.max_photos and total <= cfg.max_bytes:
            break
        try:
            f.unlink()
        except OSError as e:
            log.debug("photos: cannot remove %s: %s", f, e)
            continue
        total -= sizes[f]
        removed.append(f)
        _rmdir_if_empty(f.parent)
    if removed:
        log.info("photos: pruned %d (cap %d photos / %.0f MB)", len(removed), cfg.max_photos,
                 cfg.max_bytes / 1024 / 1024)
    return removed


def _rmdir_if_empty(day_dir: Path) -> None:
    try:
        next(day_dir.iterdir())
    except StopIteration:
        try:
            day_dir.rmdir()
        except OSError:
            pass
    except OSError:
        pass


def usage(notes_dir: Path) -> tuple[int, int]:
    """(count, total bytes) of the photos on the shelf."""
    files = all_photos(notes_dir)
    total = 0
    for f in files:
        try:
            total += f.stat().st_size
        except OSError:
            pass
    return len(files), total


def format_usage(notes_dir: Path) -> str:
    n, total = usage(notes_dir)
    return f"{n} photo{'' if n == 1 else 's'}, {total / 1024 / 1024:.1f} MB"


def iter_recent(notes_dir: Path, last: int = 10) -> Iterable[Path]:
    """The newest ``last`` photos, newest first."""
    files = all_photos(notes_dir)
    return reversed(files[-last:] if last else files)
