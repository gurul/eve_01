"""Offline adapter checks; SSE/error cases are synthetic, based on an observed first turn."""

from __future__ import annotations

import asyncio
import copy
import json
from dataclasses import replace
from datetime import datetime
from pathlib import Path
from unittest.mock import Mock

import httpx
import pytest

from eve_bridge import america

ARGS = {"question": "Where can I read the official United States Code?", "jurisdiction": "U.S. federal"}
SPEC_PATH = Path(__file__).resolve().parents[2] / "docs/contracts/america-gov.openapi.json"
SOURCE = {"title": "United States Code | GovInfo", "url": "https://www.govinfo.gov/help/uscode", "currency": "unverified"}
ANSWER = "Read the Code — including current supplements — at [OLRC](https://uscode.house.gov/)."
HEADERS = {"content-type": "text/event-stream", "x-vercel-ai-ui-message-stream": "v1"}


def events():
    return [
        {"type": "start", "messageId": "answer", "messageMetadata": {"interruptedSignature": "opaque"}},
        {"type": "message-metadata", "messageMetadata": {"generation": {"america": {"encrypted": "private-opaque"}}}},
        {"type": "reasoning-start", "id": "reasoning"},
        {"type": "reasoning-end", "id": "reasoning"},
        {"type": "tool-input-available", "toolCallId": "ground", "input": {"america": {"encrypted": "private-opaque"}}},
        {"type": "data-search", "data": {"status": "done", "groundings": [copy.deepcopy(SOURCE)],
                                         "sealed": {"america": {"encrypted": "private-opaque"}}}},
        {"type": "data-search", "data": {"status": "done", "groundings": [copy.deepcopy(SOURCE)]}},
        {"type": "text-start", "id": "text"},
        {"type": "text-delta", "id": "text", "delta": ANSWER[:16]},
        {"type": "text-delta", "id": "text", "delta": ANSWER[16:]},
        {"type": "text-end", "id": "text"},
        {"type": "finish", "messageMetadata": {"answerComplete": True}},
        {"type": "message-metadata", "messageMetadata": {"conversationEnded": False, "safetyCard": None,
                                                          "followUpSuggestions": ["Learn about supplements"]}},
        {"type": "message-metadata", "messageMetadata": {"diagnostics": {"outcome": "complete"}}},
        {"type": "message-metadata", "messageMetadata": {"historySignature": "private-opaque"}},
        "[DONE]",
    ]


def sse(items=None):
    return "".join(f"data: {item if isinstance(item, str) else json.dumps(item, ensure_ascii=False)}\n\n"
                   for item in (events() if items is None else items)).encode("utf-8")


def handle(args=ARGS, *, config=None, name="america_ask", response=None, transport=None):
    if transport is None:
        transport = httpx.MockTransport(lambda request: response if response is not None else httpx.Response(200, headers=HEADERS, content=sse()))
    selected = config or america.AmericaConfig(True)
    selected = replace(selected, transport="api")
    return asyncio.run(america.America(selected, transport=transport).handle(name, args))


def assert_failure(result, status="upstream_unavailable"):
    assert result["ok"] is False and result["status"] == status
    assert set(result) == {"ok", "status", "reason", "america_url"}
    assert result["america_url"] == "https://america.gov/chat"
    assert "private" not in json.dumps(result)


def test_feature_is_explicitly_opt_in_and_has_no_startup_request(monkeypatch):
    monkeypatch.setenv("EVE_AMERICA", "1")
    client = Mock(side_effect=AssertionError("network attempted"))
    monkeypatch.setattr(httpx, "AsyncClient", client)
    # Positive control confirms the monitored client really rejects construction.
    with pytest.raises(AssertionError, match="network attempted"):
        httpx.AsyncClient()
    client.reset_mock()
    assert america.make_america({}) is None
    assert america.make_america({"EVE_AMERICA": "0"}) is None
    for flag in ("1", "true", " yes ", "ON"):
        assert america.make_america({"EVE_AMERICA": flag}).config.enabled
    assert america.make_america() is not None
    service = america.America(america.AmericaConfig())
    assert service.tools() == [] and service.instructions() == ""
    assert_failure(asyncio.run(service.handle("america_ask", ARGS)), "unavailable")
    client.assert_not_called()


def test_tool_returns_independent_strict_schema_copies():
    service = america.America(america.AmericaConfig(True))
    tool = service.tools()[0]
    assert tool["strict"] and tool["parameters"]["additionalProperties"] is False
    assert set(tool["parameters"]["required"]) == set(tool["parameters"]["properties"]) == set(ARGS)
    tool["parameters"]["properties"]["question"]["maxLength"] = 99
    assert service.tools()[0]["parameters"]["properties"]["question"]["maxLength"] == america.MAX_QUESTION_CHARS


@pytest.mark.parametrize("args", [
    None, [], {}, {"question": "q"}, {**ARGS, "history": ["private memory"]},
    {**ARGS, "attachments": ["private.pdf"]}, {**ARGS, "question": None},
    {**ARGS, "question": 123}, {**ARGS, "question": ""}, {**ARGS, "question": "   "},
    {**ARGS, "question": "q" * (america.MAX_QUESTION_CHARS + 1)}, {**ARGS, "jurisdiction": 123},
    {**ARGS, "jurisdiction": []}, {**ARGS, "jurisdiction": "j" * (america.MAX_JURISDICTION_CHARS + 1)},
    {**ARGS, "jurisdiction": ""},
])
def test_invalid_requests_do_not_send_or_answer(args):
    request = Mock(side_effect=AssertionError("network attempted"))
    assert_failure(handle(args, transport=httpx.MockTransport(request)), "invalid_request")
    request.assert_not_called()


def test_unknown_tool_is_rejected_without_http():
    request = Mock(side_effect=AssertionError("network attempted"))
    assert_failure(handle(name="america_submit", transport=httpx.MockTransport(request)), "invalid_request")
    request.assert_not_called()


@pytest.mark.parametrize("jurisdiction", [None, "   "])
def test_unknown_jurisdiction_requests_clarification_without_http(jurisdiction):
    request = Mock(side_effect=AssertionError("network attempted"))
    result = handle({**ARGS, "jurisdiction": jurisdiction}, transport=httpx.MockTransport(request))
    assert_failure(result, "needs_jurisdiction")
    assert "state or city" in result["reason"] and "federal" in result["reason"]
    request.assert_not_called()


def test_real_request_shape_and_unicode_answer_with_sources():
    requests = []

    def respond(request):
        requests.append(request)
        return httpx.Response(200, headers=HEADERS, content=sse())

    result = handle({"question": " Where are\n the rules? ", "jurisdiction": " U.S.  federal "},
                    transport=httpx.MockTransport(respond))
    assert result["ok"] is True and result["status"] == "answered" and result["provider"] == "america_gov"
    assert result["answer"] == ANSWER and result["sources"] == [SOURCE]
    assert result["question"] == "Where are\n the rules?" and result["jurisdiction"] == "U.S. federal"
    assert result["legal_advice"] is False and datetime.fromisoformat(result["retrieved_at"]).utcoffset().total_seconds() == 0
    assert "private-opaque" not in json.dumps(result)
    assert len(requests) == 1
    request = requests[0]
    assert request.method == "POST" and str(request.url) == america.API_URL
    assert request.headers["accept"] == "text/event-stream" and request.headers["user-agent"].startswith("Eve/")
    assert "cookie" not in request.headers and "authorization" not in request.headers
    body = json.loads(request.content)
    assert set(body) == {"language", "id", "messages", "trigger"}
    assert body["language"] == "en" and body["trigger"] == "submit-message"
    assert len(body["messages"]) == 1
    message = body["messages"][0]
    assert set(message) == {"id", "role", "parts"} and message["role"] == "user"
    assert message["parts"] == [{"type": "text", "text": "Jurisdiction: U.S. federal\nQuestion: Where are\n the rules?"}]
    assert len(body["id"]) == len(message["id"]) == 16
    assert body["id"] != message["id"]
    assert america.chat_request("q", "California")["id"] != body["id"]


def test_maximum_inputs_are_sent_in_full_without_query_truncation():
    bodies = []

    def respond(request):
        bodies.append(json.loads(request.content))
        return httpx.Response(200, headers=HEADERS, content=sse())

    question, jurisdiction = "q" * america.MAX_QUESTION_CHARS, "j" * america.MAX_JURISDICTION_CHARS
    assert handle({"question": question, "jurisdiction": jurisdiction}, transport=httpx.MockTransport(respond))["ok"]
    assert bodies[0]["messages"][0]["parts"][0]["text"] == f"Jurisdiction: {jurisdiction}\nQuestion: {question}"


class ByteStream(httpx.AsyncByteStream):
    def __init__(self, data, *, delay=0, error=None):
        self.data, self.delay, self.error, self.closed = data, delay, error, False

    async def __aiter__(self):
        # Split multibyte Unicode and SSE frames to exercise arbitrary transport chunks.
        for start in range(0, len(self.data), 7):
            if self.delay:
                await asyncio.sleep(self.delay)
            yield self.data[start:start + 7]
        if self.error:
            raise self.error

    async def aclose(self):
        self.closed = True


def test_chunked_utf8_crlf_stream_is_consumed_through_done_and_closed():
    stream = ByteStream(sse().replace(b"\n", b"\r\n"))
    result = handle(response=httpx.Response(200, headers=HEADERS, stream=stream))
    assert result["answer"] == ANSWER and result["sources"] == [SOURCE] and stream.closed


@pytest.mark.parametrize("status", [301, 302, 307, 403, 418, 429, 500, 503])
def test_http_blockage_and_redirects_are_not_retried_or_followed(status):
    requests = []

    def respond(request):
        requests.append(request)
        return httpx.Response(status, headers={"location": "https://other.gov/private"}, content=b"private body and cookie")

    result = handle(transport=httpx.MockTransport(respond))
    assert_failure(result)
    assert str(status) in result["reason"] and len(requests) == 1


@pytest.mark.parametrize("headers", [
    {"content-type": "text/html"}, {"content-type": "application/json"},
    {"content-type": "text/event-stream"}, {**HEADERS, "x-vercel-ai-ui-message-stream": "v2"},
])
def test_unsupported_transport_is_a_handoff(headers):
    assert_failure(handle(response=httpx.Response(200, headers=headers, content=sse())))


@pytest.mark.parametrize("error", [httpx.ConnectError("private credentials"), httpx.ReadTimeout("private body")])
def test_transport_errors_do_not_leak_provider_or_user_data(error):
    def respond(request):
        raise error

    assert_failure(handle(transport=httpx.MockTransport(respond)))


def test_truncated_network_response_discards_partial_text_and_closes():
    stream = ByteStream(sse(), error=httpx.ReadError("private partial data"))
    assert_failure(handle(response=httpx.Response(200, headers=HEADERS, stream=stream)))
    assert stream.closed


def test_total_deadline_cancels_dribbling_stream_and_closes_client():
    stream = ByteStream(sse(), delay=0.02)
    assert_failure(handle(config=america.AmericaConfig(True, 0.01),
                          response=httpx.Response(200, headers=HEADERS, stream=stream)))
    assert stream.closed


@pytest.mark.parametrize("bad_stream", [
    sse(events()[:-1]),
    sse([event for event in events() if not isinstance(event, dict) or event["type"] != "finish"]),
    sse(events()[:-1] + [{"type": "error", "errorText": "private error"}, "[DONE]"]),
    sse(events()[:-1] + [{"type": "abort"}, "[DONE]"]),
    sse(events() + [{"type": "text-delta", "id": "text", "delta": "extra"}]),
    b"data: {invalid}\n\n",
    b"data: []\n\n",
    b"data: {\"type\": \"text-delta\", \"delta\": \"\xff\"}\n\n",
    b"data: [DONE]\n\n",
])
def test_incomplete_or_error_streams_cannot_become_legal_answers(bad_stream):
    assert_failure(handle(response=httpx.Response(200, headers=HEADERS, content=bad_stream)))


@pytest.mark.parametrize("kind,updates", [
    ("finish", {"messageMetadata": {"answerComplete": False}}),
    ("finish", {"messageMetadata": {"answerComplete": 1}}),
    ("finish", {"messageMetadata": {}}),
    ("text-start", {"id": None}),
    ("text-delta", {"id": "unknown"}),
    ("text-delta", {"delta": None}),
    ("text-end", {"id": "unknown"}),
    ("data-search", {"data": {"status": "done", "groundings": None}}),
    ("data-search", {"data": None}),
    ("message-metadata", {"messageMetadata": {"safetyCard": {"private": "unsupported"}}}),
])
def test_malformed_or_safety_events_do_not_return_an_answer(kind, updates):
    items = events()
    for event in items:
        if isinstance(event, dict) and event["type"] == kind:
            event.update(updates)
    assert_failure(handle(response=httpx.Response(200, headers=HEADERS, content=sse(items))))


@pytest.mark.parametrize("kind", ["text-end", "text-delta", "data-search"])
def test_missing_answer_or_groundings_is_not_success(kind):
    items = [event for event in events() if not isinstance(event, dict) or event["type"] != kind]
    assert_failure(handle(response=httpx.Response(200, headers=HEADERS, content=sse(items))))


def test_limits_refuse_oversized_response_and_answer_without_truncating():
    assert_failure(handle(response=httpx.Response(200, headers=HEADERS, content=b"x" * (america.MAX_STREAM_BYTES + 1))))
    items = events()
    items[8]["delta"] = "x" * (america.MAX_ANSWER_CHARS + 1)
    assert_failure(handle(response=httpx.Response(200, headers=HEADERS, content=sse(items))))


@pytest.mark.parametrize("url", [
    "http://agency.gov/info", "https://agency.gov.attacker.test/", "https://agency.gov@attacker.test/",
    "https://attacker@agency.gov/", "https://agency.gov:8443/", "https://127.0.0.1/",
    "javascript:alert(1)", "https://agency.gov/has space", "https://agency.gov:invalid/",
])
def test_unsafe_sources_are_filtered_with_a_known_good_positive_control(url):
    items = events()
    for event in items:
        if isinstance(event, dict) and event["type"] == "data-search":
            event["data"]["groundings"].append({**SOURCE, "url": url})
    result = handle(response=httpx.Response(200, headers=HEADERS, content=sse(items)))
    assert result["sources"] == [SOURCE]
    for event in items:
        if isinstance(event, dict) and event["type"] == "data-search":
            event["data"]["groundings"] = [{**SOURCE, "url": url}]
    assert_failure(handle(response=httpx.Response(200, headers=HEADERS, content=sse(items))))


def test_multiple_text_blocks_preserve_order_and_markdown_links():
    items = events()
    items[11:11] = [{"type": "text-start", "id": "second"},
                    {"type": "text-delta", "id": "second", "delta": "More at [GovInfo](https://www.govinfo.gov/)."},
                    {"type": "text-end", "id": "second"}]
    assert handle(response=httpx.Response(200, headers=HEADERS, content=sse(items)))["answer"] == ANSWER + "\nMore at [GovInfo](https://www.govinfo.gov/)."


def test_overlapping_blocks_are_rejected_instead_of_reordering_text():
    items = events()
    items[9:9] = [{"type": "text-start", "id": "overlap"},
                  {"type": "text-delta", "id": "overlap", "delta": "interleaved"},
                  {"type": "text-end", "id": "overlap"}]
    assert_failure(handle(response=httpx.Response(200, headers=HEADERS, content=sse(items))))


def test_answer_limit_includes_separators_between_sequential_blocks():
    items = events()
    items[8]["delta"] = "x" * (america.MAX_ANSWER_CHARS // 2)
    items[9]["delta"] = ""
    items[11:11] = [{"type": "text-start", "id": "second"},
                    {"type": "text-delta", "id": "second", "delta": "y" * (america.MAX_ANSWER_CHARS // 2)},
                    {"type": "text-end", "id": "second"}]
    assert_failure(handle(response=httpx.Response(200, headers=HEADERS, content=sse(items))))
    items[12]["delta"] = items[12]["delta"][:-1]
    result = handle(response=httpx.Response(200, headers=HEADERS, content=sse(items)))
    assert result["ok"] and len(result["answer"]) == america.MAX_ANSWER_CHARS


def test_saved_openapi_matches_tool_and_observed_body_and_result_outcomes():
    spec = json.loads(SPEC_PATH.read_text())
    assert spec["openapi"] == "3.1.0" and spec["servers"][0]["url"] == "https://america.gov"
    operation = spec["paths"]["/api/chat"]["post"]
    assert operation["requestBody"]["content"]["application/json"]["schema"]["$ref"].endswith("/ChatRequest")
    assert operation["responses"]["200"]["content"]["text/event-stream"]
    assert spec["x-america-gov"]["api_status"] == "observed_unpublished_protocol"
    assert spec["x-eve-interface"]["transport"] == "in_process_function"
    assert spec["x-eve-interface"]["tool"] == america.TOOL
    schemas = spec["components"]["schemas"]
    assert schemas["AskRequest"] == america.TOOL["parameters"]
    body = america.chat_request("question", "California")
    assert set(schemas["ChatRequest"]["required"]) == set(body)
    assert body["language"] == schemas["ChatRequest"]["properties"]["language"]["const"]
    assert body["trigger"] == schemas["ChatRequest"]["properties"]["trigger"]["const"]
    outcomes = [handle(), handle({**ARGS, "jurisdiction": None}), handle({}),
                handle(config=america.AmericaConfig(False)), handle(response=httpx.Response(418))]
    for result in outcomes:
        schema = schemas[spec["x-eve-interface"]["result_schemas"][result["status"]]]
        assert schema["additionalProperties"] is False
        assert set(schema["required"]) <= set(result) <= set(schema["properties"])
        for name, value in result.items():
            prop = schema["properties"][name]
            if "const" in prop:
                assert value == prop["const"]
            if "enum" in prop:
                assert value in prop["enum"]
