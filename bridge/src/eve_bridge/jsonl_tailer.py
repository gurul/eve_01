"""Watch Claude Code transcript files under ~/.claude/projects/ for token + entry updates.

Hooks don't expose cumulative token counts, so we parse them out of the session
JSONL files. Each assistant message has a `usage.output_tokens` field that we
sum per-file, then aggregate across all files for the grand total.

We also pull short snippets (user prompts + tool-call summaries) to populate
the stick's `entries` list.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
from datetime import datetime
from pathlib import Path
from typing import Any, Awaitable, Callable, Optional, Sequence  # Optional used below

from watchfiles import Change, awatch

from . import pricing
from .claude_home import transcript_roots

log = logging.getLogger(__name__)

# Callback: async (tokens_cumulative, tokens_today, cost_cumulative, cost_today, entries) -> None.
# Cost values are USD estimates; see pricing.py.
TokensCallback = Callable[[int, int, float, float, list[tuple[float, str]]], Awaitable[None]]

# Callback fired the moment a new assistant record with text content is parsed.
# Async (transcript_path, text, uuid) -> None. Skipped for tool_use-only turns.
AssistantTextCallback = Callable[[str, str, str], Awaitable[None]]


class JSONLTailer:
    """Incrementally reads every transcript JSONL, tracks file offsets so we only
    process new bytes, and recomputes aggregates on change."""

    def __init__(
        self,
        on_update: TokensCallback,
        roots: Optional[Sequence[Path]] = None,
        on_assistant_text: Optional[AssistantTextCallback] = None,
    ) -> None:
        self.on_update = on_update
        self.on_assistant_text = on_assistant_text
        # Several roots because one daemon serves sessions from more than one
        # Claude Code config home (see claude_home.py). Aggregates are keyed by
        # file path, so they sum across roots without further bookkeeping.
        self.roots: list[Path] = list(roots) if roots else list(transcript_roots())
        # file path → (offset, session_tokens_output, per_day_tokens_output)
        self._offsets: dict[str, int] = {}
        self._tokens_per_file: dict[str, int] = {}
        # day_key → sum of tokens_today across files (day_key is YYYY-MM-DD local)
        self._day_key = _today_key()
        self._today_tokens_per_file: dict[str, int] = {}
        # Mirror of the tokens dicts in USD. Estimated via pricing.estimate_cost
        # at parse time, so changing pricing.py rates only affects new records.
        self._cost_per_file: dict[str, float] = {}
        self._today_cost_per_file: dict[str, float] = {}
        # file path → set of assistant uuids we've already emitted so that the
        # initial sweep on daemon startup doesn't re-fire the callback for
        # every historical assistant message.
        self._emitted_assistant_uuids: dict[str, set[str]] = {}
        # Until the initial sweep returns, history seeds the UUIDs without
        # firing the live callback for past turns.
        self._initial_sweep_done = False
        # Deferred callbacks collected during _consume_obj (which is sync).
        # Fired from the awatch loop (async context).
        self._pending_assistant_emits: list[tuple[str, str, str]] = []

    async def run(self) -> None:
        for r in self.roots:
            if not r.exists():
                log.warning("transcript root %s does not exist; creating", r)
                r.mkdir(parents=True, exist_ok=True)
        log.info("tailing transcripts: %s", ", ".join(str(r) for r in self.roots))

        # Initial sweep so aggregates are hot before any file event fires.
        # Marked "sweep not done" so _consume_obj skips the live-emit callback
        # during history replay; callbacks only fire for future writes.
        await self._initial_sweep()
        self._initial_sweep_done = True
        await self._emit()

        # Watch for changes. watchfiles yields sets of (Change, path).
        try:
            watched = [str(r) for r in self.roots]
            async for changes in awatch(*watched, recursive=True, stop_event=None):
                await self._handle_changes(changes)
                await self._fire_pending_emits()
                await self._emit()
        except Exception:  # noqa: BLE001
            log.exception("jsonl tailer crashed")

    async def _fire_pending_emits(self) -> None:
        if not self._pending_assistant_emits or self.on_assistant_text is None:
            self._pending_assistant_emits.clear()
            return
        to_fire = self._pending_assistant_emits[:]
        self._pending_assistant_emits.clear()
        for path, text, uuid in to_fire:
            try:
                await self.on_assistant_text(path, text, uuid)
            except Exception:  # noqa: BLE001
                log.exception("on_assistant_text callback failed")

    async def _initial_sweep(self) -> None:
        """Every transcript under the roots, read from its start. On a Mac with a
        year of sessions that is hundreds of files and hundreds of megabytes, and
        done on the loop it held the daemon for 41 s at start (the loop watchdog
        named it, 2026-09-21): the board sat at "--:--" and the wake word waited.
        So the walk runs in a thread; nothing else touches the offsets until it returns."""
        await asyncio.to_thread(self._sweep_all)

    def _sweep_all(self) -> None:
        for p in (p for r in self.roots for p in r.rglob("*.jsonl")):
            try:
                self._process_file(str(p))
            except Exception as e:  # noqa: BLE001
                log.debug("initial sweep of %s failed: %s", p, e)

    async def _handle_changes(self, changes: set[tuple[Change, str]]) -> None:
        # Read and parse the batch in one worker. Await it before emitting or
        # processing another batch, so the dictionaries still have one writer.
        await asyncio.to_thread(self._process_changes, changes)

    def _process_changes(self, changes: set[tuple[Change, str]]) -> None:
        for change, path_str in changes:
            if not path_str.endswith(".jsonl"):
                continue
            if change == Change.deleted:
                self._offsets.pop(path_str, None)
                self._tokens_per_file.pop(path_str, None)
                self._today_tokens_per_file.pop(path_str, None)
                self._cost_per_file.pop(path_str, None)
                self._today_cost_per_file.pop(path_str, None)
                self._emitted_assistant_uuids.pop(path_str, None)
                continue
            try:
                self._process_file(path_str)
            except Exception as e:  # noqa: BLE001
                log.debug("process %s failed: %s", path_str, e)

    def _process_file(self, path: str) -> None:
        """Read new bytes since last offset. Parse each line, accumulate tokens + entries."""
        try:
            size = os.path.getsize(path)
        except OSError:
            return
        start = self._offsets.get(path, 0)
        if start > size:
            # File was truncated/rotated. Reset.
            start = 0
            self._tokens_per_file.pop(path, None)
            self._today_tokens_per_file.pop(path, None)
            self._cost_per_file.pop(path, None)
            self._today_cost_per_file.pop(path, None)
            self._offsets[path] = 0
        if start == size:
            return

        current_day = _today_key()
        if current_day != self._day_key:
            # Day rolled over — reset today-only counters.
            self._day_key = current_day
            self._today_tokens_per_file.clear()
            self._today_cost_per_file.clear()

        # Buffered line reads avoid holding a transcript and its split copy in
        # RAM. Limit every read to the size snapshot, so a busy writer cannot
        # extend this batch forever. Incomplete trailing lines stay unconsumed.
        with open(path, "rb") as f:
            f.seek(start)
            offset = start
            while offset < size:
                raw = f.readline(size - offset)
                if not raw.endswith(b"\n"):
                    break
                offset += len(raw)
                self._offsets[path] = offset
                if not raw.strip():
                    continue
                try:
                    obj = json.loads(raw)
                except ValueError:
                    continue
                if isinstance(obj, dict):
                    self._consume_obj(path, obj, current_day)

    def _consume_obj(self, path: str, obj: dict[str, Any], current_day: str) -> None:
        # Claude Code transcript entries can nest differently across versions.
        # Check a few common shapes.
        msg = obj.get("message") if isinstance(obj.get("message"), dict) else obj
        if not isinstance(msg, dict):
            return

        # A new assistant record with text fires the live callback.
        if msg.get("role") == "assistant":
            content = msg.get("content")
            record_uuid = obj.get("uuid")
            if isinstance(record_uuid, str) and record_uuid:
                seen = self._emitted_assistant_uuids.setdefault(path, set())
                if not self._initial_sweep_done:
                    # Seed only records actually consumed by the initial sweep.
                    # A second full scan can see newly appended records beyond
                    # the offset and wrongly suppress their live callbacks.
                    seen.add(record_uuid)
                elif record_uuid not in seen and isinstance(content, list) and self.on_assistant_text is not None:
                    for block in content:
                        if (
                            isinstance(block, dict)
                            and block.get("type") == "text"
                            and isinstance(block.get("text"), str)
                            and block["text"].strip()
                        ):
                            seen.add(record_uuid)
                            self._pending_assistant_emits.append(
                                (path, block["text"].strip(), record_uuid)
                            )
                            break

        usage = msg.get("usage")
        if not isinstance(usage, dict):
            return
        try:
            out = int(usage.get("output_tokens") or 0)
            if not out:
                return
            cost = pricing.estimate_cost(msg.get("model") or "", usage)
        except (ValueError, TypeError, AttributeError, OverflowError):
            # A foreign/torn record's metadata must not prevent subsequent
            # complete records in this file from being counted.
            log.debug("invalid transcript usage metadata in %s", path)
            return
        self._tokens_per_file[path] = self._tokens_per_file.get(path, 0) + out
        self._cost_per_file[path] = self._cost_per_file.get(path, 0.0) + cost
        # Only attribute to today's counter if the record's own timestamp falls
        # within the current local day. Records without a timestamp contribute
        # to cumulative only.
        if _record_is_today(obj.get("timestamp"), current_day):
            self._today_tokens_per_file[path] = self._today_tokens_per_file.get(path, 0) + out
            self._today_cost_per_file[path] = self._today_cost_per_file.get(path, 0.0) + cost

    async def _emit(self) -> None:
        cumulative = sum(self._tokens_per_file.values())
        today = sum(self._today_tokens_per_file.values())
        cost_cumulative = sum(self._cost_per_file.values())
        cost_today = sum(self._today_cost_per_file.values())
        # Entries aren't implemented via tailer yet — hook events feed them directly.
        # Keeping the signature for future expansion.
        await self.on_update(cumulative, today, cost_cumulative, cost_today, [])


def _today_key() -> str:
    return datetime.now().astimezone().strftime("%Y-%m-%d")


def _record_is_today(ts: Any, current_day: str) -> bool:
    """Parse an ISO 8601 timestamp (optionally Z-suffixed) and return True if
    it falls on the current local day. Claude Code writes timestamps in UTC
    with a 'Z' suffix; we convert to local time before comparing."""
    if not isinstance(ts, str) or not ts:
        return False
    try:
        # fromisoformat doesn't accept a trailing 'Z' until 3.11; normalize.
        dt = datetime.fromisoformat(ts.replace("Z", "+00:00"))
    except ValueError:
        return False
    return dt.astimezone().strftime("%Y-%m-%d") == current_day
