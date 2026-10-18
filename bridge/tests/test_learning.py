"""Lesson workflow, durable work, request boundaries, and live-response contracts."""
import base64
import json
import urllib.error
import urllib.parse
import urllib.request
from unittest.mock import patch

import pytest

from eve_bridge.learning.server import MAX_BOARD, LearningApp, mark_count, start, validate_work
from eve_bridge.learning.store import Store
from eve_bridge.learning.tutor import LiveTutor, live_settings, validate


@pytest.fixture(autouse=True)
def isolated_tutor_environment(monkeypatch):
    for key in ("EVE_LEARNING_PROVIDER", "EVE_LEARNING_MODEL", "OPENROUTER_API_KEY", "OPENAI_API_KEY", "TINYFISH_API_KEY"):
        monkeypatch.delenv(key, raising=False)


def create(app, mode="learn", topic="Algebra"):
    s = app.dispatch({"action": "create", "mode": mode, "topic": topic, "level": "Grades 6-8"})
    return app.dispatch({"id": s["id"], "action": "generate"}) if mode == "learn" else s


def test_learn_hints_steps_complete_recap_and_restart(tmp_path):
    app = LearningApp(tmp_path, demo=True)
    s = create(app)
    assert s["problem"] == "Solve 2x + 3 = 11."
    s = app.dispatch({"action": "hint"})
    assert not s["events"][-1]["step"]
    for index, expected in enumerate(("2x = 11 - 3", "2x = 8", "x = 4")):
        s = app.dispatch({"action": "step"})
        assert s["events"][-1]["step"] == expected
        assert sum(e["action"] == "step" for e in s["events"]) == index + 1
        assert (s["stage"] == "complete") == (index == 2)
    with pytest.raises(ValueError, match="complete"):
        app.dispatch({"action": "step"})
    s = app.dispatch({"action": "recap"})
    assert "why" in s["events"][-1]["feedback"]
    loaded = Store(tmp_path).get(s["id"])
    assert loaded == s
    assert len(Store(tmp_path).history(s["id"])) == s["revision"]
    assert json.loads((tmp_path / "dashboard.json").read_text())["completed"] == 1


def test_help_requires_confirmation_and_ideas(tmp_path):
    app = LearningApp(tmp_path, demo=True)
    s = create(app, "help")
    with pytest.raises(ValueError, match="Confirm"):
        app.dispatch({"action": "step"})
    app.dispatch({"action": "save", "work": {"problem": "Solve 2x + 3 = 11."}})
    s = app.dispatch({"action": "recognize"})
    assert s["stage"] == "confirm"
    app.dispatch({"action": "confirm"})
    with pytest.raises(ValueError, match="ideas"):
        app.dispatch({"action": "step"})
    app.dispatch({"action": "save", "work": {"ideas": "2x = 8"}})
    s = app.dispatch({"action": "step"})
    assert s["events"][-1]["step"] == "x = 4"
    assert s["stage"] == "complete"


def test_stuck_is_a_valid_start_and_demo_does_not_fake_ocr(tmp_path):
    app = LearningApp(tmp_path, demo=True)
    create(app, "help")
    app.dispatch({"action": "save", "work": {"problem": "A different problem"}})
    s = app.dispatch({"action": "recognize"})
    assert "cannot read" in s["events"][-1]["feedback"]
    app.dispatch({"action": "confirm"})
    app.dispatch({"action": "save", "work": {"stuck": True}})
    s = app.dispatch({"action": "step"})
    assert not s["events"][-1]["step"]
    assert s["events"][-1]["status"] == "clarify"


def test_work_snapshots_conflicts_end_resume_and_delete(tmp_path):
    app = LearningApp(tmp_path, demo=True)
    s = create(app)
    strokes = [{"tool": "pen", "points": [[.1, .2], [.3, .4]]}]
    s = app.dispatch({"action": "save", "revision": s["revision"], "work": {"ideas": "my first idea", "strokes": strokes}})
    with pytest.raises(ValueError, match="another window"):
        app.dispatch({"action": "save", "revision": s["revision"] - 1, "work": {"ideas": "overwrite"}})
    app.dispatch({"action": "save", "work": {"ideas": "a better idea", "strokes": []}})
    assert any(r["strokes"] == strokes for r in app.store.history(s["id"]))
    app.dispatch({"action": "end"})
    with pytest.raises(ValueError, match="Resume"):
        app.dispatch({"action": "save", "work": {"ideas": "oops"}})
    assert app.dispatch({"action": "resume"})["stage"] == "working"
    assert app.dispatch({"action": "delete"})["deleted"]
    assert app.store.list() == []


def test_failed_tutor_leaves_saved_work_intact(tmp_path):
    app = LearningApp(tmp_path, demo=True)
    s = create(app)
    app.dispatch({"action": "save", "work": {"ideas": "keep this"}})
    def broken(*args):
        raise ValueError("network failure")
    app.tutor = broken
    with pytest.raises(ValueError, match="network"):
        app.dispatch({"action": "step"})
    assert app.store.get(s["id"])["ideas"] == "keep this"
    assert not any(e["action"] == "step" for e in app.store.get(s["id"])["events"])


@pytest.mark.parametrize("work", [
    {"board_image": "https://example.org/private"},
    {"source_image": "data:image/svg+xml;base64,abcd"},
    {"strokes": [{"tool": "pen", "points": [[float('nan'), .5]]}]},
    {"strokes": [{"tool": "pen", "points": [[1.5, .5]]}]},
    {"strokes": [{"tool": "script", "points": []}]},
    {"strokes": [{"tool": "text", "points": [[.5, .5]], "text": "   "}]},
    {"strokes": [{"tool": "text", "points": [[.5, .5]], "text": "x" * 501}]},
    {"strokes": [{"tool": "text", "points": [[.1, .1], [.2, .2]], "text": "two anchors"}]},
    {"strokes": [{"tool": "text", "points": [[.5, .5]], "text": 42}]},
    {"strokes": [{"tool": "text", "points": [[.5, .5]]}]},
    {"strokes": [{"tool": "pen", "points": [[.5, .5]], "text": "pens carry no text"}]},
])
def test_invalid_work_rejected(work):
    with pytest.raises(ValueError):
        validate_work(work)


def test_text_boxes_are_strokes_that_round_trip(tmp_path):
    app = LearningApp(tmp_path, demo=True)
    s = create(app)
    strokes = [{"tool": "text", "points": [[.25, .1], ], "text": "I think 2x = 8\nso x = 4"},
               {"tool": "pen", "points": [[.1, .2], [.3, .4]]}]
    assert validate_work({"strokes": strokes})["strokes"] == strokes
    s = app.dispatch({"action": "save", "revision": s["revision"], "work": {"strokes": strokes}})
    assert app.store.get(s["id"])["strokes"] == strokes
    assert any(r["strokes"] == strokes for r in app.store.history(s["id"]))


def board(*shape_ids):
    """A tldraw document the way the whiteboard saves it: a store of records and its schema."""
    store = {"document:document": {"typeName": "document", "id": "document:document"},
             "page:page": {"typeName": "page", "id": "page:page"}}
    store.update({f"shape:{i}": {"typeName": "shape", "id": f"shape:{i}", "type": "draw"} for i in shape_ids})
    return {"store": store, "schema": {"schemaVersion": 2, "sequences": {}}}


def test_tldraw_board_round_trips_and_keeps_every_revision(tmp_path):
    app = LearningApp(tmp_path, demo=True)
    s = create(app)
    assert s["board"] is None
    s = app.dispatch({"action": "save", "revision": s["revision"], "work": {"board": board("a", "b")}})
    assert app.store.get(s["id"])["board"] == board("a", "b")
    s = app.dispatch({"action": "save", "revision": s["revision"], "work": {"board": board("a")}})
    assert [r.get("board") for r in app.store.history(s["id"])][-2:] == [board("a", "b"), board("a")]
    # A save that does not mention the board (typed ideas only) leaves it alone; an explicit null clears it.
    s = app.dispatch({"action": "save", "revision": s["revision"], "work": {"ideas": "still thinking"}})
    assert s["board"] == board("a")
    s = app.dispatch({"action": "save", "revision": s["revision"], "work": {"board": None}})
    assert s["board"] is None


@pytest.mark.parametrize("bad", [
    [], "board", 7, {}, {"store": {}}, {"schema": {}}, {"store": [], "schema": {}}, {"store": {}, "schema": []},
    {"store": {"shape:a": "not a record"}, "schema": {}},
    {"store": {"shape:a": {"typeName": "shape", "pad": "x" * MAX_BOARD}}, "schema": {}},
])
def test_invalid_board_rejected(bad):
    with pytest.raises(ValueError, match="hiteboard"):
        validate_work({"board": bad})


def test_marks_count_tldraw_shapes_and_strokes_from_before_tldraw():
    assert mark_count({}) == 0
    assert mark_count({"board": None, "strokes": []}) == 0
    assert mark_count({"board": board()}) == 0  # the document and page records are not marks
    assert mark_count({"board": board("a", "b")}) == 2
    old = [{"tool": "pen", "points": [[.1, .2]]}]
    assert mark_count({"strokes": old}) == 1
    assert mark_count({"board": board("a"), "strokes": old}) == 2


def test_help_path_counts_a_new_mark_on_the_board_as_an_attempt(tmp_path):
    app = LearningApp(tmp_path, demo=True)
    s = create(app, "help")
    with pytest.raises(ValueError, match="Write a problem"):
        app.dispatch({"action": "recognize"})
    # The problem written on the board is enough for eve to try reading it, and is not yet an attempt.
    s = app.dispatch({"action": "save", "revision": s["revision"], "work": {"board": board("problem")}})
    assert app.dispatch({"action": "recognize"})["stage"] == "confirm"
    # The offline demo cannot read a board, so the learner types what it says before confirming.
    app.dispatch({"action": "save", "work": {"problem": "Solve 2x + 3 = 11."}})
    s = app.dispatch({"action": "confirm"})
    assert s["problem_stroke_count"] == 1
    with pytest.raises(ValueError, match="ideas first"):
        app.dispatch({"action": "hint"})
    app.dispatch({"action": "save", "work": {"board": board("problem", "attempt")}})
    assert app.dispatch({"action": "hint"})["events"][-1]["action"] == "hint"


def test_page_policy_is_same_origin_with_a_fresh_style_nonce_and_never_unsafe_inline(tmp_path):
    server = start(tmp_path, demo=True, port=0)
    url = server.app.url
    try:
        seen = set()
        for _ in range(2):
            with urllib.request.urlopen(url) as r:
                csp, html = r.headers.get("Content-Security-Policy", ""), r.read().decode()
            policy = dict(part.strip().split(" ", 1) for part in csp.split(";"))
            assert policy["default-src"] == "'self'" and policy["script-src"] == "'self'"
            assert policy["img-src"] == "'self' data: blob:"
            assert "unsafe" not in csp and "http" not in csp and "*" not in csp
            sources = policy["style-src"].split()
            assert sources[0] == "'self'" and len(sources) == 2 and sources[1].startswith("'nonce-")
            nonce = sources[1][len("'nonce-"):-1]
            assert len(nonce) >= 16 and f'nonce="{nonce}"' in html and "__EVE_NONCE__" not in html
            seen.add(nonce)
        assert len(seen) == 2, "the nonce must change with every page load"
        with urllib.request.urlopen(url + "app.js") as r:
            assert "nonce" not in r.headers.get("Content-Security-Policy", "")
    finally:
        server.shutdown()
        server.server_close()


def test_whiteboard_bundle_is_served_same_origin_and_traversal_is_rejected(tmp_path):
    server = start(tmp_path, demo=True, port=0)
    url = server.app.url
    try:
        with urllib.request.urlopen(url) as r:
            html = r.read().decode()
        assert 'src="/canvas/canvas.js"' in html and 'href="/canvas/canvas.css"' in html
        for path, mime in (("canvas/canvas.js", "text/javascript"), ("canvas/canvas.css", "text/css"),
                           ("canvas/assets/translations/fr.json", "application/json"),
                           ("canvas/assets/icons/icon/0_merged.svg", "image/svg+xml"),
                           ("canvas/assets/fonts/IBMPlexSans-Medium.woff2", "font/woff2"),
                           ("canvas/TLDRAW-LICENSE.md", "text/plain")):
            with urllib.request.urlopen(url + path) as r:
                assert r.status == 200 and r.headers.get_content_type() == mime, path
                body = r.read()
                assert len(body) > 1000, path
            if path.endswith(".json"):
                assert isinstance(json.loads(body), dict)  # the file itself, not a JSON string wrapped around it
        for bad in ("canvas/../server.py", "canvas/%2e%2e%2fserver.py", "canvas/..%2fserver.py", "canvas/assets/../../app.js",
                    "canvas/.hidden.js", "canvas/nope.js", "canvas/assets", "canvas/canvas.js.map", "canvas/"):
            with pytest.raises(urllib.error.HTTPError) as exc:
                urllib.request.urlopen(url + bad)
            assert exc.value.code == 404, bad
    finally:
        server.shutdown()
        server.server_close()


def test_tldraw_licence_key_reaches_the_page_only_when_set(monkeypatch, tmp_path):
    server = start(tmp_path, demo=True, port=0)
    try:
        def key():
            with urllib.request.urlopen(server.app.url + "api/config") as r:
                return json.load(r)["tldraw_license_key"]
        monkeypatch.delenv("EVE_TLDRAW_LICENSE_KEY", raising=False)
        assert key() == ""
        monkeypatch.setenv("EVE_TLDRAW_LICENSE_KEY", "  tldraw-test-key  ")
        assert key() == "tldraw-test-key"
    finally:
        server.shutdown()
        server.server_close()


def test_live_requires_key_and_strict_reply(monkeypatch):
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    monkeypatch.setenv("OPENAI_API_KEY", "not-a-fallback")
    with pytest.raises(ValueError, match="OPENROUTER_API_KEY"):
        LiveTutor()("generate", {})
    with pytest.raises(ValueError):
        validate({"step": "x=2"}, "step")


def test_live_request_uses_current_images_and_a_strict_schema(monkeypatch, tmp_path):
    monkeypatch.setenv("OPENROUTER_API_KEY", "test-only")
    app = LearningApp(tmp_path, demo=True)
    s = create(app)
    s["source_image"] = "data:image/png;base64," + base64.b64encode(b"fake-image-for-mock").decode()
    reply = {"choices": [{"finish_reason": "stop", "message": {"content": json.dumps(
        {"problem": "", "feedback": "Subtract three from both sides.", "step": "2x = 11 - 3", "status": "continue"})}}]}
    class Response:
        def __enter__(self):
            return self
        def __exit__(self, *args):
            pass
        def read(self):
            return json.dumps(reply).encode()
    with patch("urllib.request.urlopen", return_value=Response()) as call:
        result = LiveTutor()("step", s)
    body = json.loads(call.call_args.args[0].data)
    assert call.call_args.args[0].full_url == "https://openrouter.ai/api/v1/chat/completions"
    assert body["response_format"]["json_schema"]["strict"] is True
    assert any(c["type"] == "image_url" for c in body["messages"][1]["content"])
    assert result["step"] == "2x = 11 - 3"


def test_http_origin_token_and_persistence(tmp_path):
    server = start(tmp_path, demo=True, port=0)
    url = server.app.url
    def get(path):
        with urllib.request.urlopen(url + path) as r:
            return json.load(r)
    try:
        cfg = get("api/config")
        data = json.dumps({"action": "create", "mode": "learn", "topic": "Addition"}).encode()
        for headers in ({}, {"X-Eve-Token": cfg["token"], "Origin": "https://evil.example"}):
            req = urllib.request.Request(url + "api/action", data, headers)
            with pytest.raises(urllib.error.HTTPError) as exc:
                urllib.request.urlopen(req)
            assert exc.value.code == 403
        req = urllib.request.Request(url + "api/action", data, {"X-Eve-Token": cfg["token"]})
        with urllib.request.urlopen(req) as r:
            lesson = json.load(r)
        assert get("api/lessons")[0]["id"] == lesson["id"]
        assert get("api/lessons/" + lesson["id"])["topic"] == "Addition"
    finally:
        server.shutdown()
        server.server_close()


def test_bundled_fonts_are_served_same_origin_and_traversal_is_rejected(tmp_path):
    server = start(tmp_path, demo=True, port=0)
    url = server.app.url
    try:
        with urllib.request.urlopen(url) as r:
            csp = r.headers.get("Content-Security-Policy", "")
        assert "font-src 'self'" in csp
        with urllib.request.urlopen(url + "style.css") as r:
            css = r.read().decode()
        fonts = sorted(set(part.split(")")[0].strip("'\"") for part in css.split("url(")[1:] if ".woff2" in part))
        assert fonts, "style.css references no bundled woff2 fonts"
        assert not any("googleapis" in f or "gstatic" in f for f in fonts)
        for ref in fonts:
            with urllib.request.urlopen(urllib.parse.urljoin(url + "style.css", ref)) as r:
                assert r.status == 200
                assert r.headers.get_content_type() == "font/woff2"
                assert len(r.read()) > 1000
        with urllib.request.urlopen(url + "fonts/OFL.txt") as r:
            assert r.headers.get_content_type() == "text/plain"
        for bad in ("fonts/../server.py", "fonts/%2e%2e%2fserver.py", "fonts/..%2fserver.py", "fonts/nope.woff2"):
            with pytest.raises(urllib.error.HTTPError) as exc:
                urllib.request.urlopen(url + bad)
            assert exc.value.code == 404, bad
    finally:
        server.shutdown()
        server.server_close()


def test_voice_uses_same_lesson_state(tmp_path):
    app = LearningApp(tmp_path, demo=True)
    with patch("webbrowser.open"):
        assert app.voice("open")["ok"]
        app.voice("start", mode="learn", topic="Calculus", level="College year 1")
    assert "3x^(3 - 1)" in app.voice("step")["answer"]
    assert "3x^2" in app.voice("step")["answer"]
    assert app.store.get(app.active)["stage"] == "complete"


@pytest.mark.parametrize("env, provider, model, ready", [
    ({}, "openrouter", "openai/gpt-6-astra", False),
    ({"OPENROUTER_API_KEY": "router"}, "openrouter", "openai/gpt-6-astra", True),
    ({"OPENAI_API_KEY": "direct"}, "openrouter", "openai/gpt-6-astra", False),       # never a fallback
    ({"EVE_LEARNING_PROVIDER": "openrouter", "OPENROUTER_API_KEY": "router"}, "openrouter", "openai/gpt-6-astra", True),
    ({"EVE_LEARNING_MODEL": "gpt-6-astra", "OPENROUTER_API_KEY": "router"}, "openrouter", "openai/gpt-6-astra", True),
    # a leftover provider=openai (the old default) means OpenRouter too since 2026-10-08
    ({"EVE_LEARNING_PROVIDER": "openai", "OPENAI_API_KEY": "direct", "OPENROUTER_API_KEY": "router"}, "openrouter", "openai/gpt-6-astra", True),
])
def test_provider_selection_and_astra_default(env, provider, model, ready):
    cfg = live_settings(env)
    assert (cfg["provider"], cfg["model"], cfg["ready"]) == (provider, model, ready)
    assert "router" not in cfg.values() and "direct" not in cfg.values()


def test_the_tutor_goes_to_openrouter_even_with_an_openai_key(monkeypatch, tmp_path):
    monkeypatch.setenv("OPENAI_API_KEY", "direct-test")
    monkeypatch.setenv("OPENROUTER_API_KEY", "router-test")
    s = create(LearningApp(tmp_path, demo=True))
    reply = {"choices": [{"finish_reason": "stop", "message": {"content": json.dumps(
        {"problem": "", "feedback": "Look at the ones place.", "step": "", "status": "continue"})}}]}

    class Response:
        def __enter__(self):
            return self

        def __exit__(self, *args):
            pass

        def read(self):
            return json.dumps(reply).encode()

    with patch("urllib.request.urlopen", return_value=Response()) as call:
        LiveTutor()("hint", s)
    request = call.call_args.args[0]
    assert request.full_url == "https://openrouter.ai/api/v1/chat/completions"
    assert request.get_header("Authorization") == "Bearer router-test"


def test_an_openai_key_alone_asks_for_the_openrouter_key(monkeypatch):
    monkeypatch.delenv("OPENROUTER_API_KEY", raising=False)
    monkeypatch.setenv("OPENAI_API_KEY", "direct-test")
    with patch("urllib.request.urlopen") as call:
        with pytest.raises(ValueError, match="OPENROUTER_API_KEY"):
            LiveTutor()("generate", {})
    call.assert_not_called()


def test_invalid_provider_fails_without_guessing():
    with pytest.raises(ValueError, match="must be"):
        live_settings({"EVE_LEARNING_PROVIDER": "typo"})


def test_openrouter_request_and_parser(monkeypatch, tmp_path):
    import io
    monkeypatch.setenv("EVE_LEARNING_PROVIDER", "openrouter")
    monkeypatch.setenv("OPENROUTER_API_KEY", "router-test-secret")
    monkeypatch.setenv("OPENAI_API_KEY", "do-not-send-this")
    s = create(LearningApp(tmp_path, demo=True))
    s["source_image"] = "data:image/png;base64,original"
    s["board_image"] = "data:image/png;base64,current"
    reply = {"problem": "", "feedback": "Subtract three from both sides.", "step": "2x = 11 - 3", "status": "continue"}
    data = {"choices": [{"finish_reason": "stop", "message": {"content": json.dumps(reply)}}]}
    with patch("urllib.request.urlopen", return_value=io.BytesIO(json.dumps(data).encode())) as call:
        assert LiveTutor()("step", s) == reply
    request = call.call_args.args[0]
    body = json.loads(request.data)
    assert request.full_url == "https://openrouter.ai/api/v1/chat/completions"
    assert request.get_header("Authorization") == "Bearer router-test-secret"
    assert body["model"] == "openai/gpt-6-astra"
    assert "models" not in body  # no alternative model routing
    assert body["response_format"]["json_schema"]["strict"] is True
    assert body["provider"]["require_parameters"] is True
    images = [c for c in body["messages"][1]["content"] if c["type"] == "image_url"]
    assert [c["image_url"]["url"] for c in images] == [s["source_image"], s["board_image"]]


@pytest.mark.parametrize("finish", ["length", "error", "content_filter"])
def test_openrouter_incomplete_response_is_not_accepted(monkeypatch, tmp_path, finish):
    import io
    monkeypatch.setenv("EVE_LEARNING_PROVIDER", "openrouter")
    monkeypatch.setenv("OPENROUTER_API_KEY", "router-test")
    s = create(LearningApp(tmp_path, demo=True))
    data = {"choices": [{"finish_reason": finish, "message": {"content": "{}"}}]}
    with patch("urllib.request.urlopen", return_value=io.BytesIO(json.dumps(data).encode())):
        with pytest.raises(ValueError, match="did not finish"):
            LiveTutor()("check", s)


def test_openrouter_http_error_never_exposes_credentials(monkeypatch, tmp_path):
    monkeypatch.setenv("EVE_LEARNING_PROVIDER", "openrouter")
    monkeypatch.setenv("OPENROUTER_API_KEY", "private-test-key")
    s = create(LearningApp(tmp_path, demo=True))
    error = urllib.error.HTTPError("https://openrouter.ai/api/v1/chat/completions", 402, "secret-provider-body", {}, None)
    with patch("urllib.request.urlopen", side_effect=error):
        with pytest.raises(ValueError, match="credits") as exc:
            LiveTutor()("check", s)
    assert "private-test-key" not in str(exc.value)
    assert "secret-provider-body" not in str(exc.value)


def test_config_reports_openrouter_readiness_without_key(monkeypatch, tmp_path):
    monkeypatch.setenv("EVE_LEARNING_PROVIDER", "openrouter")
    monkeypatch.setenv("OPENROUTER_API_KEY", "private-test-key")
    server = start(tmp_path, port=0)
    try:
        with urllib.request.urlopen(server.app.url + "api/config") as response:
            cfg = json.load(response)
        assert cfg["provider"] == "openrouter" and cfg["ready"]
        assert cfg["model"] == "openai/gpt-6-astra"
        assert "private-test-key" not in json.dumps(cfg)
    finally:
        server.shutdown()
        server.server_close()


def test_openrouter_terms_restriction_is_actionable_without_leaking_body(monkeypatch, tmp_path):
    import io
    monkeypatch.setenv("EVE_LEARNING_PROVIDER", "openrouter")
    monkeypatch.setenv("OPENROUTER_API_KEY", "private-test-key")
    s = create(LearningApp(tmp_path, demo=True))
    payload = {"error": {"message": "The request is prohibited due to a violation of provider Terms Of Service."},
               "user_id": "private-user-id"}
    error = urllib.error.HTTPError("https://openrouter.ai/api/v1/chat/completions", 403, "Forbidden", {},
                                  io.BytesIO(json.dumps(payload).encode()))
    with patch("urllib.request.urlopen", side_effect=error):
        with pytest.raises(ValueError, match="Terms of Service restriction") as exc:
            LiveTutor()("generate", s)
    assert "private-test-key" not in str(exc.value)
    assert "private-user-id" not in str(exc.value)
    assert "support" in str(exc.value)


def test_launcher_explains_busy_port_without_traceback(tmp_path, capsys):
    import errno

    from eve_bridge.learning.server import main
    with patch("eve_bridge.envfile.load_env_file"), patch(
        "eve_bridge.learning.server.start", side_effect=OSError(errno.EADDRINUSE, "Address already in use")
    ):
        assert main(["--data-dir", str(tmp_path), "--no-open"]) == 2
    error = capsys.readouterr().err
    assert "http://127.0.0.1:48766/" in error
    assert "stop the existing Eve process" in error
    assert "Traceback" not in error


def test_launcher_does_not_hide_other_startup_failures(tmp_path):
    import errno

    from eve_bridge.learning.server import main
    with patch("eve_bridge.envfile.load_env_file"), patch(
        "eve_bridge.learning.server.start", side_effect=OSError(errno.EACCES, "Permission denied")
    ):
        with pytest.raises(OSError):
            main(["--data-dir", str(tmp_path), "--no-open"])


def _png(size):
    return "data:image/png;base64," + base64.b64encode(b"\x89PNG\r\n\x1a\n" + b"x" * size).decode()


def test_an_unchanged_image_is_stored_once_across_revisions(tmp_path):
    """Review D4: every autosave copied the screenshot (up to 6 MB) and the board into a new revision."""
    import sqlite3
    app = LearningApp(tmp_path, demo=True)
    s = create(app, "help")
    image = _png(300_000)
    s = app.dispatch({"action": "save", "revision": s["revision"], "work": {"source_image": image}})
    for i in range(10):
        s = app.dispatch({"action": "save", "revision": s["revision"], "work": {"ideas": f"idea {i}"}})
    db = sqlite3.connect(tmp_path / "lessons.sqlite3")
    stored = db.execute("SELECT SUM(LENGTH(data)) FROM revisions").fetchone()[0]
    blobs = db.execute("SELECT SUM(LENGTH(data)) FROM blobs").fetchone()[0]
    db.close()
    assert stored + blobs < 2 * len(image), "the image was copied into every revision"
    history = app.store.history(s["id"])
    assert len(history) == s["revision"]
    assert [r["source_image"] for r in history][-11:] == [image] * 11   # history still returns it whole
    assert history[-1] == app.store.get(s["id"])
    app.dispatch({"action": "delete", "id": s["id"]})
    db = sqlite3.connect(tmp_path / "lessons.sqlite3")
    assert db.execute("SELECT COUNT(*) FROM blobs").fetchone()[0] == 0
    db.close()


def test_the_widget_and_list_read_summaries_without_parsing_lessons(tmp_path):
    app = LearningApp(tmp_path, demo=True)
    s = create(app)
    with patch("eve_bridge.learning.store.json.loads", side_effect=AssertionError("parsed a whole lesson")):
        rows = app.store.list()
        app.store.write_widget()
    assert rows == [Store.summary(app.store.get(s["id"]))]
    assert rows[0]["demo"] is True
    assert json.loads((tmp_path / "dashboard.json").read_text())["lessons"] == rows


class _SlowTutor:
    """DemoTutor that waits on ``release`` before answering a hint, like a live model call."""
    demo = True

    def __init__(self):
        import threading
        from eve_bridge.learning.tutor import DemoTutor
        self.inner, self.entered, self.release = DemoTutor(), threading.Event(), threading.Event()

    def __call__(self, action, lesson):
        if action == "hint":
            self.entered.set()
            assert self.release.wait(5)
        return self.inner(action, lesson)


def test_the_tutor_call_does_not_hold_the_lesson_lock(tmp_path):
    """Review D3: a slow tutor (up to 90 s live) must not block autosave or a spoken append."""
    import threading
    tutor = _SlowTutor()
    app = LearningApp(tmp_path, tutor=tutor)
    s = create(app)
    out = {}
    asker = threading.Thread(target=lambda: out.update(s=app.dispatch({"id": s["id"], "action": "hint"})))
    asker.start()
    assert tutor.entered.wait(5)
    spoken = threading.Thread(target=lambda: out.update(spoken=app.append_spoken(s["id"], "maybe subtract three")))
    spoken.start()
    spoken.join(2)
    saved = not spoken.is_alive()
    tutor.release.set()
    asker.join(5)
    spoken.join(5)
    assert saved, "append_spoken waited for the tutor"
    assert out["spoken"] is True
    assert out["s"]["events"][-1]["action"] == "hint"
    assert "maybe subtract three" in out["s"]["ideas"]   # the answer lands on top of the spoken line


def test_a_lesson_moved_on_during_the_tutor_call_is_refused(tmp_path):
    import threading
    tutor = _SlowTutor()
    app = LearningApp(tmp_path, tutor=tutor)
    s = create(app)
    errors = []

    def ask():
        try:
            app.dispatch({"id": s["id"], "action": "hint"})
        except ValueError as exc:
            errors.append(str(exc))

    asker = threading.Thread(target=ask)
    asker.start()
    assert tutor.entered.wait(5)
    app.dispatch({"id": s["id"], "action": "end"})
    tutor.release.set()
    asker.join(5)
    assert errors and "changed in another window" in errors[0]
    assert app.store.get(s["id"])["stage"] == "ended"
