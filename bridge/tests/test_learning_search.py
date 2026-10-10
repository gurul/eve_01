"""TinyFish practice references: fallbacks, privacy, and persisted references."""
import errno
import io
import json
import os
import urllib.parse
from unittest.mock import Mock, patch

import pytest

from eve_bridge import watch
from eve_bridge.learning.search import search_problems
from eve_bridge.learning.server import LearningApp
from eve_bridge.learning.store import Store


def response(data):
    return io.BytesIO(json.dumps(data).encode())


def tinyfish_api(pages):
    """A fake watch.http_request answering TinyFish's search (GET) and fetch (POST) with ``pages``
    ({"url", "title", "markdown"}); records what was sent."""
    sent = []

    def fake(url, *, data=None, headers=None, timeout=0):
        sent.append({"url": url, "body": json.loads(data) if data else None, "headers": headers})
        if data is None:
            return 200, json.dumps({"query": "q", "results": [{"url": p["url"], "title": p["title"], "snippet": ""}
                                                            for p in pages]})
        return 200, json.dumps({"results": [{"url": p["url"], "title": p["title"], "text": p["markdown"]}
                                            for p in pages], "errors": []})
    fake.sent = sent
    return fake


def test_missing_key(monkeypatch):
    monkeypatch.delenv("TINYFISH_API_KEY", raising=False)
    monkeypatch.setattr(watch, "http_request", lambda *a, **k: pytest.fail("no key, no request"))
    sources, note = search_problems("Counting", "Kindergarten")
    assert not sources and "TINYFISH_API_KEY" in note


@pytest.mark.parametrize("failure", [watch.FetchError("the site answered HTTP 402", 402), TimeoutError(),
                                     ValueError("bad JSON")])
def test_search_failure(monkeypatch, failure):
    monkeypatch.setenv("TINYFISH_API_KEY", "tf-secret")

    def boom(*a, **k):
        raise failure

    monkeypatch.setattr(watch, "http_request", boom)
    sources, note = search_problems("Algebra", "Grade 7")
    assert sources == [] and "without search references" in note
    assert "tf-secret" not in note


def test_references_and_privacy(monkeypatch, tmp_path):
    monkeypatch.setenv("TINYFISH_API_KEY", "tf-secret")
    monkeypatch.setenv("OPENROUTER_API_KEY", "tutor-secret")
    ref = {"url": "https://openstax.org/books/algebra", "title": "Algebra", "markdown": "Equation exercises"}
    search = tinyfish_api([ref, {**ref, "url": "javascript:alert(1)"}, {**ref, "url": "http://plain.example/x"}])
    monkeypatch.setattr(watch, "http_request", search)
    app = LearningApp(tmp_path)
    s = app.dispatch({"action": "create", "topic": "Algebra", "level": "Grade 7"})
    app.dispatch({"action": "save", "work": {"ideas": "private learner work"}})
    reply = {"problem": "Solve 3x = 12.", "feedback": "Try this.", "step": "", "status": "continue"}
    with patch("urllib.request.urlopen", side_effect=[
        response({"choices": [{"finish_reason": "stop", "message": {"content": json.dumps(reply)}}]}),
    ]) as call:
        app.dispatch({"action": "generate"})
    sent = json.dumps([urllib.parse.unquote_plus(c["url"]) for c in search.sent] + [c["body"] for c in search.sent])
    assert "Grade 7" in sent and "private learner work" not in sent
    assert "Equation exercises" in call.call_args_list[0].args[0].data.decode()
    event = Store(tmp_path).get(s["id"])["events"][-1]
    assert event["sources"] == [{"title": "Algebra", "url": ref["url"]}]       # https only


def test_search_is_subject_neutral(monkeypatch):
    """Any topic at any level: no site allowlist, only topic and level sent, three pages read."""
    monkeypatch.setenv("TINYFISH_API_KEY", "tf-secret")
    ref = {"title": "T", "markdown": "text"}
    search = tinyfish_api([{**ref, "url": f"https://{c}.example"} for c in "abcde"])
    monkeypatch.setattr(watch, "http_request", search)
    sources, note = search_problems("Rust ownership and borrowing", "senior backend engineer, new to Rust")
    query = urllib.parse.parse_qs(urllib.parse.urlsplit(search.sent[0]["url"]).query)
    assert len(sources) == 3 and "found with TinyFish" in note
    assert set(query) == {"query"}                                     # no include_domains, nothing else
    assert "Rust ownership" in query["query"][0] and "senior backend engineer" in query["query"][0]
    assert "Math" not in query["query"][0]
    assert search.sent[1]["body"]["urls"] == ["https://a.example", "https://b.example", "https://c.example"]
    assert search.sent[1]["body"]["format"] == "markdown"


def test_demo_never_searches(monkeypatch, tmp_path):
    monkeypatch.setenv("TINYFISH_API_KEY", "tf-secret")
    monkeypatch.setattr(watch, "http_request", lambda *a, **k: pytest.fail("the demo never searches"))
    app = LearningApp(tmp_path, demo=True)
    app.dispatch({"action": "create", "topic": "Counting"})
    with patch("urllib.request.urlopen") as call:
        app.dispatch({"action": "generate"})
    call.assert_not_called()


def test_env_file_supplies_the_tinyfish_key_to_standalone_launcher(monkeypatch, tmp_path):
    """tools/start_learning.py and `python -m eve_bridge.learning` go through server.main,
    which loads the env file before the server starts and before any search runs."""
    from eve_bridge.learning import server

    env_file = tmp_path / "env"
    env_file.write_text("TINYFISH_API_KEY=tf-from-file\n")
    # delenv records the keys as absent, so teardown removes what load_env_file adds.
    monkeypatch.delenv("TINYFISH_API_KEY", raising=False)
    monkeypatch.delenv("EVE_ENV_FILE", raising=False)
    seen = {}

    def fake_start(*args, **kwargs):
        seen["key"] = os.environ.get("TINYFISH_API_KEY")
        raise OSError(errno.EACCES, "stop before serving")

    with patch.object(server, "start", side_effect=fake_start):
        with pytest.raises(OSError):
            server.main(["--env-file", str(env_file), "--data-dir", str(tmp_path), "--no-open"])
    assert seen["key"] == "tf-from-file"
    search = tinyfish_api([])
    monkeypatch.setattr(watch, "http_request", search)
    search_problems("Fractions", "Grade 4")
    assert search.sent[0]["headers"]["X-API-Key"] == "tf-from-file"


def test_daemon_cli_loads_env_before_subcommands(monkeypatch):
    """The launchd daemon and `eve-bridge learning` both start in cli.main, which loads the env file."""
    from eve_bridge import cli

    loader = Mock()
    monkeypatch.setattr(cli, "load_env_file", loader)
    with patch("eve_bridge.learning.server.main", return_value=0) as learning_main:
        assert cli.main(["learning", "--no-open"]) == 0
    loader.assert_called_once()
    learning_main.assert_called_once()
