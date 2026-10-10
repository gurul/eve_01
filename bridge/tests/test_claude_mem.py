"""claude_mem.py: worker discovery, event -> memory mapping, the non-blocking sink, recall parsing, the
SSE mirror, and one live probe against the real worker when it is up."""

from __future__ import annotations

import contextlib
import json
import logging
import socket
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

from eve_bridge import claude_mem
from eve_bridge.claude_mem import ClaudeMemMirror, ClaudeMemSink, recall, to_memory, worker_url
from eve_bridge.memory_bus import MemoryBus

# A real /api/search body, captured from the worker on 2026-09-15 (v13.24.23).
REAL_SEARCH_BODY = {"content": [{"type": "text", "text": (
    "Found 2 result(s) matching \"eve probe\" (2 obs, 0 sessions, 0 prompts)\n\n### Sep 15, 2026\n\n"
    "**General**\n| ID | Time | T | Title | Read |\n|----|------|---|-------|------|\n"
    "| #7 | 12:40 PM | ○ | eve saw: the chair is pushed in | ~22 |\n"
    "| #2 | 12:06 PM | ○ | eve probe | ~22 |\n")}]}
REAL_OBSERVATION_2 = {"id": 2, "project": "eve", "text": None, "type": "discovery", "title": "eve probe",
                      "subtitle": "Manual memory", "narrative": "probe: eve realtime memory bus wiring test",
                      "created_at": "2026-09-15T19:06:26.217Z", "created_at_epoch": 1789499186217}


class FakeWorker:
    """A stand-in for the claude-mem worker: records requests, serves canned answers."""

    def __init__(self, sse_events: list[dict] | None = None, sse_hold: float = 0.0) -> None:
        self.requests: list[tuple[str, str, dict | None]] = []
        self.sse_events = sse_events or []
        self.sse_hold = sse_hold
        self.closing = threading.Event()        # set on exit so a held /stream handler lets go at once
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):  # noqa: D102
                pass

            def _json(self, code, body):
                data = json.dumps(body).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_POST(self):
                length = int(self.headers.get("Content-Length") or 0)
                body = json.loads(self.rfile.read(length) or b"{}")
                outer.requests.append(("POST", self.path, body))
                self._json(200, {"success": True, "id": len(outer.requests), "title": body.get("title")})

            def do_GET(self):
                outer.requests.append(("GET", self.path, None))
                if self.path.startswith("/api/search?"):
                    self._json(200, REAL_SEARCH_BODY)
                elif self.path.startswith("/api/observation/"):
                    ident = int(self.path.rsplit("/", 1)[1])
                    self._json(200, dict(REAL_OBSERVATION_2, id=ident, title=f"title {ident}", narrative=f"body {ident}"))
                elif self.path == "/stream":
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.end_headers()
                    for event in outer.sse_events:
                        self.wfile.write(f"data: {json.dumps(event)}\n\n".encode())
                        self.wfile.flush()
                    outer.closing.wait(outer.sse_hold)      # hold the stream open, but never past exit
                else:
                    self._json(404, {"error": "no"})

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        self._thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self):
        self._thread.start()
        return self

    def __exit__(self, *exc):
        self.closing.set()
        self.server.shutdown()
        self.server.server_close()

    def posts(self):
        return [b for m, p, b in self.requests if m == "POST" and p == "/api/memory/save"]


def _closed_port_url() -> str:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return f"http://127.0.0.1:{port}"


def _wait(predicate, timeout=3.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.01)
    return predicate()


# ---- worker_url ------------------------------------------------------------------------------

def test_worker_url_precedence_env_settings_pid_default(tmp_path) -> None:
    assert worker_url({}, home=tmp_path) == "http://127.0.0.1:37777"
    (tmp_path / ".claude-mem").mkdir()
    (tmp_path / ".claude-mem" / "worker.pid").write_text(json.dumps({"pid": 1, "port": 37701}))
    assert worker_url({}, home=tmp_path) == "http://127.0.0.1:37701"
    (tmp_path / ".claude-mem" / "settings.json").write_text(json.dumps({"CLAUDE_MEM_WORKER_PORT": "37702"}))
    assert worker_url({}, home=tmp_path) == "http://127.0.0.1:37702"
    assert worker_url({"EVE_CLAUDE_MEM_URL": "http://127.0.0.1:4000/"}, home=tmp_path) == "http://127.0.0.1:4000"
    (tmp_path / ".claude-mem" / "settings.json").write_text("not json")
    assert worker_url({}, home=tmp_path) == "http://127.0.0.1:37701"      # falls through to the pid file


# ---- to_memory -----------------------------------------------------------------------------

def test_to_memory_shapes_each_topic() -> None:
    obs = to_memory("/eve/memory/observation",
                    {"thought": "The chair is pushed in again.", "observations": ["chair in", "lamp on"],
                     "importance": 7, "time": 1.5})
    assert obs["title"] == "eve saw: The chair is pushed in again."
    assert obs["text"] == "The chair is pushed in again.\n\nSeen: chair in; lamp on"
    assert obs["project"] == "eve"
    assert obs["metadata"]["platformSource"] == "eve" and obs["metadata"]["topic"] == "/eve/memory/observation"
    assert obs["metadata"]["importance"] == 7 and obs["metadata"]["time"] == 1.5
    assert "thought" not in obs["metadata"] and "observations" not in obs["metadata"]

    # What was said is not on the bus any more (owner, 2026-09-23): a conversation topic is dropped.
    assert to_memory("/eve/memory/conversation",
                     {"title": "Coffee plans", "note": "The owner wants coffee at four."}) is None

    lesson = to_memory("/eve/memory/lesson",
                       {"action": "step", "stage": "working", "topic": "Fractions", "level": "Grade 4",
                        "feedback": "Add the tops, keep the bottom."})
    assert lesson["title"] == "lesson: Fractions"
    assert lesson["text"] == "step on Fractions (Grade 4): Add the tops, keep the bottom."
    assert lesson["metadata"]["stage"] == "working" and lesson["metadata"]["subject"] == "Fractions"
    assert lesson["metadata"]["topic"] == "/eve/memory/lesson"                 # the bus topic, never overwritten

    keep = to_memory("/eve/memory/remember", {"text": "The blue mug is Sam's.", "title": ""})
    assert keep["title"] == "The blue mug is Sam's." and keep["text"] == "The blue mug is Sam's."
    assert to_memory("/eve/memory/remember", {"text": "x" * 100})["title"] == "x" * 60


@pytest.mark.parametrize("topic,msg", [
    ("/eve/state", {"state": "wake"}),
    ("/claude/observation", {"title": "t", "text": "x"}),
    ("/eve/memory/lesson", {"action": "hint", "topic": "Fractions", "feedback": "Try the tops."}),
    ("/eve/memory/lesson", {"action": "open"}),
    ("/eve/memory/lesson", {"action": "listen"}),
    ("/eve/memory/lesson", {"action": "status"}),
    ("/eve/memory/observation", {"thought": "", "observations": []}),
    ("/eve/memory/conversation", {"note": "   "}),
    ("/eve/memory/remember", {"text": ""}),
    ("/eve/memory/remember", "not a dict"),
])
def test_to_memory_returns_none_for_noise(topic, msg) -> None:
    assert to_memory(topic, msg) is None


def test_to_memory_never_carries_learner_words_even_when_offered() -> None:
    leaked = {"action": "check", "topic": "Fractions", "level": "Grade 4", "feedback": "Look at the 3.",
              "ideas": "I think I add the tops", "spoken": ["um, four?"]}
    body = to_memory("/eve/memory/lesson", leaked)
    flat = json.dumps(body)
    assert "ideas" not in body["metadata"] and "spoken" not in body["metadata"]
    assert "add the tops" not in flat and "um, four" not in flat
    assert body["text"] == "check on Fractions (Grade 4): Look at the 3."


def test_to_memory_clips_long_strings_and_lists() -> None:
    body = to_memory("/eve/memory/observation", {"thought": "t" * 5000, "observations": [str(i) for i in range(50)],
                                                     "tags": ["a"] * 30, "note": "n" * 3000})
    assert len(body["text"]) <= 4000 and body["text"].count(";") == 19            # thought clipped, 20 seen kept
    assert "tags" not in body["metadata"]                                         # list too long: dropped
    assert "note" not in body["metadata"]                                         # not on the allowlist
    assert "thought" not in body["metadata"] and "observations" not in body["metadata"]


# ---- the sink -----------------------------------------------------------------------------

def test_sink_posts_each_memory_with_project_and_source() -> None:
    with FakeWorker() as worker:
        bus = MemoryBus()
        sink = ClaudeMemSink(bus, url=worker.url)
        sink.start()
        bus.publish("/eve/memory/observation", {"thought": "Lamp on."})
        bus.publish("/eve/memory/lesson", {"action": "start", "topic": "Fractions", "level": "Grade 4"})
        bus.publish("/eve/memory/remember", {"text": "Keep this."})
        bus.publish("/eve/state", {"state": "wake"})                            # not a memory
        bus.publish("/eve/memory/lesson", {"action": "hint", "feedback": "nudge"})  # not a memory
        assert _wait(lambda: sink.stats["saved"] == 3)
        sink.stop()
    posts = worker.posts()
    assert len(posts) == 3
    assert all(p["project"] == "eve" and p["metadata"]["platformSource"] == "eve" for p in posts)
    assert [p["title"] for p in posts] == ["eve saw: Lamp on.", "lesson: Fractions", "Keep this."]
    assert sink.stats == {"saved": 3, "dropped": 0, "failed": 0}


def test_sink_with_the_worker_down_never_blocks_and_warns_once(caplog) -> None:
    caplog.set_level(logging.WARNING, logger="eve_bridge.claude_mem")
    bus = MemoryBus()
    sink = ClaudeMemSink(bus, url=_closed_port_url(), timeout=0.5)
    sink.start()
    for i in range(5):
        started = time.perf_counter()
        bus.publish("/eve/memory/remember", {"text": f"line {i}"})
        assert time.perf_counter() - started < 0.1
    assert _wait(lambda: sink.stats["failed"] == 5, timeout=8.0)
    sink.stop()
    warnings = [r for r in caplog.records if r.levelno == logging.WARNING and "claude-mem" in r.getMessage()]
    assert len(warnings) == 1 and "could not save" in warnings[0].getMessage()
    assert sink.stats["saved"] == 0


def test_sink_queue_overflow_drops_and_counts() -> None:
    bus = MemoryBus()
    sink = ClaudeMemSink(bus, url=_closed_port_url(), queue_size=2)
    sink._sub = bus.subscribe("/eve/memory/*", sink._on_event)      # subscribe without the drain thread
    for i in range(5):
        bus.publish("/eve/memory/remember", {"text": f"line {i}"})
    assert sink.stats["dropped"] == 3 and sink._queue.qsize() == 2
    bus.unsubscribe(sink._sub)


def test_sink_start_and_stop_are_idempotent() -> None:
    with FakeWorker() as worker:
        sink = ClaudeMemSink(MemoryBus(), url=worker.url)
        sink.start()
        sink.start()
        sink.stop()
        sink.stop()


# ---- recall -------------------------------------------------------------------------------

def test_recall_parses_the_search_table_and_fetches_bodies() -> None:
    with FakeWorker() as worker:
        results = recall("eve probe", limit=5, url=worker.url)
    assert [r["id"] for r in results] == [7, 2]                              # newest first, as the table lists them
    assert results[0]["title"] == "title 7" and results[0]["text"] == "body 7"
    assert results[1]["time"] == "2026-09-15T19:06:26.217Z"
    search = [p for m, p, _ in worker.requests if p.startswith("/api/search?")][0]
    assert "query=eve+probe" in search and "project=eve" in search and "limit=5" in search


def test_recall_returns_an_empty_list_when_the_worker_is_down() -> None:
    assert recall("anything", url=_closed_port_url(), timeout=0.5) == []


def test_recall_clamps_limit() -> None:
    with FakeWorker() as worker:
        assert len(recall("eve probe", limit=1, url=worker.url)) == 1
        assert "limit=20" in [p for m, p, _ in worker.requests if p.startswith("/api/search?")][-1] or \
               "limit=1" in [p for m, p, _ in worker.requests if p.startswith("/api/search?")][-1]


# ---- the mirror ---------------------------------------------------------------------------

def _obs(ident: int, project: str, title: str) -> dict:
    return {"type": "new_observation", "observation": {
        "id": ident, "memory_session_id": None, "session_id": "s", "platform_source": "claude",
        "type": "decision", "title": title, "subtitle": None, "text": None, "narrative": f"narrative {ident}",
        "facts": "[]", "concepts": "[]", "files_read": "[]", "files_modified": "[]", "project": project,
        "prompt_number": 1, "created_at_epoch": 1789499186217}}


def test_mirror_republishes_other_projects_and_skips_eve() -> None:
    events = [{"type": "connected", "timestamp": 1}, {"type": "processing_status", "isProcessing": True},
              _obs(11, "debrief", "Fixed the tailer"), _obs(12, "eve", "eve saw: lamp"),
              {"type": "new_summary", "summary": {"id": 3, "project": "debrief"}}]
    with FakeWorker(sse_events=events, sse_hold=30.0) as worker:
        bus = MemoryBus()
        seen: list[tuple[str, dict]] = []
        bus.subscribe("/claude/observation", lambda t, m: seen.append((t, m)))
        mirror = ClaudeMemMirror(bus, url=worker.url, reconnect_secs=0.2)
        mirror.start()
        assert _wait(lambda: len(seen) == 1)
        time.sleep(0.2)
        started = time.perf_counter()
        mirror.stop(wait=3.0)
        assert time.perf_counter() - started < 3.0
    assert seen == [("/claude/observation", {"id": 11, "title": "Fixed the tailer", "project": "debrief",
                                             "type": "decision", "time": 1789499186217, "text": "narrative 11"})]
    assert mirror.stats["mirrored"] == 1


def test_mirror_reconnects_after_the_stream_ends_and_stops_cleanly() -> None:
    with FakeWorker(sse_events=[_obs(1, "debrief", "one")], sse_hold=0.0) as worker:
        bus = MemoryBus()
        count = {"n": 0}
        bus.subscribe("/claude/observation", lambda t, m: count.__setitem__("n", count["n"] + 1))
        mirror = ClaudeMemMirror(bus, url=worker.url, reconnect_secs=0.05)
        mirror.start()
        assert _wait(lambda: mirror.stats["reconnects"] >= 2)
        mirror.stop(wait=3.0)
    assert count["n"] >= 2


def test_a_quiet_stream_reconnects_and_keeps_mirroring_without_spinning() -> None:
    # The worker sends one observation and then says nothing. A timed-out read can never be retried (Python
    # marks the socket), and the old loop spun on it at full speed forever and never read again (2026-09-24).
    with FakeWorker(sse_events=[_obs(1, "debrief", "one")], sse_hold=30.0) as worker:
        bus = MemoryBus()
        count = {"n": 0}
        bus.subscribe("/claude/observation", lambda t, m: count.__setitem__("n", count["n"] + 1))
        mirror = ClaudeMemMirror(bus, url=worker.url, reconnect_secs=5.0, read_timeout=0.3)
        mirror.start()
        assert _wait(lambda: count["n"] >= 1)
        cpu = time.process_time()
        time.sleep(1.0)                                   # quiet: at least two reconnects, each one mirrored
        busy = time.process_time() - cpu
        started = time.perf_counter()
        mirror.stop(wait=3.0)
        stopped_in = time.perf_counter() - started
    assert count["n"] >= 3                                # it kept reading after the quiet spells
    assert mirror.stats["reconnects"] == 0                # a quiet stream is not a lost one
    assert busy < 0.5                                     # the old loop burned the whole second
    assert stopped_in < 1.0


def test_stop_ends_a_blocked_read_at_once() -> None:
    with FakeWorker(sse_events=[_obs(1, "debrief", "one")], sse_hold=30.0) as worker:
        bus = MemoryBus()
        seen: list = []
        bus.subscribe("/claude/observation", lambda t, m: seen.append(m))
        mirror = ClaudeMemMirror(bus, url=worker.url)      # the real 30 s quiet window
        mirror.start()
        assert _wait(lambda: len(seen) == 1)
        started = time.perf_counter()
        mirror.stop(wait=3.0)
        assert time.perf_counter() - started < 1.0


def test_mirror_survives_a_dead_worker(caplog) -> None:
    caplog.set_level(logging.WARNING, logger="eve_bridge.claude_mem")
    mirror = ClaudeMemMirror(MemoryBus(), url=_closed_port_url(), reconnect_secs=0.05)
    mirror.start()
    assert _wait(lambda: mirror.stats["reconnects"] >= 3)
    mirror.stop(wait=3.0)
    assert len([r for r in caplog.records if "mirror lost" in r.getMessage()]) == 1


# ---- live ---------------------------------------------------------------------------------

def _worker_up(url: str) -> bool:
    """True when the worker answers /health with status ok."""
    try:
        with urllib.request.urlopen(url.rstrip("/") + "/health", timeout=2.0) as resp:
            return json.loads(resp.read()).get("status") == "ok"
    except Exception:  # noqa: BLE001
        return False


@pytest.mark.live
def test_live_save_then_recall_by_title() -> None:
    LIVE_URL = worker_url()                              # resolved only when the live test is asked for
    if not _worker_up(LIVE_URL):
        pytest.skip("claude-mem worker is not running")
    stamp = time.strftime("%Y%m%d-%H%M%S")
    title = f"eve live probe {stamp}"
    bus = MemoryBus()
    sink = ClaudeMemSink(bus, url=LIVE_URL)
    sink.start()
    bus.publish("/eve/memory/remember", {"text": f"live probe from test_claude_mem at {stamp}; safe to delete",
                                           "title": title})
    assert _wait(lambda: sink.stats["saved"] == 1, timeout=5.0), sink.stats
    sink.stop()
    found = _wait(lambda: any(r["title"] == title for r in recall(title, limit=5, url=LIVE_URL)), timeout=5.0)
    hits = [r for r in recall(title, limit=5, url=LIVE_URL) if r["title"] == title]
    print(f"\nLIVE {LIVE_URL}: saved {sink.stats}, recall found {len(hits)}: {hits[:1]}")
    try:
        assert found and hits[0]["text"].startswith("live probe from test_claude_mem")
    finally:
        # Leave nothing behind in the owner's memory: every run would otherwise add one probe.
        for hit in hits:
            request = urllib.request.Request(f"{LIVE_URL}/api/observation/{hit['id']}", method="DELETE")
            with contextlib.suppress(OSError):
                urllib.request.urlopen(request, timeout=5).close()


def test_module_never_names_the_learner_word_fields_in_payload_code() -> None:
    """Privacy line: the words 'ideas' and 'spoken' stay in the lesson store, never in a payload builder."""
    import inspect
    source = inspect.getsource(claude_mem)
    code_only = "\n".join(line for line in source.splitlines()
                          if not line.strip().startswith("#") and '"""' not in line)
    body = code_only.split("def _clip", 1)[1]                                 # everything after the docstring
    assert '"ideas"' not in body and "'ideas'" not in body and "spoken" not in body
