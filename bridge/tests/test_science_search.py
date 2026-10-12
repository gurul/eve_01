"""science_search.py: Ask science, the scholarly-paper search. The tool's schema and words, the arguments (validated
and clamped), the answer's shape (clipped, compact, linked), the network failures (429, 5xx, timeouts, a refusal)
over a mocked transport, the process's own rate ceiling, the owner's words kept out of the log, the switch, the
taint gate, and the tool offered and answered in every surface web_search is (the chat brain and the app chat, out
loud, the transcript, the intent eval's labels and the tool-choice lines).

No test here touches the network except the one marked live (EVE_LIVE=1). The suite pins the switch off
(conftest._science_search_pinned_off); these tests switch it on."""
from __future__ import annotations

import asyncio
import functools
import json
import logging
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable

import httpx
import pytest

from eve_bridge import science_search as ss
from eve_bridge import tool_gate

TOOLS_DIR = Path(__file__).resolve().parents[1] / "tools"
NOW = datetime(2026, 10, 12, 12, 0, tzinfo=timezone.utc)
ARGS = {"query": "effect of intermittent fasting on weight loss in adults", "k": None, "year_from": None,
        "year_to": None, "reviews_only": None, "open_access_only": None}

# Shaped like the service's /search answer (fields from its guide); the papers are made up.
PAPER = {"id": "W100", "doi": "10.1000/example.1", "title": "Fasting and weight: a trial", "date": "2020-02-01",
         "year": 2020, "type": "article", "authors": "Ada One | Ben Two | Cy Three | Di Four | Ed Five",
         "authors_count": 7, "venue": "Journal of Examples", "publisher": "Example Press", "is_oa": 1,
         "oa_status": "gold", "license": "cc-by", "citations": 42, "is_retracted": 0,
         "url": "https://doi.org/10.1000/example.1", "pdf_url": "https://example.org/a.pdf", "topic": "Diet",
         "field": "Medicine", "keywords": "fasting", "mesh": None, "pmcid": None, "smr": 0,
         "abstract": "<jats:p>OBJECTIVE: To test.</jats:p> " + "word " * 300, "score": 0.6, "junk": 0.01,
         "openalex": "https://openalex.org/W100"}
BODY = {"mode": "semantic", "results": [PAPER], "filters": {}, "timing": {"total_ms": 300.0}}


@pytest.fixture(autouse=True)
def _on(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(ss.SWITCH, "1")
    ss.LIMITER.reset()
    yield
    ss.LIMITER.reset()


class Server:
    """A mocked transport: each request takes the next of ``replies`` (a status and body, or an exception)."""

    def __init__(self, *replies: Any) -> None:
        self.replies = list(replies)
        self.requests: list[httpx.Request] = []
        self.transport = httpx.MockTransport(self._handle)

    def _handle(self, request: httpx.Request) -> httpx.Response:
        self.requests.append(request)
        reply = self.replies.pop(0) if len(self.replies) > 1 else self.replies[0]
        if isinstance(reply, Exception):
            raise reply
        status, body, headers = reply if len(reply) == 3 else (*reply, {})
        if isinstance(body, (dict, list)):
            return httpx.Response(status, json=body, headers=headers)
        return httpx.Response(status, content=str(body).encode(), headers=headers)

    def params(self, i: int = 0) -> dict[str, str]:
        return dict(self.requests[i].url.params)


async def _no_sleep(_s: float) -> None:
    return None


def run(args: dict[str, Any], server: Server, **kw: Any) -> dict[str, Any]:
    return asyncio.run(ss.search(args, transport=server.transport, sleep=_no_sleep, now=NOW, **kw))


def with_args(**kw: Any) -> dict[str, Any]:
    out = dict(ARGS)
    out.update(kw)
    return out


# ---- the tool --------------------------------------------------------------------------------------------------

def test_the_tool_is_strict_with_every_argument_required_and_nullable() -> None:
    assert ss.TOOL_NAMES == ("science_search",) and ss.TOOLS == [ss.TOOL]
    params = ss.TOOL["parameters"]
    assert ss.TOOL["strict"] is True and params["additionalProperties"] is False
    assert sorted(params["required"]) == sorted(params["properties"]) == sorted(ARGS)
    assert params["properties"]["query"]["type"] == "string"
    for name in ("k", "year_from", "year_to", "reviews_only", "open_access_only"):
        assert "null" in params["properties"][name]["type"], name


def test_the_description_says_when_to_use_it_and_how_to_cite() -> None:
    text = ss.TOOL["description"]
    for words in ("research", "studies", "review", "DOI", "Do not use it for news", "prices", "how-to",
                  "everyday facts", "title and year", "link", "only abstracts were read", "thin"):
        assert words in text, words
    # it names no other tool: a name in a prompt is a value the model may fill a slot with (AGENTS.md rule 7)
    assert "_search" not in text and "web_search" not in text


# ---- the arguments ---------------------------------------------------------------------------------------------

def test_the_defaults_and_the_query_as_sent() -> None:
    assert ss.params(ARGS, NOW) == {"q": ARGS["query"], "k": ss.DEFAULT_K}


@pytest.mark.parametrize(("k", "sent"), [(None, 8), (0, 1), (-3, 1), (1, 1), (20, 20), (50, 20), (5.0, 5),
                                         ("12", 12), (True, 8), ("many", 8), (float("nan"), 8)])
def test_k_is_clamped_to_one_through_twenty(k: Any, sent: int) -> None:
    assert ss.params(with_args(k=k), NOW)["k"] == sent


def test_years_are_clamped_and_put_in_order() -> None:
    p = ss.params(with_args(year_from=2024, year_to=2018), NOW)
    assert (p["year_from"], p["year_to"]) == (2018, 2024)
    p = ss.params(with_args(year_from=5, year_to=99999), NOW)
    assert (p["year_from"], p["year_to"]) == (ss.MIN_YEAR, 2027)
    p = ss.params(with_args(year_from=2020), NOW)
    assert p["year_from"] == 2020 and "year_to" not in p
    assert "year_from" not in ss.params(with_args(year_from="recent"), NOW)


def test_the_flags_become_the_services_filters() -> None:
    p = ss.params(with_args(reviews_only=True, open_access_only=True), NOW)
    assert p["type"] == "review" and p["open_access"] == "true"
    p = ss.params(with_args(reviews_only=False, open_access_only=None), NOW)
    assert "type" not in p and "open_access" not in p


def test_the_query_is_one_clean_line_clipped() -> None:
    p = ss.params(with_args(query="  gut\x00 microbiome\n\tand autism\x7f  "), NOW)
    assert p["q"] == "gut microbiome and autism"
    long = ss.params(with_args(query="sleep " * 400), NOW)["q"]
    assert len(long) <= ss.MAX_QUERY_CHARS and not long.endswith(("…", " "))
    assert ss.params(with_args(query='"gut microbiome" AND (autism OR ASD) NOT mice'), NOW)["q"] == (
        '"gut microbiome" AND (autism OR ASD) NOT mice')            # the service's own syntax passes untouched


@pytest.mark.parametrize("query", ["", " ", "x", "\x00\x01", None, 7])
def test_a_query_too_short_is_refused_without_a_request(query: Any) -> None:
    server = Server((200, BODY))
    out = run(with_args(query=query), server)
    assert out["ok"] is False and "at least" in out["reason"] and server.requests == []


# ---- the answer ------------------------------------------------------------------------------------------------

def test_a_paper_is_compact_clipped_and_linked() -> None:
    p = ss.paper(PAPER)
    assert p["title"] == "Fasting and weight: a trial" and p["year"] == 2020 and p["type"] == "article"
    assert p["venue"] == "Journal of Examples" and p["citations"] == 42 and p["open_access"] is True
    assert p["authors"] == "Ada One, Ben Two, Cy Three and 4 more"         # 7 in all, 3 shown
    assert p["doi"] == "10.1000/example.1" and p["link"] == "https://doi.org/10.1000/example.1"
    assert p["page"] == "https://sciencemadereadable.com/?work=10.1000/example.1"
    assert "plain_words" not in p
    assert p["abstract"].startswith("OBJECTIVE: To test. word") and "<" not in p["abstract"]
    assert len(p["abstract"]) <= ss.ABSTRACT_CHARS and p["abstract"].endswith("…")
    # only what a model needs: no scores, junk, keywords, MeSH, PDF or publisher
    assert set(p) == {"title", "year", "type", "venue", "authors", "citations", "open_access", "doi", "link", "page",
                      "abstract"}


def test_a_paper_with_little_on_record() -> None:
    p = ss.paper({"id": "W7", "title": "Untitled\x00 note", "authors": "", "is_oa": 0, "smr": 1,
                  "openalex": "https://openalex.org/W7", "url": "javascript:alert(1)", "year": "2020",
                  "citations": True})
    assert p["title"] == "Untitled note" and p["authors"] is None and p["abstract"] is None
    assert p["doi"] is None and p["link"] == "https://openalex.org/W7"     # never a non-web url
    assert p["page"] == "https://sciencemadereadable.com/?work=W7"
    assert p["plain_words"] == "https://sciencemadereadable.com/?paper=W7"
    assert p["year"] is None and p["citations"] is None and p["open_access"] is False
    two = ss.paper({"authors": "A | B", "authors_count": 2})
    assert two["authors"] == "A, B"
    odd = ss.paper({"doi": "10.1/a b#c", "smr": 1})
    assert odd["link"] == "https://doi.org/10.1/a b#c"
    assert odd["page"] == "https://sciencemadereadable.com/?work=10.1/a%20b%23c"


def test_the_answer_says_how_it_searched_and_what_it_read() -> None:
    out = ss.shape(BODY, "q")
    assert out["ok"] is True and out["count"] == 1 and out["searched"] == "by meaning"
    assert "abstracts" in out["note"].lower() and "matching_papers" not in out
    kw = ss.shape({"mode": "keyword", "matches": 1234, "results": [PAPER, "junk", None]}, "q")
    assert kw["searched"] == "by exact words" and kw["matching_papers"] == 1234 and kw["count"] == 1
    fell = ss.shape({"mode": "semantic", "fallback": "no keyword match", "results": []}, "q")
    assert "no paper had the exact words" in fell["searched"] and fell["note"].startswith("No paper matched")
    assert ss.shape({"detail": "x"}, "q")["ok"] is False and ss.shape([], "q")["ok"] is False


def test_twenty_papers_stay_small_enough_for_a_models_context() -> None:
    out = ss.shape({"mode": "semantic", "results": [dict(PAPER, title="T" * 2000, venue="V" * 900)] * 20}, "q")
    # each paper at most about 1,400 characters even with the longest title and venue, so 20 stay under ~7k tokens
    for paper in out["results"]:
        assert len(json.dumps(paper, ensure_ascii=False)) <= 1400
    typical = ss.shape({"mode": "semantic", "results": [PAPER] * ss.DEFAULT_K}, "q")
    assert len(json.dumps(typical, ensure_ascii=False)) < 8 * 1100
    assert len(out["results"][0]["title"]) <= ss.TITLE_CHARS and len(out["results"][0]["venue"]) <= ss.VENUE_CHARS


# ---- the request -----------------------------------------------------------------------------------------------

def test_one_search_one_get_with_the_arguments() -> None:
    server = Server((200, BODY))
    out = run(with_args(k=5, year_from=2019, reviews_only=True, open_access_only=True), server)
    assert out["ok"] is True and out["results"][0]["title"] == "Fasting and weight: a trial"
    assert len(server.requests) == 1
    req = server.requests[0]
    assert req.method == "GET" and str(req.url).startswith(ss.SEARCH_URL + "?")
    assert server.params() == {"q": ARGS["query"], "k": "5", "year_from": "2019", "type": "review",
                               "open_access": "true"}
    assert "eve-bridge" in req.headers["user-agent"] and "authorization" not in req.headers


def test_a_429_is_never_retried_and_says_how_long_to_wait() -> None:
    server = Server((429, {"detail": "slow down", "retry_after": 17}, {"Retry-After": "12"}))
    out = run(ARGS, server)
    assert out["ok"] is False and out["retry_after_s"] == 12 and "12 seconds" in out["reason"]
    assert "too many" in out["reason"] and len(server.requests) == 1
    body_only = Server((429, {"retry_after": 17}))
    assert run(ARGS, body_only)["retry_after_s"] == 17
    nothing = Server((429, "busy"))
    out = run(ARGS, nothing)
    assert "a minute" in out["reason"] and "retry_after_s" not in out


@pytest.mark.parametrize("status", [500, 502, 503])
def test_a_5xx_is_retried_once(status: int) -> None:
    server = Server((status, "down"), (200, BODY))
    assert run(ARGS, server)["ok"] is True and len(server.requests) == 2
    server = Server((status, "down"))
    out = run(ARGS, server)
    assert out["ok"] is False and f"HTTP {status}" in out["reason"] and len(server.requests) == 2


def test_a_timeout_or_a_dropped_connection_is_retried_once() -> None:
    server = Server(httpx.ReadTimeout("slow"), (200, BODY))
    assert run(ARGS, server)["ok"] is True and len(server.requests) == 2
    server = Server(httpx.ConnectTimeout("slow"))
    out = run(ARGS, server)
    assert out == {"ok": False, "reason": "Ask science took too long to answer."} and len(server.requests) == 2
    server = Server(httpx.ConnectError("no route"))
    out = run(ARGS, server)
    assert out == {"ok": False, "reason": "Ask science could not be reached."} and len(server.requests) == 2


def test_no_retry_when_the_budget_is_spent(monkeypatch: pytest.MonkeyPatch) -> None:
    ticks = iter([0.0, 0.0, ss.BUDGET_SECS - 1.0, ss.BUDGET_SECS])
    monkeypatch.setattr(ss.time, "monotonic", lambda: next(ticks, ss.BUDGET_SECS))
    server = Server(httpx.ReadTimeout("slow"), (200, BODY))
    out = run(ARGS, server, limiter=ss._Limiter(clock=lambda: 0.0))
    assert out["ok"] is False and len(server.requests) == 1


def test_a_refusal_says_why_and_is_not_retried() -> None:
    server = Server((422, {"detail": [{"msg": "q: String should have at most 2000 characters"}]}))
    out = run(ARGS, server)
    assert out["ok"] is False and "HTTP 422" in out["reason"] and "2000 characters" in out["reason"]
    assert len(server.requests) == 1
    assert run(ARGS, Server((404, "<html>")))["reason"] == "Ask science refused the search (HTTP 404)."


def test_an_answer_that_is_not_json_is_said_plainly() -> None:
    out = run(ARGS, Server((200, "<html>maintenance</html>")))
    assert out == {"ok": False, "reason": "Ask science sent an answer eve could not read."}


def test_the_process_keeps_under_its_own_ceiling() -> None:
    clock = {"t": 0.0}
    limiter = ss._Limiter(clock=lambda: clock["t"])
    server = Server((200, BODY))
    for _ in range(ss.MAX_PER_MINUTE):
        assert run(ARGS, server, limiter=limiter)["ok"] is True
    out = run(ARGS, server, limiter=limiter)
    assert out["ok"] is False and out["retry_after_s"] == 60 and len(server.requests) == ss.MAX_PER_MINUTE
    clock["t"] = ss.RATE_WINDOW_SECS
    assert run(ARGS, server, limiter=limiter)["ok"] is True
    # a retry is a request too: it takes a slot, and with none left the first failure is the answer
    full = ss._Limiter(clock=lambda: 0.0)
    for _ in range(ss.MAX_PER_MINUTE - 1):
        full.take()
    flaky = Server((503, "down"), (200, BODY))
    out = run(ARGS, flaky, limiter=full)
    assert out["ok"] is False and "HTTP 503" in out["reason"] and len(flaky.requests) == 1


def test_the_owners_words_never_reach_the_log(caplog: pytest.LogCaptureFixture) -> None:
    secret = "zebrafish fin regeneration"
    logging.getLogger("httpx").setLevel(logging.INFO)
    caplog.set_level(logging.DEBUG)
    server = Server((200, BODY))
    assert run(with_args(query=secret), server)["ok"] is True
    run(with_args(query=secret), Server((500, "x")))
    text = "\n".join(r.getMessage() for r in caplog.records)
    assert "zebrafish" not in text and "science search:" in text
    # httpx's request line, as httpx writes it: the q value hidden, only on this service's URLs
    record = logging.LogRecord("httpx", logging.INFO, __file__, 1, 'HTTP Request: GET %s "%s"',
                               (f"{ss.SEARCH_URL}?q=zebrafish+fin&k=8", "HTTP/1.1 200 OK"), None)
    ss.QueryFilter().filter(record)
    assert "zebrafish" not in record.getMessage() and "q=<hidden>&k=8" in record.getMessage()
    other = logging.LogRecord("httpx", logging.INFO, __file__, 1, "GET https://example.org/?q=zebrafish", (), None)
    ss.QueryFilter().filter(other)
    assert "zebrafish" in other.getMessage()
    assert any(isinstance(f, ss.QueryFilter) for f in logging.getLogger("httpx").filters)


# ---- the switch ------------------------------------------------------------------------------------------------

def test_it_ships_on_and_the_switch_turns_it_off(monkeypatch: pytest.MonkeyPatch) -> None:
    assert ss.enabled({}) and ss.tools({}) == [ss.TOOL]
    for on in ("1", "true", "on", "yes", ""):
        assert ss.enabled({ss.SWITCH: on})
    server = Server((200, BODY))
    for off in ("0", "false", "no", "off", " OFF "):
        assert not ss.enabled({ss.SWITCH: off}) and ss.tools({ss.SWITCH: off}) == []
        monkeypatch.setenv(ss.SWITCH, off)
        out = asyncio.run(ss.handle("science_search", ARGS, transport=server.transport))
        assert out == {"ok": False, "reason": ss.OFF_REASON}
    assert server.requests == []


def test_handle_refuses_other_names_and_non_objects() -> None:
    assert asyncio.run(ss.handle("web_search", ARGS))["ok"] is False
    assert asyncio.run(ss.handle("science_search", ["q"]))["ok"] is False  # type: ignore[arg-type]


# ---- the taint gate and the transcript -------------------------------------------------------------------------

def test_the_taint_gate_reads_it_as_outside_text_that_never_asks() -> None:
    assert not tool_gate.acting("science_search", ARGS)
    assert not tool_gate.needs_confirmation("science_search", ARGS, "a web search")
    assert tool_gate.taints("science_search", ARGS) and tool_gate.taints("science_search", {})
    assert tool_gate.source("science_search") == "a search of research papers"
    assert "science_search" not in tool_gate.CLEAN


def test_its_results_are_kept_like_web_searchs_and_marked_as_outside_words() -> None:
    from eve_bridge import telegram, transcripts, voice_agent

    assert "science_search" in telegram.TRANSCRIBED_TOOLS and "science_search" in voice_agent.ANSWER_TOOLS
    assert transcripts._outside_words({"kind": "tool", "tool": "science_search", "text": "a title"})


# ---- every surface web_search is in ----------------------------------------------------------------------------

def _mock_service(monkeypatch: pytest.MonkeyPatch, server: Server) -> None:
    monkeypatch.setattr(ss, "search", functools.partial(ss.search, transport=server.transport, sleep=_no_sleep))


def test_the_chat_brain_offers_and_answers_it_and_the_turn_is_tainted(monkeypatch: pytest.MonkeyPatch) -> None:
    from test_telegram import OWNER, FakeApi, FakeCreate, Rig, call, run_rig, say, update

    server = Server((200, BODY))
    _mock_service(monkeypatch, server)
    rig = Rig(FakeApi([update("what does the research say about fasting")]),
              FakeCreate(call("science_search", ARGS), say("One 2020 trial found ...")))
    run_rig(rig)
    first = rig.create.requests[0]
    assert [t for t in first["tools"] if t.get("name") == "science_search"] == [ss.TOOL]
    out = [json.loads(i["output"]) for i in rig.create.requests[1]["input"] if i.get("type") == "function_call_output"]
    assert out and out[-1]["ok"] is True and out[-1]["results"][0]["year"] == 2020
    assert len(server.requests) == 1
    assert rig.inlet._taint == "a search of research papers"
    assert rig.api.sent[-1] == (OWNER, "One 2020 trial found ...")


def test_the_chat_brain_off_neither_offers_nor_answers(monkeypatch: pytest.MonkeyPatch) -> None:
    from test_telegram import OWNER, FakeApi, FakeCreate, Rig, run_rig, say, update

    monkeypatch.setenv(ss.SWITCH, "0")
    rig = Rig(FakeApi([update("what does the research say about fasting")]), FakeCreate(say("ok")))
    run_rig(rig)
    assert "science_search" not in [t.get("name") for t in rig.create.requests[0]["tools"]]
    assert asyncio.run(rig.inlet._tool("science_search", ARGS, OWNER)) == {"ok": False, "reason": ss.OFF_REASON}


def test_the_tool_choice_line_rides_only_with_both_searches() -> None:
    from eve_bridge import telegram

    both = telegram.intent_block({"web_search", "science_search"})
    assert "is science_search" in both and "stay with web_search" in both
    assert "science_search" not in telegram.intent_block({"web_search"})
    assert "science_search" not in telegram.intent_block({"science_search"})


def test_out_loud_it_is_offered_never_in_a_lesson_and_answered(monkeypatch: pytest.MonkeyPatch) -> None:
    from test_voice_agent import FakeAgent, FakeConnection, _eventually, _session, _tool_call

    from eve_bridge import voice_agent
    from eve_bridge.voice_agent import VoiceConfig

    names: Callable[[dict[str, Any]], list[Any]] = lambda cfg: [  # noqa: E731
        t.get("name") for t in cfg["delegation"]["responses"]["tools"]]
    assert "science_search" in names(voice_agent.session_config(VoiceConfig()))
    lesson = voice_agent.session_config(VoiceConfig(), think_aloud={"id": "l1", "topic": "fractions"})
    assert "science_search" not in names(lesson)
    monkeypatch.setenv(ss.SWITCH, "0")
    assert "science_search" not in names(voice_agent.session_config(VoiceConfig()))
    monkeypatch.setenv(ss.SWITCH, "1")

    server = Server((200, BODY))
    _mock_service(monkeypatch, server)
    conn = FakeConnection([_tool_call("science_search", "c1", **ARGS)])
    s, _, _ = _session(conn, [FakeAgent(None, None)])

    async def go() -> None:
        task = asyncio.create_task(s.run())
        await _eventually(lambda: bool(conn.tool_outputs()))
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    out = conn.tool_outputs()[0]
    assert out["ok"] is True and out["results"][0]["title"] == "Fasting and weight: a trial"
    assert s._taint == "a search of research papers"


def _intent_eval() -> Any:
    sys.path.insert(0, str(TOOLS_DIR))
    try:
        import intent_eval
    finally:
        sys.path.remove(str(TOOLS_DIR))
    return intent_eval


def test_the_intent_eval_has_its_label() -> None:
    ie = _intent_eval()
    assert "science_search" in ie.LABELS and ie.TOOL_LABELS["science_search"] == "science_search"
    assert ie.label("science_search", ARGS) == "science_search"
    assert "science_search" not in ie.acting_only_labels() and "science_search" not in ie.PREP


# ---- the live service ------------------------------------------------------------------------------------------

@pytest.mark.live
def test_live_one_real_search() -> None:
    """EVE_LIVE=1: one real search against the public service (one of its 30 a minute)."""
    out = asyncio.run(ss.search(with_args(query="effect of intermittent fasting on weight loss in adults", k=3)))
    assert out["ok"] is True, out
    assert 1 <= out["count"] <= 3 and all(p["title"] and p["link"] for p in out["results"])
    assert all(p["page"].startswith(ss.SITE) for p in out["results"])
