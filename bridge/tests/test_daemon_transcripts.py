"""The daemon's side of memory (owner, 2026-09-23): the one-time move before any store opens, one Memory
lent to both brains, the nightly dream, close markers and a drain at shutdown, and a bus "remember" that
becomes a transcript line and never a star. The doubles are test_daemon_explore's and test_daemon_membus's
stub daemons; nothing touches the owner's real memory folder."""

from __future__ import annotations

import asyncio
import inspect
import logging
import time
from datetime import timedelta
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from test_daemon_explore import _daemon as _voice_daemon
from test_daemon_membus import _daemon as _bus_daemon
from test_daemon_membus import _install_fakes

from eve_bridge import daemon as daemon_mod
from eve_bridge import mem0_memory, records, self_context, telegram, transcripts
from eve_bridge import memory as memory_mod
from eve_bridge import recall as recall_mod
from eve_bridge.daemon import Daemon
from eve_bridge.memory import Memory
from eve_bridge.recall import RecallConfig
from eve_bridge.transcripts import TranscriptConfig, Transcripts

MEMORY_ENV = ("EVE_MEMORY", "EVE_RECORDS")


def _cfg(tmp_path: Path) -> RecallConfig:
    return RecallConfig(store=tmp_path / "memory", notes=tmp_path / "notes")


def _memory(tmp_path: Path) -> Memory:
    cfg = _cfg(tmp_path)
    return Memory(cfg, Transcripts(TranscriptConfig(enabled=True, root=cfg.transcripts_dir),
                                   archive=cfg.archive_dir), None)


def _memory_off(monkeypatch: pytest.MonkeyPatch) -> None:
    for name in MEMORY_ENV:
        monkeypatch.delenv(name, raising=False)


# ---- the move, before anything opens a store ------------------------------------------------

def _record_opening(monkeypatch: pytest.MonkeyPatch, order: list[str]) -> None:
    real_transcripts = transcripts.Transcripts

    def migrate(cfg, legacy_store, legacy_mem0):
        order.append("migrate")
        return {}

    def ensure_repo(repo):
        order.append("ensure_repo")
        return True

    def make_transcripts(config, archive=None, **kw):
        order.append("transcripts")
        return real_transcripts(config, archive=archive, **kw)

    def shared(cfg, environ=None):
        order.append("mem0")
        return None

    monkeypatch.setattr(memory_mod, "migrate", migrate)
    monkeypatch.setattr(records, "ensure_repo", ensure_repo)
    monkeypatch.setattr(transcripts, "Transcripts", make_transcripts)
    monkeypatch.setattr(mem0_memory, "shared", shared)


def test_with_memory_on_the_move_runs_before_any_store_opens(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setenv("EVE_MEMORY", "1")
    monkeypatch.setenv("EVE_MEMORY_DIR", str(tmp_path / "memory"))
    order: list[str] = []
    _record_opening(monkeypatch, order)
    d = SimpleNamespace(_recall_cfg=_cfg(tmp_path))
    memory = Daemon._build_memory(d)
    assert isinstance(memory, Memory)
    assert order == ["migrate", "ensure_repo", "transcripts", "mem0"]
    assert memory.transcripts.enabled and memory.transcripts.archive == _cfg(tmp_path).archive_dir


def test_with_memory_off_nothing_moves_and_nothing_opens(tmp_path: Path, monkeypatch) -> None:
    _memory_off(monkeypatch)
    order: list[str] = []
    _record_opening(monkeypatch, order)
    d = SimpleNamespace(_recall_cfg=_cfg(tmp_path))
    assert Daemon._build_memory(d) is None
    assert order == []
    assert not (tmp_path / "memory").exists()


def test_the_real_move_puts_the_old_store_under_the_new_root(tmp_path: Path, monkeypatch) -> None:
    """End to end with the real migrate: the old records land in records/ as a sealed repository of its own,
    the old notes in transcripts/meetings, and everything else in the archive."""
    import shutil
    if shutil.which("git") is None:
        pytest.skip("git is not installed")
    legacy = tmp_path / "debrief"
    (legacy / "records").mkdir(parents=True)
    (legacy / "records" / "coffee.md").write_text("---\nid: coffee\ntype: preference\n---\n- flat white\n")
    (legacy / "notes" / "2026-09-20").mkdir(parents=True)
    (legacy / "notes" / "2026-09-20" / "1400-standup.md").write_text("# Standup\n")
    (legacy / "sessions" / "2026-09-20").mkdir(parents=True)
    (legacy / "sessions" / "2026-09-20" / "0900-a1.md").write_text("# A chat\n")
    monkeypatch.setenv("EVE_MEMORY", "1")
    monkeypatch.setenv("EVE_MEM0", "0")
    monkeypatch.setenv("EVE_MEMORY_DIR", str(tmp_path / "memory"))
    monkeypatch.setenv("EVE_DEBRIEF_DIR", str(legacy))
    # The old mem0 home is always ~/.config/eve-bridge/mem0: without a private HOME this test moved the
    # owner's real mem0 folder into tmp_path, where pytest later deleted it (2026-09-23; restored from backup).
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    assert recall_mod.legacy_mem0().is_relative_to(tmp_path)
    cfg = _cfg(tmp_path)
    memory = Daemon._build_memory(SimpleNamespace(_recall_cfg=cfg))
    assert memory is not None and memory.index is None
    assert (cfg.records_dir / "coffee.md").is_file() and (cfg.records_dir / ".git").is_dir()
    assert (cfg.transcripts_dir / "meetings" / "2026-09-20" / "1400-standup.md").is_file()
    assert (cfg.archive_dir / "sessions" / "2026-09-20" / "0900-a1.md").is_file()
    assert (cfg.store / memory_mod.MIGRATED_FILE).is_file()
    # the archive is searched: the old note is found by the same search the brains use
    hits = memory.transcripts.search("chat", days_back=3650)["hits"]
    assert [h["ch"] for h in hits] == ["note"]


def test_run_opens_memory_before_the_ipc_server(monkeypatch) -> None:
    """The IPC server can write meeting notes into the memory folder, so the move must come first."""
    order: list[str] = []

    class Stop(Exception):
        pass

    async def open_memory(self):
        order.append("memory")

    async def ipc_start():
        order.append("ipc")
        raise Stop

    monkeypatch.setattr(Daemon, "_open_memory", open_memory)
    monkeypatch.setattr(daemon_mod, "_log_permission_config_summary", lambda matchers: None)
    monkeypatch.setattr(daemon_mod.voice_agent, "warm_live_import", lambda: None)
    d = SimpleNamespace(matchers=None, ipc=SimpleNamespace(start=ipc_start))
    with pytest.raises(Stop):
        asyncio.run(Daemon.run(d))
    assert order == ["memory", "ipc"]


# ---- the dream, and the loops that are gone ---------------------------------------------------

def test_the_dream_loop_starts_in_the_background_with_memory_and_a_client(tmp_path: Path, monkeypatch) -> None:
    started: list[Any] = []

    class FakeDreamer:
        def __init__(self, cfg, transcripts_, client, index) -> None:
            started.append((cfg, transcripts_, client, index))

        async def loop(self, shutdown: asyncio.Event) -> None:
            await shutdown.wait()

    client = object()
    monkeypatch.setattr(daemon_mod.dream_mod, "Dreamer", FakeDreamer)
    monkeypatch.setattr(records, "make_client", lambda environ=None: client)

    async def go():
        memory = _memory(tmp_path)
        d = SimpleNamespace(_memory=memory, _recall_cfg=memory.cfg, _shutdown=asyncio.Event(), _background=set())
        task = Daemon._start_dream(d)
        assert task is not None and task in d._background and task.get_name() == "memory-dream"
        await asyncio.sleep(0)
        assert started == [(memory.cfg, memory.transcripts, client, None)]
        d._shutdown.set()
        await asyncio.wait_for(task, 1)
        assert not task.cancelled()                        # it ended by itself, on the shutdown event

        # no client (no key): no dream; memory off: no dream
        monkeypatch.setattr(records, "make_client", lambda environ=None: None)
        assert Daemon._start_dream(d) is None
        d._memory = None
        assert Daemon._start_dream(d) is None
        assert len(started) == 1
    asyncio.run(go())


def test_no_curate_reconcile_or_mem0_loop_is_left() -> None:
    source = inspect.getsource(Daemon.run)
    assert "Daemon._start_dream(self)" in source           # the control: the one loop that remains
    for gone in ("curate_loop", "make_reconciler", "records-reconcile", "mem0-ingest", "chat-memory"):
        assert gone not in inspect.getsource(daemon_mod), gone
    assert not hasattr(records, "make_reconciler") and not hasattr(records, "Reconciler")
    assert not hasattr(mem0_memory.OwnerMemory, "loop")


# ---- a voice conversation --------------------------------------------------------------------

class _Ears:
    def subscribe(self):
        return asyncio.Queue()

    def unsubscribe(self, q) -> None:
        pass


def test_converse_lends_the_memory_and_its_blocks_to_the_session(tmp_path: Path, monkeypatch) -> None:
    memory = _memory(tmp_path)
    t = memory.transcripts
    # The transcript day starts at DAY_START_HOUR (04:00). Between 04:00 and 04:20, "20 minutes ago" is
    # yesterday's day and this test failed by the clock (2026-09-24, 04:15): start the day 12 hours from now.
    monkeypatch.setattr(transcripts, "DAY_START_HOUR", (t.now().hour + 12) % 24)
    yesterday = t.now() - timedelta(days=1)
    assert t.append("telegram", "t-1726000000000-abcd", "owner", "say", "the plant needs water", now=yesterday)
    assert t.append("telegram", "t-1726000000001-abcd", "owner", "say", "the kettle is new",
                    now=t.now() - timedelta(minutes=20))
    seen: dict[str, Any] = {}

    async def fake_open_session(mic, on_state, agent_factory, **kw):
        seen.update(kw)

    monkeypatch.setattr(daemon_mod.voice_agent, "open_session", fake_open_session)

    async def go():
        d = _voice_daemon()
        d._ears, d._memory, d._recall_cfg = _Ears(), memory, memory.cfg
        await d._converse()
        return d
    d = asyncio.run(go())
    assert seen["memory"] is memory and d._voice_conv == seen["conv"]
    assert seen["brief"].startswith("You last talked")
    assert seen["conv"].startswith("v-")
    assert "kettle" in seen["today"] and "kettle" in seen["backend_today"]
    assert "plant" not in seen["today"]                    # yesterday is not today
    assert len(seen["today"]) <= transcripts.VOICE_CHARS
    for gone in ("profile", "on_star", "on_closed"):
        assert gone not in seen, gone


def test_a_slow_today_build_opens_the_conversation_without_it(tmp_path: Path, monkeypatch) -> None:
    memory = _memory(tmp_path)
    assert memory.transcripts.append("telegram", "t-1726000000001-abcd", "owner", "say", "the kettle is new")
    real_today = memory.today

    def slow_today(*a, **kw):
        time.sleep(0.3)
        return real_today(*a, **kw)

    memory.today = slow_today
    seen: dict[str, Any] = {}

    async def fake_open_session(mic, on_state, agent_factory, **kw):
        seen.update(kw)

    monkeypatch.setattr(daemon_mod.voice_agent, "open_session", fake_open_session)

    async def go():
        d = _voice_daemon()
        d._ears, d._memory, d._recall_cfg = _Ears(), memory, memory.cfg
        started = time.monotonic()
        await d._converse()
        return time.monotonic() - started
    took = asyncio.run(go())
    assert seen["memory"] is memory and seen["conv"].startswith("v-")
    assert seen["today"] == "" and seen["backend_today"] == ""
    assert took < 0.25                                     # it did not wait for the slow build


def test_with_memory_off_the_brains_get_nothing_and_prompts_are_unchanged(monkeypatch) -> None:
    seen: dict[str, Any] = {}

    async def fake_open_session(mic, on_state, agent_factory, **kw):
        seen.update(kw)

    monkeypatch.setattr(daemon_mod.voice_agent, "open_session", fake_open_session)

    async def go():
        d = _voice_daemon()
        d._ears = _Ears()
        await d._converse()
    asyncio.run(go())
    assert seen["memory"] is None
    assert (seen["brief"], seen["conv"], seen["today"], seen["backend_today"]) == ("", "", "", "")
    plain = daemon_mod.voice_agent.session_config(daemon_mod.voice_agent.VoiceConfig())
    assert daemon_mod.voice_agent.session_config(daemon_mod.voice_agent.VoiceConfig(), seen["brief"],
                                                 today=seen["today"], backend_today=seen["backend_today"]) == plain

    # Telegram: the inlet is lent memory=None and an empty brief, so its request is the memory-less one
    for name, value in (("EVE_TELEGRAM", "1"), ("EVE_TELEGRAM_TOKEN", "123456:AAsecretTOKENvalue"),
                        ("EVE_TELEGRAM_OWNER", "4242"), ("OPENROUTER_API_KEY", "sk-test")):
        monkeypatch.setenv(name, value)
    stub = SimpleNamespace(_make_agent=lambda *a: None, _agent_cfg=SimpleNamespace(enabled=True),
                           _recall_cfg=None, _memory=None, _photo_for_owner=None, _thinker=None, _scene=None,
                           _head=None, _on_agent_state=lambda s: None, _request_explore=None,
                           _set_sound=lambda on: None, _on_caption=lambda m: None, _room_notes_taker=lambda: None,
                           state=None)
    inlet = Daemon._make_telegram(stub)
    try:
        assert inlet is not None and inlet._memory is None and inlet._brief() == ""
        body = telegram.request(inlet.config, [], inlet._brief())
        # The instructions are the generic ones plus what eve runs on (self_context.py, 2026-09-29: not memory,
        # so it rides whether memory is on or off), and nothing after it: no profile, no memory hint, no today.
        about = inlet.config.about
        assert about.startswith(self_context.HEADER) and about.endswith(self_context.RULE)
        assert body["instructions"] == telegram.INSTRUCTIONS + about
        for memory_part in (telegram.PROFILE_HEADER, telegram.MEMORY_HINT, telegram.TODAY_HEADER):
            assert memory_part not in body["instructions"]
        assert not any(t.get("name") == "memory_search" for t in body["tools"])
    finally:
        asyncio.run(inlet.api.close())


def test_with_memory_on_the_inlet_gets_the_same_memory(tmp_path: Path, monkeypatch) -> None:
    for name, value in (("EVE_TELEGRAM", "1"), ("EVE_TELEGRAM_TOKEN", "123456:AAsecretTOKENvalue"),
                        ("EVE_TELEGRAM_OWNER", "4242"), ("OPENROUTER_API_KEY", "sk-test")):
        monkeypatch.setenv(name, value)
    memory = _memory(tmp_path)
    assert memory.transcripts.append("voice", "v-1726000000001-abcd", "owner", "say", "hello",
                                     now=memory.transcripts.now() - timedelta(days=2))
    stub = SimpleNamespace(_make_agent=lambda *a: None, _agent_cfg=SimpleNamespace(enabled=True),
                           _recall_cfg=memory.cfg, _memory=memory, _photo_for_owner=None, _thinker=None,
                           _scene=None, _head=None, _on_agent_state=lambda s: None, _request_explore=None,
                           _set_sound=lambda on: None, _on_caption=lambda m: None, _room_notes_taker=lambda: None,
                           state=None)
    inlet = Daemon._make_telegram(stub)
    try:
        assert inlet is not None and inlet._memory is memory
        assert inlet._brief() == "You last talked 2 days ago."
    finally:
        asyncio.run(inlet.api.close())


# ---- shutdown ----------------------------------------------------------------------------------

def test_shutdown_closes_open_conversations_once(tmp_path: Path) -> None:
    memory = _memory(tmp_path)
    t = memory.transcripts
    voice, tg, done, silent = (t.new_conv("voice"), t.new_conv("telegram"), t.new_conv("voice"),
                               t.new_conv("telegram"))
    assert t.append("voice", voice, "owner", "say", "one")
    assert t.append("telegram", tg, "owner", "say", "two")
    assert t.append("voice", done, "owner", "say", "three") and t.close("voice", done)
    inlet = SimpleNamespace(_conv=tg)

    async def go(voice_conv: str, inlet_: Any):
        d = SimpleNamespace(_memory=memory, _telegram=inlet_, _voice_conv=voice_conv)
        await Daemon._close_open_conversations(d)
        return d

    d = asyncio.run(go(voice, inlet))
    closes = lambda conv: sum(1 for ln in t.conv_lines(conv) if ln["kind"] == "close")  # noqa: E731
    assert closes(voice) == 1 and closes(tg) == 1
    assert inlet._conv is None and d._voice_conv == ""
    asyncio.run(go(done, SimpleNamespace(_conv=silent)))  # already closed, and one that wrote nothing
    assert closes(done) == 1 and t.conv_lines(silent) == []
    asyncio.run(go(voice, None))                           # a second shutdown pass closes nothing twice
    assert closes(voice) == 1


def test_shutdown_with_memory_off_writes_nothing(tmp_path: Path) -> None:
    d = SimpleNamespace(_memory=None, _telegram=SimpleNamespace(_conv="t-1"), _voice_conv="v-1")
    asyncio.run(Daemon._close_open_conversations(d))
    assert list(tmp_path.iterdir()) == []


def test_shutdown_awaits_background_work_then_cuts_off_the_rest(monkeypatch) -> None:
    monkeypatch.setattr(daemon_mod, "DRAIN_SECS", 0.2)
    finished: list[str] = []

    async def quick():
        await asyncio.sleep(0.05)
        finished.append("quick")

    async def stuck():
        await asyncio.sleep(30)
        finished.append("stuck")

    async def go():
        d = SimpleNamespace(_background=set())
        tasks = [asyncio.create_task(quick()), asyncio.create_task(stuck())]
        d._background.update(tasks)
        started = time.monotonic()
        await Daemon._drain_background(d)
        return tasks, time.monotonic() - started
    tasks, took = asyncio.run(go())
    assert finished == ["quick"]
    assert tasks[0].done() and not tasks[0].cancelled()
    assert tasks[1].cancelled()
    assert took < 1.0


def test_the_drain_budget_is_twelve_seconds() -> None:
    assert daemon_mod.DRAIN_SECS == 12.0 and daemon_mod.TODAY_BUILD_SECS == 0.05


# ---- a remember on the bus -------------------------------------------------------------------

def test_a_bus_remember_is_a_transcript_line_and_never_a_star(tmp_path: Path, monkeypatch, caplog) -> None:
    _install_fakes(monkeypatch)
    memory = _memory(tmp_path)

    async def go():
        d = _bus_daemon()
        d._memory = memory
        await d._start_memory_sinks()
        with caplog.at_level(logging.INFO, logger="eve_bridge.daemon"):
            d.bus.publish("/eve/memory/remember", {"text": "The green mug lives on the left shelf.",
                                                     "title": "Mug"})
            await asyncio.sleep(0)
            d.bus.publish("/eve/memory/remember", {"text": "   "})     # nothing to keep
            await asyncio.sleep(0)
    asyncio.run(go())
    t = memory.transcripts
    lines = [ln for day in t.days() for ln in t.lines(day)]
    said = [ln for ln in lines if ln["kind"] != "close"]
    assert len(said) == 1
    line = said[0]
    assert (line["ch"], line["who"], line["kind"], line.get("tool")) == ("bus", "system", "tool", "remember")
    assert line["text"] == "Mug: The green mug lives on the left shelf."
    assert sum(1 for ln in lines if ln["kind"] == "close" and ln["conv"] == line["conv"]) == 1
    assert not (memory.cfg.records_dir / records.STARRED_FILE).exists()       # never a star
    assert records.stars(memory.cfg) == []
    # for the dream to judge, never the owner talking: not in today's block, not the last talk
    assert "left shelf" in t.day_text(t.day_of(t.now()))
    assert "(sent)" in transcripts.render_line(line)
    assert t.today_block(10000) == "" and Transcripts(t.config).last_turn_at() is None
    assert "left shelf" not in caplog.text                 # logs never carry the words
    assert "a transcript line" in caplog.text


def test_the_bus_channel_is_known_to_the_transcripts() -> None:
    assert transcripts.BUS == "bus" and transcripts.BUS in transcripts.CHANNELS
    assert transcripts.TALK == ("voice", "telegram")


def test_recent_conversation_never_shows_a_bus_line(tmp_path: Path) -> None:
    memory = _memory(tmp_path)
    t = memory.transcripts
    since = t.now() - timedelta(minutes=5)
    assert t.append("telegram", t.new_conv("telegram"), "owner", "say", "typed on the phone")
    assert t.append("bus", t.new_conv("bus"), "system", "tool", "sent over the bus", tool="remember")
    rows = [ln["text"] for ln in t.lines_since(since, exclude_ch="voice")]
    assert rows == ["typed on the phone"]                   # the control is there; the bus line is not
    assert [h["ch"] for h in t.search("bus")["hits"]] == ["bus"]     # but search still finds it
