"""Tests for jsonl_tailer — focused on the parsing helpers rather than filesystem watching."""

from __future__ import annotations

import asyncio
import builtins
import json
import tracemalloc
from datetime import datetime, timedelta, timezone
from pathlib import Path

from watchfiles import Change

from eve_bridge.jsonl_tailer import JSONLTailer, _record_is_today, _today_key


def test_record_is_today_matches_local_day():
    now = datetime.now(tz=timezone.utc)
    ts = now.isoformat().replace("+00:00", "Z")
    assert _record_is_today(ts, _today_key())


def test_record_is_today_rejects_yesterday():
    past = datetime.now(tz=timezone.utc) - timedelta(days=2)
    ts = past.isoformat().replace("+00:00", "Z")
    assert not _record_is_today(ts, _today_key())


def test_record_is_today_rejects_non_strings():
    assert not _record_is_today(None, _today_key())
    assert not _record_is_today(12345, _today_key())
    assert not _record_is_today("", _today_key())


def test_record_is_today_rejects_bad_iso():
    assert not _record_is_today("not-a-date", _today_key())
    assert not _record_is_today("2026/04/22", _today_key())


def test_record_is_today_handles_z_suffix():
    # Mid-day UTC → always the same day regardless of timezone (well, almost).
    # Use a timestamp fresh enough that it's definitely today in any tz.
    ts = datetime.now(tz=timezone.utc).isoformat().replace("+00:00", "Z")
    assert _record_is_today(ts, _today_key())


# ---- initial sweep ----

def _write_jsonl(path: Path, records: list[dict]) -> None:
    path.write_text(
        "\n".join(json.dumps(r) for r in records) + "\n",
        encoding="utf-8",
    )


def _sync_sweep(root: Path) -> JSONLTailer:
    """Helper: spin up a tailer, run the initial sweep, return it. No file watching."""
    captured: list = []

    async def cb(c, t, cc, ct, e):
        captured.append((c, t, cc, ct, e))

    tailer = JSONLTailer(cb, roots=[root])
    asyncio.run(tailer._initial_sweep())
    return tailer


def test_cost_accumulates_alongside_tokens(tmp_path: Path):
    """Cost per file should be summed from usage records using the model's rates."""
    jsonl = tmp_path / "s.jsonl"
    _write_jsonl(jsonl, [
        {"type": "assistant", "message": {
            "role": "assistant",
            "model": "claude-sonnet-4-6",
            "content": [{"type": "text", "text": "hi"}],
            "usage": {"input_tokens": 1_000_000, "output_tokens": 1_000_000},
        }},
        {"type": "assistant", "message": {
            "role": "assistant",
            "model": "claude-opus-4-7",
            "content": [{"type": "text", "text": "hi"}],
            "usage": {"output_tokens": 1_000_000},
        }},
    ])
    tailer = _sync_sweep(tmp_path)
    # Sonnet (3 + 15) + Opus (75) = $93
    assert abs(tailer._cost_per_file[str(jsonl)] - 93.0) < 1e-6
    # Both records lack a timestamp, so today's dict stays empty.
    assert tailer._today_cost_per_file.get(str(jsonl), 0.0) == 0.0


def test_assistant_record_without_content_does_not_crash(tmp_path: Path):
    """An assistant record without a content array shouldn't crash; its tokens still count."""
    jsonl = tmp_path / "s.jsonl"
    _write_jsonl(jsonl, [
        {"type": "assistant", "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": "first"}],
            "usage": {"output_tokens": 1},
        }},
        {"type": "assistant", "message": {
            "role": "assistant",
            # No "content" field
            "usage": {"output_tokens": 1},
        }},
    ])
    tailer = _sync_sweep(tmp_path)
    assert tailer._tokens_per_file[str(jsonl)] == 2


def _assistant(uuid: str, text: str = "hello") -> dict:
    return {"uuid": uuid, "message": {"role": "assistant", "content": [
        {"type": "text", "text": text}], "usage": {"output_tokens": 1}}}


def test_run_reads_history_once_and_does_not_replay_it(tmp_path: Path, monkeypatch):
    path = tmp_path / "s.jsonl"
    _write_jsonl(path, [_assistant("old")])
    reads = []
    emitted = []
    real_open = builtins.open

    def track_open(file, mode="r", *args, **kwargs):
        if str(file) == str(path) and "r" in mode:
            reads.append(str(file))
        return real_open(file, mode, *args, **kwargs)

    async def no_watch(*args, **kwargs):
        return
        yield

    async def on_update(*args):
        pass

    async def on_text(*args):
        emitted.append(args)

    monkeypatch.setattr(builtins, "open", track_open)
    monkeypatch.setattr("eve_bridge.jsonl_tailer.awatch", no_watch)
    tailer = JSONLTailer(on_update, roots=[tmp_path], on_assistant_text=on_text)
    asyncio.run(tailer.run())
    assert reads == [str(path)]
    assert tailer._emitted_assistant_uuids[str(path)] == {"old"}
    assert emitted == []


def test_record_appended_after_history_sweep_is_emitted(tmp_path: Path, monkeypatch):
    path = tmp_path / "s.jsonl"
    _write_jsonl(path, [_assistant("old")])
    emitted = []

    async def on_update(*args):
        pass

    async def on_text(*args):
        emitted.append(args)

    tailer = JSONLTailer(on_update, roots=[tmp_path], on_assistant_text=on_text)
    real_sweep = tailer._initial_sweep

    async def append_after_sweep():
        await real_sweep()
        with path.open("a") as file:
            file.write(json.dumps(_assistant("new", "new answer")) + "\n")

    async def one_change(*args, **kwargs):
        yield {(Change.modified, str(path))}

    monkeypatch.setattr(tailer, "_initial_sweep", append_after_sweep)
    monkeypatch.setattr("eve_bridge.jsonl_tailer.awatch", one_change)
    asyncio.run(tailer.run())
    assert emitted == [(str(path), "new answer", "new")]
    assert tailer._tokens_per_file[str(path)] == 2


def test_non_object_record_does_not_discard_later_valid_records(tmp_path: Path):
    path = tmp_path / "s.jsonl"
    path.write_text("null\n[]\n42\n" + json.dumps(_assistant("a1")) + "\n")
    tailer = _sync_sweep(tmp_path)
    assert tailer._tokens_per_file[str(path)] == 1


def test_deleting_transcript_releases_assistant_history(tmp_path: Path):
    path = tmp_path / "s.jsonl"
    _write_jsonl(path, [_assistant("old")])
    tailer = _sync_sweep(tmp_path)
    tailer._emitted_assistant_uuids[str(path)] = {"old"}
    assert str(path) in tailer._emitted_assistant_uuids
    asyncio.run(tailer._handle_changes({(Change.deleted, str(path))}))
    assert str(path) not in tailer._emitted_assistant_uuids


def test_partial_line_is_left_for_the_next_change(tmp_path: Path):
    path = tmp_path / "s.jsonl"
    complete = json.dumps(_assistant("old")) + "\n"
    partial = json.dumps(_assistant("new", "new text"))
    path.write_text(complete + partial)
    tailer = _sync_sweep(tmp_path)
    assert tailer._offsets[str(path)] == len(complete.encode())
    assert tailer._emitted_assistant_uuids[str(path)] == {"old"}
    tailer._initial_sweep_done = True
    tailer.on_assistant_text = lambda *args: None
    with path.open("a") as file:
        file.write("\n")
    tailer._process_file(str(path))
    assert tailer._tokens_per_file[str(path)] == 2
    assert tailer._pending_assistant_emits == [(str(path), "new text", "new")]
    tailer._process_file(str(path))
    assert tailer._tokens_per_file[str(path)] == 2
    assert len(tailer._pending_assistant_emits) == 1


def test_truncated_transcript_resets_counters_and_partial_offset(tmp_path: Path):
    path = tmp_path / "s.jsonl"
    _write_jsonl(path, [_assistant("old"), _assistant("older")])
    tailer = _sync_sweep(tmp_path)
    assert tailer._tokens_per_file[str(path)] == 2
    path.write_text(json.dumps(_assistant("new"))[:-1])
    tailer._process_file(str(path))
    assert tailer._tokens_per_file.get(str(path), 0) == 0
    with path.open("a") as file:
        file.write("}\n")
    tailer._process_file(str(path))
    assert tailer._tokens_per_file[str(path)] == 1
    assert tailer._offsets[str(path)] == path.stat().st_size


def test_process_file_stops_at_its_size_snapshot(tmp_path: Path, monkeypatch):
    path = tmp_path / "s.jsonl"
    _write_jsonl(path, [_assistant("first")])
    snapshot_size = path.stat().st_size
    with path.open("a") as file:
        file.write(json.dumps(_assistant("later")) + "\n")
    monkeypatch.setattr("eve_bridge.jsonl_tailer.os.path.getsize", lambda p: snapshot_size)
    tailer = _sync_sweep(tmp_path)
    assert tailer._tokens_per_file[str(path)] == 1
    assert tailer._offsets[str(path)] == snapshot_size
    assert tailer._emitted_assistant_uuids[str(path)] == {"first"}


def test_process_file_memory_does_not_scale_with_history(tmp_path: Path):
    paths = []
    for name, count in (("small", 400), ("large", 4000)):
        path = tmp_path / f"{name}.jsonl"
        with path.open("w") as file:
            for _ in range(count):
                file.write(json.dumps({"message": {"role": "assistant", "content": "x" * 200,
                                                  "usage": {"output_tokens": 1}}}) + "\n")
        paths.append((path, count))

    peaks = []
    for path, count in paths:
        tailer = JSONLTailer(lambda *args: None, roots=[tmp_path])
        tracemalloc.start()
        try:
            tailer._process_file(str(path))
            peaks.append(tracemalloc.get_traced_memory()[1])
        finally:
            tracemalloc.stop()
        assert tailer._tokens_per_file[str(path)] == count
    assert peaks[1] < 2 * peaks[0] + 100_000, peaks
    # Positive control: the previous bulk-read/split approach holds all lines.
    tracemalloc.start()
    try:
        raw = paths[1][0].read_bytes()
        lines = raw.splitlines()
        bulk_peak = tracemalloc.get_traced_memory()[1]
        assert len(lines) == 4000
    finally:
        tracemalloc.stop()
    assert bulk_peak > 5 * peaks[1], (peaks, bulk_peak)


def test_malformed_usage_does_not_discard_later_valid_records(tmp_path: Path):
    path = tmp_path / "s.jsonl"
    malformed = [_assistant(f"bad-{i}") for i in range(3)]
    malformed[0]["message"]["usage"]["output_tokens"] = "not a count"
    malformed[1]["message"]["usage"]["input_tokens"] = []
    malformed[1]["message"]["usage"]["cache_creation"] = [1]
    malformed[2]["message"]["model"] = {"not": "a model id"}
    _write_jsonl(path, malformed + [_assistant("valid")])
    tailer = _sync_sweep(tmp_path)
    assert tailer._tokens_per_file[str(path)] == 1
    assert tailer._offsets[str(path)] == path.stat().st_size
