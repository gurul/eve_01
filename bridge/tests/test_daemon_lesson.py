"""The daemon side of `eve-bridge lesson <action>`: the IPC handler that runs the lesson
voice tool's path and has the robot act it out, and the CLI that sends it. No board, no mic, no
network, no browser: the SimpleNamespace + MethodType stub test_daemon_explore.py uses, and a fake
LearningApp.voice."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from types import MethodType, SimpleNamespace

import pytest

from eve_bridge import cli
from eve_bridge.caption_pager import CaptionPager
from eve_bridge.daemon import Daemon
from eve_bridge.explore import ExploreConfig, Explorer
from eve_bridge.learning import LESSON_ACTIONS
from eve_bridge.thought_screen import ThoughtScreen

MODE_OFF = {"cmd": "mode", "explore": False}
THINKING = {"cmd": "agent", "state": "thinking"}


class _Ble:
    def __init__(self, connected: bool = True) -> None:
        self.connected = connected
        self.sent: list[dict] = []

    async def send(self, obj: dict, codec=None) -> bool:
        self.sent.append(obj)
        return True


class _Talking:
    def done(self) -> bool:
        return False


def _cfg() -> ExploreConfig:
    return ExploreConfig(enabled=True, after_secs=600.0, notes_per_hour=0.0, model="fake",
                         notes_dir=Path("/nonexistent"), cycle_wait_secs=900.0)


def _daemon(connected: bool = True, learning=None, conversation=None) -> SimpleNamespace:
    d = SimpleNamespace(
        ble=_Ble(connected),
        state=SimpleNamespace(running_count=0, waiting_count=0),
        _listen_sent=None,
        _listen_down=False,
        _explore_cfg=_cfg(),
        _last_activity_at=0.0,
        _explore_raw_frame=None,
        _conversation=conversation,
        _agent_state="idle",
        _sound=SimpleNamespace(on=True, muted=False),
        _notes=None,
        _learning_idle_handle=None,
        _learning_notices=0,
        _learning_server=None if learning is None else SimpleNamespace(app=SimpleNamespace(voice=learning)),
    )
    d._explorer = Explorer(d._explore_cfg, now=0.0, notes_enabled=False)
    d._thought_pager = CaptionPager()
    d._screen = ThoughtScreen()
    for name in ("_handle_ipc", "_handle_lesson", "_learning_notice", "_learning_idle", "_on_agent_state",
                 "_on_caption", "_note_activity", "_dismiss_explore", "_clear_thought", "_run_explore_action",
                 "_request_explore"):
        setattr(d, name, MethodType(getattr(Daemon, name), d))
    return d


def _captions(d) -> list[dict]:
    return [m for m in d.ble.sent if m.get("cmd") == "caption" and not m.get("clear")]


def _states(d) -> list[str]:
    return [m["state"] for m in d.ble.sent if m.get("cmd") == "agent"]


async def _settle() -> None:
    await asyncio.sleep(0)       # _on_caption and _on_agent_state send on tasks of their own
    await asyncio.sleep(0)


def _voice(calls: list, answer: str = "Subtract three from both sides to get the x part alone."):
    def fake(**kw):
        calls.append(kw)
        return {"ok": True, "answer": answer}
    return fake


def _cancel_idle(d) -> None:
    if d._learning_idle_handle is not None:
        d._learning_idle_handle.cancel()


# ---- IPC -------------------------------------------------------------------------------------

def test_step_thinks_then_shows_one_caption_that_fits_the_screen() -> None:
    async def go():
        calls: list = []
        d = _daemon(learning=_voice(calls))
        resp = await d._handle_ipc({"evt": "lesson", "action": "step"})
        await _settle()
        assert resp["ok"] and resp["action"] == "step" and resp["answer"]
        assert calls == [{"action": "step"}]
        assert THINKING in d.ble.sent
        caps = _captions(d)
        assert len(caps) == 1
        assert d.ble.sent.index(THINKING) < d.ble.sent.index(caps[0])
        assert 1 <= len(caps[0]["lines"]) <= 4 and all(len(line) <= 17 for line in caps[0]["lines"])
        assert _states(d)[-1] == "speaking"
        json.dumps(resp)
        _cancel_idle(d)
    asyncio.run(go())


def test_a_dispatch_notice_is_the_only_caption() -> None:
    async def go():
        calls: list = []
        d = _daemon()
        loop = asyncio.get_running_loop()

        def fake(**kw):
            calls.append(kw)
            loop.call_soon_threadsafe(d._learning_notice, "Well done, that is the answer.", "complete")
            return {"ok": True, "answer": "Well done, that is the answer."}
        d._learning_server = SimpleNamespace(app=SimpleNamespace(voice=fake))
        resp = await d._handle_ipc({"evt": "lesson", "action": "check"})
        await _settle()
        assert resp["ok"]
        assert len(_captions(d)) == 1
        assert "done" in _states(d)
        _cancel_idle(d)
    asyncio.run(go())


def test_open_speaks_without_thinking() -> None:
    async def go():
        calls: list = []
        d = _daemon(learning=_voice(calls, "Would you like to learn a topic?"))
        resp = await d._handle_ipc({"evt": "lesson", "action": "open"})
        await _settle()
        assert resp["ok"] and calls == [{"action": "open"}]
        assert len(_captions(d)) == 1
        assert "thinking" not in _states(d) and _states(d)[-1] == "speaking"
        _cancel_idle(d)
    asyncio.run(go())


def test_start_options_are_forwarded_and_unknown_keys_dropped() -> None:
    async def go():
        calls: list = []
        d = _daemon(learning=_voice(calls, "What is 3/4 + 1/4?"))
        resp = await d._handle_ipc({"evt": "lesson", "action": "start", "mode": "learn", "topic": "Fractions",
                                    "level": "Grade 4", "extra": "x"})
        await _settle()
        assert resp["ok"]
        assert calls == [{"action": "start", "mode": "learn", "topic": "Fractions", "level": "Grade 4"}]
        _cancel_idle(d)
    asyncio.run(go())


def test_unknown_action_and_bad_mode_are_refused_before_anything_happens() -> None:
    async def go():
        calls: list = []
        d = _daemon(learning=_voice(calls))
        resp = await d._handle_ipc({"evt": "lesson", "action": "dance"})
        assert resp["ok"] is False and "Unknown lesson action" in resp["error"]
        resp = await d._handle_ipc({"evt": "lesson", "action": "start", "mode": "play"})
        assert resp["ok"] is False and "learn or help" in resp["error"]
        await _settle()
        assert calls == [] and d.ble.sent == []
    asyncio.run(go())


def test_learning_disabled_is_a_clear_error_without_a_caption() -> None:
    async def go():
        d = _daemon(learning=None)
        resp = await d._handle_ipc({"evt": "lesson", "action": "hint"})
        await _settle()
        assert resp["ok"] is False and "unavailable" in resp["error"].lower()
        assert resp["reason"] == resp["error"]
        assert _captions(d) == [] and "thinking" not in _states(d)
        json.dumps(resp)
    asyncio.run(go())


def test_a_refusal_shows_the_error_state_with_the_reason() -> None:
    async def go():
        def refuse(**kw):
            raise ValueError("Open a lesson first.")
        d = _daemon(learning=refuse)
        resp = await d._handle_ipc({"evt": "lesson", "action": "hint"})
        await _settle()
        assert resp["ok"] is False and resp["error"] == "Open a lesson first."
        assert _states(d)[-1] == "error"
        assert len(_captions(d)) == 1 and _captions(d)[0]["chirp"] is False
        _cancel_idle(d)

        def stale(**kw):
            raise KeyError("gone")
        d = _daemon(learning=stale)
        resp = await d._handle_ipc({"evt": "lesson", "action": "status"})
        assert resp["ok"] is False and "no longer there" in resp["error"]
        _cancel_idle(d)
    asyncio.run(go())


def test_an_unexpected_error_ends_thinking_with_an_error_caption(caplog) -> None:
    async def go():
        def broken(**kw):
            raise RuntimeError("disk full near 2x+3=11")
        d = _daemon(learning=broken)
        resp = await d._handle_ipc({"evt": "lesson", "action": "check", "text": "2x+3=11"})
        await _settle()
        assert resp["ok"] is False and resp["action"] == "check" and resp["error"] == resp["reason"]
        assert THINKING in d.ble.sent
        assert _states(d)[-1] == "error"
        caps = _captions(d)
        assert len(caps) == 1 and caps[0]["chirp"] is False
        assert d.ble.sent.index(THINKING) < d.ble.sent.index(caps[0])
        assert d._learning_idle_handle is not None
        assert "RuntimeError" in caplog.text and "2x+3" not in caplog.text
        json.dumps(resp)
        _cancel_idle(d)
    caplog.set_level("INFO")
    asyncio.run(go())


def test_during_a_conversation_live_keeps_the_screen() -> None:
    async def go():
        calls: list = []
        d = _daemon(learning=_voice(calls), conversation=_Talking())
        resp = await d._handle_ipc({"evt": "lesson", "action": "step"})
        await _settle()
        assert resp["ok"] and calls == [{"action": "step"}]
        assert "thinking" not in _states(d) and _captions(d) == []
        assert d._learning_idle_handle is None
    asyncio.run(go())


def test_a_lesson_ends_a_manual_explore() -> None:
    async def go():
        calls: list = []
        d = _daemon(learning=_voice(calls))
        await d._request_explore("requested by cli")
        assert d._explorer.state == "exploring"
        resp = await d._handle_ipc({"evt": "lesson", "action": "step"})
        await _settle()
        assert resp["ok"]
        assert MODE_OFF in d.ble.sent and d._explorer.state == "off"
        _cancel_idle(d)
    asyncio.run(go())


def test_status_neither_ends_the_explore_nor_captions() -> None:
    async def go():
        def status(**kw):
            return {"ok": True, "lesson": {"topic": "Fractions", "mode": "learn", "stage": "practice"},
                    "answer": "Try the next one."}
        d = _daemon(learning=status)
        await d._request_explore("requested by cli")
        n = len(d.ble.sent)
        resp = await d._handle_ipc({"evt": "lesson", "action": "status"})
        await _settle()
        assert resp["ok"] and resp["lesson"]["topic"] == "Fractions"
        assert d._explorer.state == "exploring" and MODE_OFF not in d.ble.sent[n:]
        assert _captions(d) == []
    asyncio.run(go())


def test_back_to_back_notices_keep_only_the_latest_idle_timer() -> None:
    async def go():
        d = _daemon()
        d._learning_notice("First step.", "")
        first = d._learning_idle_handle
        d._learning_notice("Second step.", "")
        second = d._learning_idle_handle
        assert first is not None and second is not None and first is not second
        assert first.cancelled() and not second.cancelled()
        assert d._learning_notices == 2
        second.cancel()
    asyncio.run(go())


def test_a_long_word_still_fits_the_screen() -> None:
    async def go():
        d = _daemon()
        d._learning_notice("  supercalifragilisticexpialidocious\n and   more words here " * 3, "")
        await _settle()
        lines = _captions(d)[0]["lines"]
        assert 1 <= len(lines) <= 4 and all(len(line) <= 17 for line in lines)
        _cancel_idle(d)
        d = _daemon()
        d._learning_notice("", "")
        await _settle()
        assert _captions(d)[0]["lines"] == ["..."]
        _cancel_idle(d)
    asyncio.run(go())


# ---- CLI -------------------------------------------------------------------------------------

@pytest.fixture
def fake_post(monkeypatch):
    """Keep the real env file unread and the real daemon unreached."""
    monkeypatch.setattr(cli, "load_env_file", lambda *a, **k: None)
    sent: list = []
    reply = {"value": {"ok": True}}

    def post(event, **kwargs):
        sent.append((event, kwargs))
        return reply["value"]
    monkeypatch.setattr("eve_bridge.hooks._client.post", post)
    return sent, reply


def test_help_lists_every_action_and_option(fake_post, capsys) -> None:
    with pytest.raises(SystemExit) as exc:
        cli.main(["lesson", "--help"])
    assert exc.value.code == 0
    out = capsys.readouterr().out
    for word in (*LESSON_ACTIONS, "--mode", "--topic", "--level", "--text"):
        assert word in out, word


def test_start_sends_the_lesson_event_and_prints_the_answer(fake_post, capsys) -> None:
    sent, reply = fake_post
    reply["value"] = {"ok": True, "action": "start", "answer": "What is 3/4 + 1/4?"}
    rc = cli.main(["lesson", "start", "--mode", "learn", "--topic", "Fractions", "--level", "Grade 4"])
    assert rc == 0
    event, kwargs = sent[0]
    assert event == {"evt": "lesson", "action": "start", "mode": "learn", "topic": "Fractions", "level": "Grade 4"}
    assert kwargs["timeout"] >= 120
    assert "What is 3/4 + 1/4?" in capsys.readouterr().out


def test_quick_actions_use_a_short_timeout(fake_post) -> None:
    sent, _ = fake_post
    assert cli.main(["lesson", "open"]) == 0
    assert sent[0][0] == {"evt": "lesson", "action": "open"}
    assert sent[0][1]["timeout"] < 60


def test_daemon_not_running_exits_2(fake_post, capsys) -> None:
    _, reply = fake_post
    reply["value"] = None
    assert cli.main(["lesson", "hint"]) == 2
    assert "daemon not reachable" in capsys.readouterr().err


def test_a_refusal_exits_1_with_the_reason(fake_post, capsys) -> None:
    _, reply = fake_post
    reply["value"] = {"ok": False, "error": "Open a lesson first."}
    assert cli.main(["lesson", "check"]) == 1
    assert "Open a lesson first." in capsys.readouterr().err


def test_an_invalid_action_is_rejected_by_argparse(fake_post) -> None:
    sent, _ = fake_post
    with pytest.raises(SystemExit) as exc:
        cli.main(["lesson", "dance"])
    assert exc.value.code == 2
    assert sent == []


def test_bare_ideas_is_refused_so_saved_thinking_is_not_erased(fake_post, capsys) -> None:
    sent, _ = fake_post
    assert cli.main(["lesson", "ideas"]) == 2
    assert sent == []
    assert "--text" in capsys.readouterr().err


def test_empty_ideas_text_is_still_sent(fake_post) -> None:
    sent, _ = fake_post
    assert cli.main(["lesson", "ideas", "--text", ""]) == 0
    assert sent[0][0] == {"evt": "lesson", "action": "ideas", "text": ""}


def test_status_prints_the_lesson(fake_post, capsys) -> None:
    _, reply = fake_post
    reply["value"] = {"ok": True, "action": "status", "answer": "Nice work.",
                      "lesson": {"topic": "Fractions", "mode": "learn", "stage": "practice", "problem": "1/2 + 1/4"}}
    assert cli.main(["lesson", "status"]) == 0
    out = capsys.readouterr().out
    assert "Fractions (learn, practice): 1/2 + 1/4" in out and "Nice work." in out


@pytest.mark.parametrize("action", ["listen", "stop-listening"])
def test_listen_commands_send_the_lesson_event_with_a_short_timeout(fake_post, capsys, action) -> None:
    sent, reply = fake_post
    reply["value"] = {"ok": True, "action": action, "answer": "I'm listening."}
    assert cli.main(["lesson", action]) == 0
    assert sent[0][0] == {"evt": "lesson", "action": action}
    assert sent[0][1]["timeout"] < 60
    assert "I'm listening." in capsys.readouterr().out


# ---- think out loud on the daemon ----------------------------------------------------------------

LISTEN_ON = {"cmd": "listen", "on": True}
LISTEN_OFF = {"cmd": "listen", "on": False}


class _FakeSession:
    def __init__(self, lesson_id=None) -> None:
        self.think_aloud_lesson_id = lesson_id
        self.ended = False
        self.entered: list = []
        self.prepared: list = []
        self.stopped: list = []

    async def enter_think_aloud(self, lesson):
        self.entered.append(lesson["id"])
        self.think_aloud_lesson_id = lesson["id"]
        return {"ok": True}

    def prepare_think_aloud(self, lesson):
        self.prepared.append(lesson["id"])

    def stop_think_aloud(self, reason="stopped"):
        self.stopped.append(reason)
        self.ended = True


def _listening_daemon(tmp_path, monkeypatch, ears=True):
    """A stub daemon wired to a real demo LearningApp whose listener is the daemon's own."""
    from eve_bridge.learning.server import LearningApp
    monkeypatch.setenv("OPENAI_API_KEY", "test-placeholder")     # only its presence is checked; nothing connects
    app = LearningApp(tmp_path, demo=True)
    d = _daemon()
    d._learning_server = SimpleNamespace(app=app)
    d._ears = object() if ears else None
    d._voice_session = None
    d._think_aloud_wish = None
    d._think_aloud_state = "off"
    d._think_aloud_lesson_id = None
    d._shutdown = asyncio.Event()
    d.started: list = []

    async def fake_converse(think_aloud=None):
        d.started.append(think_aloud["id"] if think_aloud else None)
        await asyncio.Event().wait()                 # a conversation that stays open until cancelled
    d._converse = fake_converse
    for name in ("_make_listener", "_think_aloud_start", "_think_aloud_stop", "_start_think_aloud_conversation",
                 "_on_voice_session", "_on_think_aloud", "_sync_listen_pose"):
        setattr(d, name, MethodType(getattr(Daemon, name), d))
    s = app.dispatch({"action": "create", "mode": "learn", "topic": "Algebra", "level": "Grade 5"})
    d.lesson = app.dispatch({"action": "generate", "id": s["id"]})
    return d, app


async def _close(d) -> None:
    if d._conversation is not None and not d._conversation.done():
        d._conversation.cancel()
        await asyncio.gather(d._conversation, return_exceptions=True)
    _cancel_idle(d)


def test_ipc_listen_opens_a_conversation_and_stop_listening_closes_it(tmp_path, monkeypatch) -> None:
    async def go():
        d, app = _listening_daemon(tmp_path, monkeypatch)
        app.listener = d._make_listener(asyncio.get_running_loop())
        resp = await d._handle_ipc({"evt": "lesson", "action": "listen"})
        await _settle()
        assert resp["ok"] and resp["action"] == "listen" and resp["state"] == "starting"
        assert d.started == [d.lesson["id"]] and d._conversation.get_name() == "think-aloud-conversation"
        assert _captions(d) == [] and "thinking" not in _states(d)

        again = await d._handle_ipc({"evt": "lesson", "action": "listen"})     # a second press: no second session
        await _settle()
        assert again["ok"] and d.started == [d.lesson["id"]]

        session = _FakeSession(d.lesson["id"])                              # the session connected and listens
        d._on_voice_session(session)
        d._on_think_aloud(True, d.lesson["id"])
        await _settle()
        assert LISTEN_ON in d.ble.sent
        assert app.listen_status() == {"available": True, "state": "on", "lesson_id": d.lesson["id"], "reason": ""}

        resp = await d._handle_ipc({"evt": "lesson", "action": "stop-listening"})
        await _settle()
        assert resp["ok"] and resp["state"] == "off" and session.stopped == ["stopped"]
        assert d.ble.sent[-1] == LISTEN_OFF
        json.dumps(resp)
        await _close(d)
    asyncio.run(go())


def test_ipc_listen_with_no_lesson_open_is_refused_with_a_caption(tmp_path, monkeypatch) -> None:
    async def go():
        d, app = _listening_daemon(tmp_path, monkeypatch)
        app.listener = d._make_listener(asyncio.get_running_loop())
        app.active = None
        resp = await d._handle_ipc({"evt": "lesson", "action": "listen"})
        await _settle()
        assert resp["ok"] is False and "Open a lesson first" in resp["error"]
        assert d.started == [] and d._think_aloud_state == "off"
        assert len(_captions(d)) == 1 and _states(d)[-1] == "error"
        await _close(d)
    asyncio.run(go())


def test_listen_without_a_microphone_says_why(tmp_path, monkeypatch) -> None:
    async def go():
        d, app = _listening_daemon(tmp_path, monkeypatch, ears=False)
        app.listener = d._make_listener(asyncio.get_running_loop())
        out = await asyncio.to_thread(app.listen)
        assert out["ok"] is False and "microphone is off" in out["reason"] and d.started == []
    asyncio.run(go())


def test_a_wake_word_conversation_switches_instead_of_opening_another(tmp_path, monkeypatch) -> None:
    async def go():
        d, app = _listening_daemon(tmp_path, monkeypatch)
        d._conversation = _Talking()
        d._voice_session = session = _FakeSession()
        out = await d._think_aloud_start(d.lesson)
        assert out["ok"] and session.entered == [d.lesson["id"]] and d.started == []
    asyncio.run(go())


def test_listen_while_connecting_is_handed_to_the_session_when_it_opens(tmp_path, monkeypatch) -> None:
    async def go():
        d, app = _listening_daemon(tmp_path, monkeypatch)
        d._conversation = _Talking()                      # the wake word opened one; it is still connecting
        out = await d._think_aloud_start(d.lesson)
        assert out["ok"] and d._think_aloud_state == "starting" and d.started == []
        session = _FakeSession()
        d._on_voice_session(session)
        assert session.prepared == [d.lesson["id"]] and d._think_aloud_wish is None
    asyncio.run(go())


def test_stop_while_a_listening_conversation_is_still_connecting_cancels_it(tmp_path, monkeypatch) -> None:
    async def go():
        d, app = _listening_daemon(tmp_path, monkeypatch)
        await d._think_aloud_start(d.lesson)
        await asyncio.sleep(0)
        conversation = d._conversation
        out = await d._think_aloud_stop()
        await asyncio.gather(conversation, return_exceptions=True)
        assert out["ok"] and conversation.cancelled() and d._think_aloud_state == "off"
        out = await d._think_aloud_stop()                  # stopping twice is harmless
        assert out["ok"] and "not listening" in out["answer"]
        await _close(d)
    asyncio.run(go())


def test_the_listening_pose_drops_while_eve_thinks_or_speaks(tmp_path, monkeypatch) -> None:
    async def go():
        d, _ = _listening_daemon(tmp_path, monkeypatch)
        d._on_think_aloud(True, d.lesson["id"])
        d._on_agent_state("thinking")
        d._on_agent_state("speaking")
        d._on_agent_state("listening")
        await _settle()
        listens = [m["on"] for m in d.ble.sent if m.get("cmd") == "listen"]
        assert listens == [True, False, True]
        d._on_think_aloud(False, None)
        await _settle()
        assert d.ble.sent[-1] == LISTEN_OFF
    asyncio.run(go())


def test_a_listener_call_on_the_event_loop_is_refused_rather_than_deadlocking(tmp_path, monkeypatch) -> None:
    async def go():
        d, _ = _listening_daemon(tmp_path, monkeypatch)
        listener = d._make_listener(asyncio.get_running_loop())
        with pytest.raises(RuntimeError, match="worker thread"):
            listener.stop()
    asyncio.run(go())


def test_an_open_session_that_refuses_is_answered_not_queued(tmp_path, monkeypatch) -> None:
    class _Busy(_FakeSession):
        async def enter_think_aloud(self, lesson):
            return {"ok": False, "reason": "eve is busy with a computer task. Stop it first, then think out loud."}

    async def go():
        d, _ = _listening_daemon(tmp_path, monkeypatch)
        d._conversation = _Talking()
        d._voice_session = _Busy()
        out = await d._think_aloud_start(d.lesson)
        assert out["ok"] is False and "computer task" in out["reason"]
        assert d._think_aloud_wish is None and d._think_aloud_state == "off" and d.started == []
    asyncio.run(go())
