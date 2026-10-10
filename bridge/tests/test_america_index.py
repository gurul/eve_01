"""Offline government-index contract, failure boundaries and shared cap checks."""

from __future__ import annotations

import json
from datetime import datetime, timezone
from unittest.mock import Mock

import pytest

from eve_bridge import america_index as index
from eve_bridge import firecrawl, watch

QUESTION = "Where can I read the United States Code?"
JURISDICTION = "U.S. federal"
URL = "https://uscode.house.gov/currency/currency.shtml"
PAGE = "<html><nav>Ignore this</nav><main><h1>United States Code</h1><p>Official page text.</p></main></html>"


@pytest.fixture(autouse=True)
def offline(monkeypatch):
    meter = Mock()
    monkeypatch.setattr(firecrawl, "meter", meter)
    monkeypatch.setattr(watch, "http_request", Mock(side_effect=AssertionError("unexpected network request")))
    return meter


def row(url=URL, **changes):
    return {"url": url, "title": "United States Code", "snippet": "Indexed official snippet.",
            "updated_at": 1787585414000, **changes}


def reply(*rows, credits=2):
    return {"data": {"results": len(rows), "data": list(rows)}, "credits": credits}


def service(*rows, fetch=None, **kwargs):
    runner = Mock(return_value=reply(*rows))
    fetch = fetch if fetch is not None else Mock(return_value=(200, PAGE))
    return index.GovernmentIndex(runner=runner, fetch=fetch, **kwargs), runner, fetch


def test_fixed_single_query_returns_source_evidence_and_meters_known_credits(offline, monkeypatch):
    monkeypatch.setenv("EVE_FIRECRAWL_USD", "0.004")
    lookup, runner, fetch = service(row())
    result = lookup.query(" " + QUESTION + " ", " " + JURISDICTION + " ")
    request = runner.call_args.args[0]
    assert request == {"provider": "firecrawl-gov-index", "capability": "search", "options": {
        "query": "Jurisdiction: U.S. federal\nQuestion: " + QUESTION, "top_k": 5, "full_content": False}}
    runner.assert_called_once()
    assert 0 < runner.call_args.kwargs["timeout"] <= firecrawl.TOOL_TIMEOUT_SECS
    fetch.assert_called_once_with(URL, timeout=fetch.call_args.kwargs["timeout"])
    assert 0 < fetch.call_args.kwargs["timeout"] <= index.MAX_READ_SECS
    offline.assert_called_once_with("alexandria firecrawl-gov-index", 2, "america", 0.004)
    assert result["ok"] and result["status"] == "sources_found" and "answer" not in result
    assert result["provider"] == "firecrawl_gov_index"
    assert result["question"] == QUESTION and result["jurisdiction"] == JURISDICTION
    assert result["legal_advice"] is False
    source = result["sources"][0]
    assert source == {"title": "United States Code", "url": URL, "currency": "unverified",
                      "excerpt": "Indexed official snippet.", "page_text": "United States Code Official page text.",
                      "read_status": "live", "text_truncated": False,
                      "indexed_at": "2026-08-24T15:30:14.000Z", "fetched_at": source["fetched_at"]}
    now = datetime.now(timezone.utc)
    for stamp in (source["fetched_at"], result["retrieved_at"]):
        assert 0 <= (now - datetime.fromisoformat(stamp)).total_seconds() < 5


def test_sessions_tracking_fragments_and_query_order_are_canonical_before_read_or_dedup():
    base = "https://www.irs.gov/forms?form=1040&year=2025"
    lookup, runner, fetch = service(
        row("https://WWW.IRS.GOV:443/forms;jsessionid=one?year=2025&utm_source=mail&form=1040#section"),
        row("https://www.irs.gov/forms%3Bjsessionid%3Dtwo?form=1040&year=2025&SESSION_ID=private"),
        row(base + "&fbclid=tracker"), row(base + "&year=2024"),
    )
    result = lookup.query(QUESTION, JURISDICTION)
    assert [s["url"] for s in result["sources"]] == [base, base + "&year=2024"]
    assert [call.args[0] for call in fetch.call_args_list] == [base, base + "&year=2024"]
    assert runner.call_count == 1
    assert "private" not in json.dumps(result)


@pytest.mark.parametrize("value", [
    "http://www.irs.gov/a", "https://www.irs.gov.evil.example/a", "https://gov/a",
    "https://evil.example/a", "https://user:secret@www.irs.gov/a", "https://@www.irs.gov/a",
    "https://www.irs.gov:8443/a", "https://127.0.0.1/a", "https://[::1]/a", "https://localhost/a",
    "https://192.168.1.1/a", "file:///tmp/a", "https://www.irs.gov/a\\b",
    "https://www.irs.gov/a\nb", "https://www.irs.gov/a%00b", "https://www.irs.gov/a%5Cb",
    "https://www.irs.gov/a%xx", "https://www.irs.gov/a?q=%0a", "https://bad_host.gov/a",
    "https://www.irs.gov../a", "https://[broken/a", "https://www.irs.gov:bad/a", None, {},
])
def test_invalid_nonofficial_credential_or_private_literal_links_are_not_read(value):
    # A real official positive control prevents an absence-only assertion.
    lookup, _, fetch = service(row(value), row("https://www.army.mil/legal"))
    result = lookup.query(QUESTION, JURISDICTION)
    assert [s["url"] for s in result["sources"]] == ["https://www.army.mil/legal"]
    assert fetch.call_count == 1 and fetch.call_args.args == ("https://www.army.mil/legal",)
    assert index.official_guard(value)
    assert index.official_guard("https://www.army.mil/legal") == ""


def test_canonicalization_retains_statute_identifiers_repeated_values_and_encoded_path():
    assert index.canonical_url("https://uscode.house.gov/%7Ecode%20page?title=42&section=1983&id=x&sid=y&x=b&x=a") == (
        "https://uscode.house.gov/~code%20page?id=x&section=1983&sid=y&title=42&x=b&x=a")
    assert index.canonical_url("https://www.irs.gov.") == "https://www.irs.gov/"


def test_canonical_unicode_query_expansion_respects_final_url_limit():
    assert index.canonical_url("https://www.irs.gov/?query=benefits") == "https://www.irs.gov/?query=benefits"
    value = "https://www.irs.gov/?query=" + "\u00e9" * 400
    assert len(value) < 2000 and index.canonical_url(value) is None
    lookup, _, fetch = service(row(value), row())
    assert [s["url"] for s in lookup.query(QUESTION, JURISDICTION)["sources"]] == [URL]
    assert fetch.call_count == 1


def test_only_first_five_rows_and_three_unique_sources_can_be_read():
    lookup, runner, fetch = service(*(row(f"https://www.irs.gov/page/{n}") for n in range(20)))
    result = lookup.query(QUESTION, JURISDICTION)
    assert len(result["sources"]) == fetch.call_count == 3
    assert [s["url"] for s in result["sources"]] == [f"https://www.irs.gov/page/{n}" for n in range(3)]
    assert runner.call_count == 1
    lookup, _, fetch = service(*(row("https://evil.example/page") for _ in range(5)), row())
    with pytest.raises(index.IndexFailure, match="no usable official sources"):
        lookup.query(QUESTION, JURISDICTION)
    fetch.assert_not_called()


def test_source_text_limits_and_untrusted_index_content_are_bounded():
    lookup, _, _ = service(row(title="t" * 1000, snippet="e" * 3000, content="PRIVATE_IGNORED_CONTENT"),
                           fetch=Mock(return_value=(200, "<main>" + "p" * 9000 + "</main>")))
    source = lookup.query(QUESTION, JURISDICTION)["sources"][0]
    assert len(source["title"]) == 500
    assert len(source["excerpt"]) == 2000 and len(source["page_text"]) == 6000
    assert source["text_truncated"] is True and source["currency"] == "unverified"
    assert "PRIVATE_IGNORED_CONTENT" not in json.dumps(source)


@pytest.mark.parametrize("fetch", [
    Mock(side_effect=watch.FetchError("PRIVATE_QUESTION Bearer PRIVATE_KEY provider account error")),
    Mock(return_value=(403, "PRIVATE_KEY")), Mock(return_value=(200, "<script>hidden</script>")),
    Mock(return_value=(200, {})), Mock(return_value=(True, PAGE)),
    Mock(return_value=(200, "x" * (watch.MAX_BYTES + 1))),
])
def test_failed_or_unreadable_fresh_reads_keep_indexed_excerpt_without_freshness_claim(fetch, caplog):
    fetch.reset_mock()
    lookup, runner, _ = service(row(), fetch=fetch)
    source = lookup.query(QUESTION, JURISDICTION)["sources"][0]
    assert source["excerpt"] == "Indexed official snippet."
    assert source["page_text"] == "" and source["fetched_at"] is None
    assert source["read_status"] == "indexed_only" and source["currency"] == "unverified"
    assert "PRIVATE_" not in json.dumps(source) + caplog.text
    assert runner.call_count == fetch.call_count == 1


def test_blank_excerpt_needs_live_text_to_be_usable():
    lookup, _, _ = service(row(snippet=""))
    assert lookup.query(QUESTION, JURISDICTION)["sources"][0]["page_text"]
    lookup, _, fetch = service(row(snippet=""), fetch=Mock(side_effect=TimeoutError("PRIVATE_BODY")))
    with pytest.raises(index.IndexFailure, match="no usable official sources"):
        lookup.query(QUESTION, JURISDICTION)
    assert fetch.call_count == 1


def test_pdf_and_binary_documents_never_become_live_text_with_html_positive_control():
    fetch = Mock(side_effect=[(200, "\ufeff \n%PDF-1.7\nA decoded binary document."), (200, PAGE)])
    lookup, _, _ = service(row(URL + "?format=pdf"), row(), fetch=fetch)
    sources = lookup.query(QUESTION, JURISDICTION)["sources"]
    assert sources[0]["read_status"] == "indexed_only"
    assert sources[0]["page_text"] == "" and sources[0]["fetched_at"] is None
    assert sources[1]["read_status"] == "live" and "Official page text." in sources[1]["page_text"]
    assert fetch.call_count == 2
    assert all(s["currency"] == "unverified" for s in sources)


@pytest.mark.parametrize("value", [True, "1787585414000", None, float("nan"), float("inf"), -1, 10 ** 400])
def test_invalid_index_timestamp_is_null(value):
    lookup, _, _ = service(row(updated_at=value))
    assert lookup.query(QUESTION, JURISDICTION)["sources"][0]["indexed_at"] is None


def test_millisecond_precision_and_first_ranked_duplicate_metadata_are_preserved():
    lookup, _, fetch = service(row(updated_at=1787585414123, snippet="First ranked excerpt."),
                              row(URL + ";jsessionid=duplicate", updated_at=1787586414999,
                                  snippet="Different duplicate excerpt."))
    result = lookup.query(QUESTION, JURISDICTION)
    assert len(result["sources"]) == fetch.call_count == 1
    assert result["sources"][0]["indexed_at"] == "2026-08-24T15:30:14.123Z"
    assert result["sources"][0]["excerpt"] == "First ranked excerpt."


@pytest.mark.parametrize("credits", [True, -2, "2", float("nan"), float("inf"), 10 ** 400])
def test_unreported_or_invalid_credit_cost_is_metered_unpriced(credits, offline):
    lookup = index.GovernmentIndex(runner=Mock(return_value=reply(row(), credits=credits)),
                                   fetch=Mock(return_value=(200, PAGE)))
    assert lookup.query(QUESTION, JURISDICTION)["ok"]
    assert offline.call_args.args[1] is None


def test_default_source_reader_passes_only_url_timeout_and_official_redirect_guard(monkeypatch):
    fetch = Mock(return_value=(200, PAGE))
    monkeypatch.setattr(watch, "http_request", fetch)
    lookup = index.GovernmentIndex(runner=Mock(return_value=reply(row())))
    assert lookup.query(QUESTION, JURISDICTION)["sources"][0]["read_status"] == "live"
    fetch.assert_called_once_with(URL, timeout=fetch.call_args.kwargs["timeout"], url_guard=index.official_guard)
    assert index.official_guard("https://www.irs.gov/redirected") == ""
    assert index.official_guard("https://evil.example/redirected")
    assert index.official_guard("http://www.irs.gov/redirected")
    assert "headers" not in fetch.call_args.kwargs and "data" not in fetch.call_args.kwargs


def test_default_guarded_reader_cannot_connect_to_private_dns(monkeypatch):
    connections = Mock(side_effect=AssertionError("private connection attempted"))
    monkeypatch.setattr(watch, "_exchange", connections)
    monkeypatch.setattr(watch, "check_url", lambda url: "private address")
    # Exercise the real guarded reader while its DNS rejection is deterministic.
    monkeypatch.setattr(watch, "http_request", _REAL_HTTP_REQUEST)
    lookup = index.GovernmentIndex(runner=Mock(return_value=reply(row())))
    source = lookup.query(QUESTION, JURISDICTION)["sources"][0]
    assert source["read_status"] == "indexed_only" and source["fetched_at"] is None
    connections.assert_not_called()


_REAL_HTTP_REQUEST = watch.http_request


@pytest.mark.parametrize("payload", [None, [], {}, {"data": []}, {"data": {"data": {}}}])
def test_malformed_provider_result_fails_safely_without_source_reads(payload):
    runner, fetch = Mock(return_value=payload), Mock()
    with pytest.raises(index.IndexFailure, match="unreadable source result"):
        index.GovernmentIndex(runner=runner, fetch=fetch).query(QUESTION, JURISDICTION)
    runner.assert_called_once()
    fetch.assert_not_called()


def test_upstream_exception_and_meter_exception_never_reveal_private_bodies_or_retry(offline, caplog):
    runner, fetch = Mock(side_effect=RuntimeError("PRIVATE_KEY PRIVATE_QUESTION")), Mock()
    with pytest.raises(index.IndexFailure) as failure:
        index.GovernmentIndex(runner=runner, fetch=fetch).query(QUESTION, JURISDICTION)
    assert str(failure.value) == "Government index is unavailable. No alternate provider was called."
    assert "PRIVATE_" not in str(failure.value) + caplog.text
    runner.assert_called_once()
    fetch.assert_not_called()
    offline.assert_not_called()
    offline.side_effect = RuntimeError("PRIVATE_KEY")
    runner = Mock(return_value=reply(row()))
    with pytest.raises(index.IndexFailure, match="usage could not be recorded"):
        index.GovernmentIndex(runner=runner, fetch=fetch).query(QUESTION, JURISDICTION)
    runner.assert_called_once()
    fetch.assert_not_called()


@pytest.mark.parametrize("question,jurisdiction", [
    ("", JURISDICTION), (" ", JURISDICTION), (None, JURISDICTION), (QUESTION, None),
    (QUESTION, ""), ("q" * 4001, JURISDICTION), (QUESTION, "j" * 101),
])
def test_invalid_input_never_calls_provider(question, jurisdiction):
    runner, fetch = Mock(), Mock()
    with pytest.raises(index.IndexFailure, match="question and U.S. jurisdiction"):
        index.GovernmentIndex(runner=runner, fetch=fetch).query(question, jurisdiction)
    runner.assert_not_called()
    fetch.assert_not_called()


@pytest.mark.parametrize("timeout", [0, -1, True, None, "55", float("inf"), float("nan"), 10 ** 400])
def test_invalid_timeout_is_rejected(timeout):
    with pytest.raises(ValueError, match="finite and positive"):
        index.GovernmentIndex(timeout_secs=timeout)


def test_read_timeout_uses_remaining_total_budget_and_stops_new_reads(monkeypatch):
    clock = [0.0]
    monkeypatch.setattr(index.time, "monotonic", lambda: clock[0])
    calls = []

    def runner(request, *, timeout):
        assert timeout == 12
        clock[0] = 9
        return reply(row(), row("https://www.irs.gov/second"), row("https://www.irs.gov/third"))

    def fetch(url, *, timeout):
        calls.append((url, timeout))
        clock[0] += timeout
        raise TimeoutError("PRIVATE_QUESTION")

    result = index.GovernmentIndex(runner=runner, fetch=fetch, timeout_secs=12).query(QUESTION, JURISDICTION)
    assert calls == [(URL, 3)]
    assert len(result["sources"]) == 3 and all(s["read_status"] == "indexed_only" for s in result["sources"])
    runner, fetch = Mock(), Mock()
    with pytest.raises(index.IndexFailure, match="no time left to start"):
        index.GovernmentIndex(runner=runner, fetch=fetch, timeout_secs=4).query(QUESTION, JURISDICTION)
    runner.assert_not_called()
    fetch.assert_not_called()


def test_government_lookup_and_existing_search_share_one_persistent_firecrawl_cap(monkeypatch):
    monkeypatch.setenv("FIRECRAWL_API_KEY", "offline-key")
    monkeypatch.setenv("EVE_FIRECRAWL_PER_DAY", "2")
    monkeypatch.setattr(firecrawl, "_CALLS", {})
    monkeypatch.setattr(firecrawl, "_LOADED", set())
    sent = []

    def http(url, *, data, headers, timeout):
        assert headers["Authorization"] == "Bearer offline-key" and timeout > 0
        sent.append((url, json.loads(data)))
        if url == firecrawl.SEARCH_URL:
            return 200, json.dumps({"success": True, "creditsUsed": 1, "data": {"web": []}})
        return 200, json.dumps({"success": True, "data": {"alexandria": [
            {"creditsCost": 2, "data": reply(row())["data"]}]}})

    monkeypatch.setattr(watch, "http_request", http)
    firecrawl.search("Existing shared-brain search")
    fetch = Mock(return_value=(200, PAGE))
    lookup = index.GovernmentIndex(fetch=fetch)
    assert lookup.query(QUESTION, JURISDICTION)["ok"] and firecrawl.left() == 0
    # Simulate a restart: the shared count reloads from the suite's temporary ledger.
    monkeypatch.setattr(firecrawl, "_CALLS", {})
    monkeypatch.setattr(firecrawl, "_LOADED", set())
    with pytest.raises(index.IndexFailure, match="Government index is unavailable"):
        lookup.query(QUESTION, JURISDICTION)
    assert [url for url, _ in sent] == [firecrawl.SEARCH_URL, firecrawl.TOOL_URL]
    assert sent[1][1]["alexandria"][0]["provider"] == "firecrawl-gov-index"
    assert fetch.call_count == 1
