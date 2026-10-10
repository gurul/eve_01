"""photos.py: naming, the Markdown image line, the two retention caps, and
the path guard. No board, no network — bytes and a tmp directory."""

from __future__ import annotations

import logging
from datetime import datetime
from pathlib import Path

from eve_bridge import photos
from eve_bridge.photos import (
    PhotoConfig,
    configured,
    photo_line,
    prune,
    relative_path,
    save,
    usage,
)

WHEN = datetime(2026, 9, 6, 14, 12, 7)
JPEG = b"\xff\xd8\xff\xe0" + b"\x00" * 64 + b"\xff\xd9"


def _cfg(**kw) -> PhotoConfig:
    base = dict(max_photos=100, max_bytes=10 * 1024 * 1024)
    base.update(kw)
    return PhotoConfig(**base)


def test_relative_path_sorts_by_day_then_time() -> None:
    assert relative_path(WHEN, 42) == "photos/2026-09-06/141207-42.jpg"
    earlier = relative_path(datetime(2026, 9, 6, 9, 5, 0), 7)
    assert earlier < relative_path(WHEN, 42)


def test_photo_line_is_an_indented_image_that_is_not_a_note() -> None:
    line = photo_line("photos/2026-09-06/141207-42.jpg")
    assert line == "  ![](photos/2026-09-06/141207-42.jpg)"
    # Both readers key on a leading "- HH:MM"; this line has neither.
    from eve_bridge.notes_widget import LINE_RE
    assert LINE_RE.match(line.strip()) is None
    assert not line.lstrip().startswith("- ")


def test_save_writes_the_jpeg_private_and_returns_a_relative_path(tmp_path: Path) -> None:
    rel = save(tmp_path, WHEN, 42, JPEG, _cfg())
    assert rel == "photos/2026-09-06/141207-42.jpg"
    path = tmp_path / rel
    assert path.read_bytes() == JPEG
    assert (tmp_path / "photos").stat().st_mode & 0o777 == 0o700
    assert usage(tmp_path) == (1, len(JPEG))


def test_save_refuses_non_jpeg_bytes_and_a_zero_cap(tmp_path: Path, caplog) -> None:
    with caplog.at_level(logging.WARNING):
        assert save(tmp_path, WHEN, 1, b"GIF89a...", _cfg()) is None
    assert any("not a JPEG" in r.message for r in caplog.records)
    assert save(tmp_path, WHEN, 1, JPEG, _cfg(max_photos=0)) is None
    assert save(tmp_path, WHEN, 1, JPEG, _cfg(max_bytes=0)) is None
    assert not (tmp_path / "photos").exists()


def test_saving_prunes_the_oldest_past_the_count_cap(tmp_path: Path) -> None:
    cfg = _cfg(max_photos=3)
    rels = [save(tmp_path, datetime(2026, 9, 6, 10, 0, i), i, JPEG, cfg) for i in range(5)]
    kept = sorted(p.name for p in photos.all_photos(tmp_path))
    assert len(kept) == 3
    assert [r.split("/")[-1] for r in rels[2:]] == kept       # the three newest
    assert not (tmp_path / rels[0]).exists()


def test_pruning_past_the_size_cap_ignores_the_count(tmp_path: Path) -> None:
    cfg = _cfg(max_photos=100, max_bytes=len(JPEG) * 2)
    for i in range(4):
        save(tmp_path, datetime(2026, 9, 6, 10, 0, i), i, JPEG, cfg)
    n, total = usage(tmp_path)
    assert n == 2 and total <= cfg.max_bytes


def test_pruning_removes_the_day_directory_it_empties(tmp_path: Path) -> None:
    cfg = _cfg(max_photos=1)
    save(tmp_path, datetime(2026, 9, 5, 10, 0, 0), 1, JPEG, cfg)
    save(tmp_path, datetime(2026, 9, 6, 10, 0, 0), 2, JPEG, cfg)
    assert not (tmp_path / "photos" / "2026-09-05").exists()
    assert (tmp_path / "photos" / "2026-09-06").is_dir()


def test_prune_on_an_empty_shelf_is_a_noop(tmp_path: Path) -> None:
    assert prune(tmp_path, _cfg()) == []
    assert usage(tmp_path) == (0, 0)


def test_configured_reads_the_env_and_falls_back_on_junk(caplog) -> None:
    assert configured({}) == PhotoConfig()
    c = configured({"EVE_PHOTOS_MAX": "12", "EVE_PHOTOS_MAX_MB": "2.5"})
    assert c.max_photos == 12 and c.max_bytes == int(2.5 * 1024 * 1024)
    with caplog.at_level(logging.WARNING):
        junk = configured({"EVE_PHOTOS_MAX": "lots", "EVE_PHOTOS_MAX_MB": "big"})
    assert junk == PhotoConfig()
    assert len(caplog.records) == 2


def test_iter_recent_is_newest_first(tmp_path: Path) -> None:
    for i in range(4):
        save(tmp_path, datetime(2026, 9, 6, 10, 0, i), i, JPEG, _cfg())
    names = [p.name for p in photos.iter_recent(tmp_path, last=2)]
    assert names == ["100003-3.jpg", "100002-2.jpg"]
