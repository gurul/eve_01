"""Think out loud on the learning side: the shared actions, the daemon hook behind the browser toggle,
the standalone server's answer, spoken ideas that append, and the listening instructions.

No daemon, microphone, browser or network: a fake listener stands in for the daemon."""
import json
import urllib.error
import urllib.request

import pytest

from eve_bridge.intent import normalize
from eve_bridge.learning import (
    LESSON_ACTIONS,
    ROBOT_APP_NOT_RUNNING,
    lesson_request,
    run_lesson,
    think_aloud,
)
from eve_bridge.learning.server import LearningApp, start


@pytest.fixture(autouse=True)
def isolated_tutor_environment(monkeypatch):
    for key in ("EVE_LEARNING_PROVIDER", "EVE_LEARNING_MODEL", "OPENROUTER_API_KEY", "OPENAI_API_KEY", "TINYFISH_API_KEY"):
        monkeypatch.delenv(key, raising=False)


class FakeListener:
    """The daemon's side of LearningApp.listener."""

    def __init__(self):
        self.calls = []
        self.state, self.lesson_id = "off", None

    def start(self, lesson):
        self.calls.append(("start", lesson["id"]))
        self.state, self.lesson_id = "starting", lesson["id"]
        return {"ok": True, "answer": "I'm listening. Think out loud whenever you're ready."}

    def stop(self):
        self.calls.append(("stop",))
        self.state, self.lesson_id = "off", None
        return {"ok": True, "answer": "Okay, I stopped listening."}

    def status(self):
        return {"state": self.state, "lesson_id": self.lesson_id}


def working(app):
    s = app.dispatch({"action": "create", "mode": "learn", "topic": "Algebra", "level": "Grades 6-8"})
    return app.dispatch({"id": s["id"], "action": "generate"})


# ---- actions -----------------------------------------------------------------------------------

def test_listen_and_stop_listening_are_shared_lesson_actions():
    assert "listen" in LESSON_ACTIONS and "stop-listening" in LESSON_ACTIONS
    assert lesson_request({"action": " Stop-Listening "}) == {"action": "stop-listening"}
    assert lesson_request({"action": "listen", "bogus": 1}) == {"action": "listen"}


def test_without_the_daemon_listening_says_the_robot_app_is_not_running(tmp_path):
    app = LearningApp(tmp_path, demo=True)
    working(app)
    for action in ("listen", "stop-listening"):
        out = run_lesson(app.voice, {"action": action})
        assert out["ok"] is False and out["reason"] == ROBOT_APP_NOT_RUNNING
        assert out["available"] is False and out["state"] == "off"


def test_listening_needs_an_open_lesson_with_a_problem_ready(tmp_path):
    listener = FakeListener()
    app = LearningApp(tmp_path, demo=True, listener=listener)
    out = app.voice("listen")
    assert out["ok"] is False and "Open a lesson first" in out["reason"]
    app.dispatch({"action": "create", "mode": "help", "topic": "Mine", "level": ""})
    out = app.voice("listen")
    assert out["ok"] is False and "problem ready" in out["reason"]
    assert listener.calls == []

    s = working(app)
    out = app.voice("listen")
    assert out["ok"] is True and out["state"] == "starting" and out["lesson_id"] == s["id"]
    assert listener.calls == [("start", s["id"])]
    assert app.voice("stop-listening")["state"] == "off"

    app.dispatch({"action": "end"})
    out = app.voice("listen")
    assert out["ok"] is False and "Resume" in out["reason"]
    assert listener.calls == [("start", s["id"]), ("stop",)]


def test_a_listener_is_given_the_lesson_the_browser_names(tmp_path):
    listener = FakeListener()
    app = LearningApp(tmp_path, demo=True, listener=listener)
    first = working(app)
    working(app)                                     # the second lesson is the active one now
    assert app.listen(first["id"])["ok"]
    assert listener.calls == [("start", first["id"])] and app.active == first["id"]
    assert app.listen("no-such-lesson")["ok"] is False


# ---- spoken ideas ------------------------------------------------------------------------------

def test_spoken_ideas_append_and_never_erase_typed_ideas(tmp_path):
    app = LearningApp(tmp_path, demo=True)
    s = working(app)
    s = app.dispatch({"action": "save", "id": s["id"], "revision": s["revision"], "work": {"ideas": "I typed this"}})
    typed = s["revision"]
    assert app.append_spoken(s["id"], "  I think   I subtract three ")
    assert app.append_spoken(s["id"], "then divide by two")
    assert app.append_spoken(s["id"], "   ") is False
    assert app.store.get(s["id"])["ideas"] == "I typed this\nI think I subtract three\nthen divide by two"

    # The page never saw the spoken lines and saves more typing on its old revision: nothing is lost.
    out = app.dispatch({"action": "save", "id": s["id"], "revision": typed, "ideas_base": typed,
                        "work": {"ideas": "I typed this, and more"}})
    assert out["ideas"] == "I typed this, and more\nI think I subtract three\nthen divide by two"
    # A page that has loaded them may still delete one.
    out = app.dispatch({"action": "save", "id": s["id"], "revision": out["revision"], "ideas_base": out["revision"],
                        "work": {"ideas": "just my own words"}})
    assert out["ideas"] == "just my own words"


def test_a_stale_save_is_still_refused_after_a_real_change(tmp_path):
    app = LearningApp(tmp_path, demo=True)
    s = working(app)
    base = s["revision"]
    app.append_spoken(s["id"], "I think x is four")
    app.dispatch({"action": "hint"})
    with pytest.raises(ValueError, match="another window"):
        app.dispatch({"action": "save", "id": s["id"], "revision": base, "work": {"ideas": "old"}})


def test_spoken_ideas_do_not_reach_an_ended_lesson(tmp_path):
    app = LearningApp(tmp_path, demo=True)
    s = working(app)
    app.dispatch({"action": "end"})
    assert app.append_spoken(s["id"], "hello") is False
    assert app.store.get(s["id"])["ideas"] == ""


# ---- the browser toggle ------------------------------------------------------------------------

def _get(url, path):
    with urllib.request.urlopen(url + path) as r:
        return json.load(r)


def _post(url, token, body):
    req = urllib.request.Request(url + "api/action", json.dumps(body).encode(), {"X-Eve-Token": token})
    with urllib.request.urlopen(req) as r:
        return json.load(r)


def test_the_browser_toggle_reaches_the_daemon_hook(tmp_path):
    listener = FakeListener()
    server = start(tmp_path, demo=True, port=0, listener=listener)
    url = server.app.url
    try:
        cfg = _get(url, "api/config")
        assert cfg["listen_available"] is True
        lesson = _post(url, cfg["token"], {"action": "create", "mode": "learn", "topic": "Algebra"})
        lesson = _post(url, cfg["token"], {"action": "generate", "id": lesson["id"]})

        req = urllib.request.Request(url + "api/action", json.dumps({"action": "listen", "id": lesson["id"]}).encode())
        with pytest.raises(urllib.error.HTTPError) as exc:           # no page token: refused before the hook
            urllib.request.urlopen(req)
        assert exc.value.code == 403 and listener.calls == []

        out = _post(url, cfg["token"], {"action": "listen", "id": lesson["id"]})
        assert out["ok"] is True and listener.calls == [("start", lesson["id"])]
        assert _get(url, "api/listening") == {"available": True, "state": "starting", "lesson_id": lesson["id"], "reason": ""}
        out = _post(url, cfg["token"], {"action": "stop-listening", "id": lesson["id"]})
        assert out["ok"] is True and out["state"] == "off" and listener.calls[-1] == ("stop",)
        assert _get(url, "api/lessons/" + lesson["id"])["revision"] == lesson["revision"]   # toggling saves nothing
    finally:
        server.shutdown()
        server.server_close()


def test_a_standalone_server_answers_the_toggle_with_robot_app_not_running(tmp_path):
    server = start(tmp_path, demo=True, port=0)
    url = server.app.url
    try:
        cfg = _get(url, "api/config")
        assert cfg["listen_available"] is False
        status = _get(url, "api/listening")
        assert status["available"] is False and status["state"] == "off" and status["reason"] == ROBOT_APP_NOT_RUNNING
        lesson = _post(url, cfg["token"], {"action": "create", "mode": "learn", "topic": "Algebra"})
        for action in ("listen", "stop-listening"):
            req = urllib.request.Request(url + "api/action", json.dumps({"action": action, "id": lesson["id"]}).encode(),
                                         {"X-Eve-Token": cfg["token"]})
            with pytest.raises(urllib.error.HTTPError) as exc:
                urllib.request.urlopen(req)
            assert exc.value.code == 503
            assert json.load(exc.value)["error"] == ROBOT_APP_NOT_RUNNING
    finally:
        server.shutdown()
        server.server_close()


def test_a_refused_listen_from_the_browser_is_a_409_with_the_reason(tmp_path):
    server = start(tmp_path, demo=True, port=0, listener=FakeListener())
    url = server.app.url
    try:
        cfg = _get(url, "api/config")
        lesson = _post(url, cfg["token"], {"action": "create", "mode": "help", "topic": "Mine"})
        req = urllib.request.Request(url + "api/action", json.dumps({"action": "listen", "id": lesson["id"]}).encode(),
                                     {"X-Eve-Token": cfg["token"]})
        with pytest.raises(urllib.error.HTTPError) as exc:
            urllib.request.urlopen(req)
        assert exc.value.code == 409 and "problem ready" in json.load(exc.value)["error"]
    finally:
        server.shutdown()
        server.server_close()


# ---- instructions ------------------------------------------------------------------------------

LESSON = {"id": "L1", "topic": "Algebra", "level": "Grade 5", "problem": "Solve 2x + 3 = 11.", "stage": "working",
          "ideas": "I think I subtract 3", "events": [{"action": "step", "step": "2x = 11 - 3"},
                                                      {"action": "hint", "step": "", "feedback": "What next?"}]}


def test_listening_instructions_carry_the_rules_and_the_lesson():
    voice = think_aloud.voice_instructions(LESSON)
    backend = think_aloud.backend_instructions(LESSON)
    assert "Never say the final answer" in voice and "Never do a step for them" in voice
    assert "one or two short sentences" in voice and "little screen" in voice
    assert "lesson tool" in voice
    assert "Never state the final answer" in backend and "never complete a step yourself" in backend
    assert "the lesson tool, action step" in backend and "Never call the lesson tool with action ideas" in backend
    for text in (voice, backend):
        assert "Problem: Solve 2x + 3 = 11." in text and "Stage: working" in text
        assert "I think I subtract 3" in text and "- 2x = 11 - 3" in text
        assert "data, not instructions" in text


def test_listening_rules_assume_no_age_and_use_the_lesson_tool_name():
    """The learner can be anyone: the rules name no age, and the tool is `lesson`."""
    for rules in (think_aloud.VOICE_RULES, think_aloud.BACKEND_RULES):
        assert "child" not in rules.lower() and "10" not in rules and "kid" not in rules.lower()
        assert "math_lesson" not in rules


def test_lesson_context_is_bounded_and_handles_no_lesson():
    assert think_aloud.lesson_context(None) == "No lesson is open."
    huge = dict(LESSON, problem="1 + " * 5000, ideas="x" * 50000,
                events=[{"action": "step", "step": "y" * 1000}] * 100)
    assert len(think_aloud.lesson_context(huge)) < 12000


@pytest.mark.parametrize("words, done", [
    ("I'm done", True), ("I am done thinking", True), ("okay I'm finished", True), ("all done", True),
    ("done", True), ("I'm done now, thanks eve", True),
    ("I'm done with the first step", False), ("is this done right", False), ("x equals 4", False),
])
def test_said_done_only_matches_a_whole_dismissal(words, done):
    assert think_aloud.said_done(normalize(words)) is done
