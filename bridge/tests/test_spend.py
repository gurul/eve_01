"""The daily spend meter (spend.py), its prices (pricing.py) and the providers' own figures (spend_sync.py)."""

from __future__ import annotations

import io
import json
import threading
import urllib.error
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import pricing, spend, spend_sync

DAY = date(2026, 9, 24)


def rows(folder: Path, day: date | None = None) -> list[dict[str, Any]]:
    return spend.day_rows((day or date.today()).isoformat(), folder)


def noon(d: date) -> float:
    return datetime(d.year, d.month, d.day, 12).timestamp()


# ---- the ledger --------------------------------------------------------------------------------------------

def test_a_record_is_one_tiny_line_in_the_local_days_file(_spend_ledger_in_tmp: Path) -> None:
    spend.record("openai", "gpt-6-luna", spend.CHAT, 0.00042, tokens={"in": 3100, "cached": 2900, "out": 80,
                                                                      "zero": 0}, now=noon(DAY))
    path = _spend_ledger_in_tmp / "2026-09-24.jsonl"
    line = json.loads(path.read_text())
    assert line == {"t": int(noon(DAY)), "p": "openai", "m": "gpt-6-luna", "f": "chat", "usd": 0.00042,
                    "src": "priced", "tok": {"in": 3100, "cached": 2900, "out": 80}}


def test_an_unknown_price_is_recorded_as_none_and_flagged_never_guessed(_spend_ledger_in_tmp: Path,
                                                                          caplog) -> None:
    for _ in range(2):
        spend.record("openai", "gpt-9-unknown", spend.CHAT, None, now=noon(DAY))
    got = rows(_spend_ledger_in_tmp, DAY)
    assert [r["usd"] for r in got] == [None, None] and all(r["unpriced"] for r in got)
    assert sum("gpt-9-unknown" in r.message for r in caplog.records) == 1      # logged once per model


def test_record_never_raises_when_the_folder_cannot_be_written(tmp_path: Path) -> None:
    blocker = tmp_path / "file"
    blocker.write_text("x")
    spend.set_dir(blocker / "spend")                       # a folder under a file: mkdir fails
    spend.record("openai", "gpt-6-luna", spend.CHAT, 0.1)    # no exception


def test_concurrent_records_are_all_kept_whole(_spend_ledger_in_tmp: Path) -> None:
    def many() -> None:
        for _ in range(200):
            spend.record("openai", "gpt-5.4-nano", spend.CAMERA, 0.0001, tokens={"in": 100})

    threads = [threading.Thread(target=many) for _ in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    got = rows(_spend_ledger_in_tmp)
    assert len(got) == 1600 and spend.totals(got)["usd"] == pytest.approx(0.16)


def test_a_torn_line_is_skipped(_spend_ledger_in_tmp: Path) -> None:
    spend.record("openai", "gpt-6-luna", spend.CHAT, 0.5, now=noon(DAY))
    with open(_spend_ledger_in_tmp / "2026-09-24.jsonl", "a") as f:
        f.write('{"t": 1, "p": "open')
    assert len(rows(_spend_ledger_in_tmp, DAY)) == 1


def test_the_ledger_line_carries_no_words(_spend_ledger_in_tmp: Path) -> None:
    secret = "please remember my bank pin"
    spend.record_response(spend.CHAT, {"model": "gpt-6-luna", "usage": {"input_tokens": 10, "output_tokens": 5},
                                       "output": [{"type": "message", "content": [{"type": "output_text",
                                                                                   "text": secret}]}]})
    assert secret not in (_spend_ledger_in_tmp / f"{date.today().isoformat()}.jsonl").read_text()


# ---- aggregation -------------------------------------------------------------------------------------------

def test_totals_split_by_provider_feature_and_model_largest_first() -> None:
    lines = [{"p": "openai", "m": "gpt-6-luna", "f": "chat", "usd": 0.2},
             {"p": "openai", "m": "gpt-live-1", "f": "voice", "usd": 0.5},
             {"p": "openrouter", "m": "google/gemini-3.1-flash-lite", "f": "search", "usd": 0.01},
             {"p": "chatgpt", "m": "codex", "f": "codex", "usd": None, "unpriced": True}]
    t = spend.totals(lines)
    assert t["usd"] == pytest.approx(0.71) and t["calls"] == 4 and t["unpriced"] == 1
    assert list(t["by_provider"]) == ["openai", "openrouter", "chatgpt"]
    assert list(t["by_feature"]) == ["voice", "chat", "search", "codex"]
    assert t["by_model"]["gpt-live-1"] == pytest.approx(0.5) and t["unpriced_models"] == ["chatgpt/codex"]


def test_summary_has_today_yesterday_thirty_days_and_the_month(_spend_ledger_in_tmp: Path) -> None:
    spend.record("openai", "gpt-6-luna", spend.CHAT, 1.0, now=noon(DAY))
    spend.record("openai", "gpt-live-1", spend.VOICE, 0.5, now=noon(DAY))
    spend.record("openai", "gpt-6-luna", spend.CHAT, 2.0, now=noon(DAY - timedelta(days=1)))
    spend.record("openai", "gpt-6-luna", spend.CHAT, 4.0, now=noon(date(2026, 9, 1)))
    spend.record("openai", "gpt-6-luna", spend.CHAT, 8.0, now=noon(date(2026, 8, 31)))   # last month
    s = spend.summary(DAY)
    assert s["today"]["usd"] == pytest.approx(1.5) and s["today"]["by_feature"] == {"chat": 1.0, "voice": 0.5}
    assert s["yesterday"]["usd"] == pytest.approx(2.0) and s["yesterday"]["date"] == "2026-09-23"
    assert s["month"] == {"label": "2026-09", "usd": pytest.approx(7.5)}
    assert len(s["days"]) == 30 and s["days"][-1] == {"date": "2026-09-24", "usd": 1.5}
    assert s["days"][0]["date"] == "2026-08-26" and s["days"][5] == {"date": "2026-08-31", "usd": 8.0}
    assert sum(d["usd"] == 0 for d in s["days"]) == 26                      # empty days are zeros, not gaps


def test_brief_is_written_by_code_with_the_top_three_features_and_openrouters_figure() -> None:
    s = {"today": {**spend.totals([{"p": "openai", "f": f, "m": "m", "usd": u}
                                   for f, u in (("chat", 0.3), ("voice", 1.2), ("search", 0.05), ("camera", 0.01))]),
                   "date": "2026-09-24"},
         "yesterday": {"usd": 2.5}, "month": {"usd": 40.0}}
    text = spend.brief(s, {"openrouter": {"status": "ok", "key_today": 0.12, "account_today": 0.4,
                                          "account_partial": False}})
    assert text.splitlines() == ["Spending (eve's own meter)", "Today: $1.56", "Yesterday: $2.50",
                                 "This month: $40.00", "Top today: voice $1.20, chat $0.30, search $0.050",
                                 "OpenRouter says today (UTC): $0.12 on eve's key, $0.40 on the whole account."]


# ---- the prices --------------------------------------------------------------------------------------------

M = 1_000_000


@pytest.mark.parametrize("model, inp, cached, out", [
    ("gpt-6-astra", 10.0, 1.0, 50.0), ("gpt-6-sol", 2.0, 0.20, 10.0), ("gpt-6-luna", 0.10, 0.01, 0.50),
    ("gpt-5-mini", 0.25, 0.025, 2.00), ("gpt-5.4-nano", 0.20, 0.02, 1.25),
])
def test_every_responses_model_eve_uses_has_its_grounded_rate(model: str, inp: float, cached: float,
                                                                 out: float) -> None:
    usage = {"input_tokens": 2 * M, "input_tokens_details": {"cached_tokens": M}, "output_tokens": M}
    assert pricing.estimate_openai_cost(model, usage) == pytest.approx(inp + cached + out)


def test_live_minutes_transcription_embeddings_and_opus() -> None:
    assert pricing.estimate_live_cost("gpt-live-1", 90) == pytest.approx(0.075)          # $0.05 a minute, per second
    assert pricing.estimate_transcribe_cost("gpt-4o-mini-transcribe", {"input_tokens": M, "output_tokens": M}) \
        == pytest.approx(1.25 + 5.00)
    assert pricing.estimate_transcribe_cost("gpt-4o-mini-transcribe", {}, seconds=120) == pytest.approx(0.006)
    assert pricing.estimate_transcribe_cost("gpt-4o-mini-transcribe", {"type": "duration", "seconds": 60}) \
        == pytest.approx(0.003)
    assert pricing.estimate_embedding_cost("text-embedding-3-small", M) == pytest.approx(0.02)
    opus = {"input_tokens": M, "output_tokens": M, "cache_read_input_tokens": M, "cache_creation_input_tokens": 2 * M,
            "cache_creation": {"ephemeral_5m_input_tokens": M, "ephemeral_1h_input_tokens": M}}
    assert pricing.estimate_anthropic_cost("claude-opus-5-5", opus) == pytest.approx(4 + 20 + 0.20 + 5 + 8)


def test_a_model_without_a_rate_prices_to_none_but_a_pinned_suffix_is_the_same_model() -> None:
    for fn, args in ((pricing.estimate_live_cost, ("gpt-live-2", 60)),
                     (pricing.estimate_embedding_cost, ("text-embedding-3-large", 10)),
                     (pricing.estimate_anthropic_cost, ("claude-sonnet-5", {"input_tokens": 1})),
                     (pricing.estimate_transcribe_cost, ("whisper-1", {"input_tokens": 1}))):
        assert fn(*args) is None
    assert pricing.estimate_anthropic_cost("claude-opus-5-5-20260801", {"output_tokens": M}) == pytest.approx(20)
    assert pricing.estimate_openai_cost("openai/gpt-6-astra-2026-08-01", {"output_tokens": M}) == pytest.approx(50)


# ---- the helpers ----------------------------------------------------------------------------------------------

def test_a_responses_reply_is_priced_with_its_hosted_searches(_spend_ledger_in_tmp: Path) -> None:
    spend.record_response(spend.THINKING, {"model": "gpt-6-sol-2026-09-01",
                                           "usage": {"input_tokens": M, "output_tokens": 0},
                                           "output": [{"type": "web_search_call"}, {"type": "web_search_call"}]})
    [row] = rows(_spend_ledger_in_tmp)
    assert row["usd"] == pytest.approx(2.0 + 0.02) and row["tok"] == {"in": M, "searches": 2}
    assert row["src"] == "priced" and row["f"] == "thinking"


def test_a_providers_own_cost_wins_and_a_reply_without_usage_is_unpriced(_spend_ledger_in_tmp: Path) -> None:
    spend.record_chat_completion(spend.SEARCH, {"usage": {"prompt_tokens": 900, "completion_tokens": 60,
                                                          "cost": 0.0061}}, model="google/gemini-3.1-flash-lite")
    spend.record_chat_completion(spend.LESSONS, {"usage": {"prompt_tokens": M, "completion_tokens": 0}},
                                 model="openai/gpt-6-astra")
    spend.record_response(spend.CHAT, {"model": "gpt-6-luna"})
    a, b, c = rows(_spend_ledger_in_tmp)
    assert (a["usd"], a["src"], a["p"]) == (0.0061, "reported", "openrouter")
    assert (b["usd"], b["src"]) == (pytest.approx(10.0), "priced")
    assert c["usd"] is None and c["unpriced"] and c["note"] == "no usage in the reply"


def test_an_sdk_object_is_read_like_a_dict(_spend_ledger_in_tmp: Path) -> None:
    class Reply:
        def model_dump(self, exclude_none: bool = False) -> dict[str, Any]:
            return {"model": "gpt-5.4-nano", "usage": {"input_tokens": M, "output_tokens": 0}}

    spend.record_response(spend.CAMERA, Reply())
    assert rows(_spend_ledger_in_tmp)[0]["usd"] == pytest.approx(0.20)


# ---- the providers' own figures (spend_sync.py) ----------------------------------------------------------

class FakeResponse(io.BytesIO):
    def __enter__(self) -> "FakeResponse":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()


class Opener:
    """Answers by URL prefix; records each request's URL and headers."""

    def __init__(self, routes: dict[str, Any]) -> None:
        self.routes, self.seen = routes, []

    def __call__(self, req: Any, timeout: float = 0) -> FakeResponse:
        self.seen.append((req.full_url, dict(req.header_items())))
        for prefix, answer in self.routes.items():
            if req.full_url.startswith(prefix):
                if isinstance(answer, list):
                    answer = answer.pop(0)
                if isinstance(answer, Exception):
                    raise answer
                return FakeResponse(json.dumps(answer).encode())
        raise AssertionError(f"unexpected URL {req.full_url}")


NOW = datetime(2026, 9, 24, 18, 0, tzinfo=timezone.utc).timestamp()


def openrouter(total: float, daily: float = 0.12) -> dict[str, Any]:
    return {spend_sync.OPENROUTER_KEY_URL: {"data": {"usage_daily": daily, "usage_monthly": 3.4, "usage": 9.9}},
            spend_sync.OPENROUTER_CREDITS_URL: {"data": {"total_credits": 50, "total_usage": total}}}


def test_openrouter_key_daily_and_account_delta_between_days() -> None:
    env = {"OPENROUTER_API_KEY": "or-test"}
    spend_sync.sync_once(env, now=NOW - 86400, opener=Opener(openrouter(10.0)))
    spend_sync.sync_once(env, now=NOW - 3600, opener=Opener(openrouter(10.5)))
    opener = Opener(openrouter(11.25))
    state = spend_sync.sync_once(env, now=NOW, opener=opener)
    assert opener.seen[0][1]["Authorization"] == "Bearer or-test"
    got = spend_sync.reported(state, now=NOW, env=env)["openrouter"]
    assert got["status"] == "ok" and got["key_today"] == 0.12 and got["key_month"] == 3.4
    assert got["account_today"] == pytest.approx(1.25) and got["account_partial"] is False


def test_a_first_day_counts_from_its_own_first_snapshot_and_says_so() -> None:
    env = {"OPENROUTER_API_KEY": "or-test"}
    spend_sync.sync_once(env, now=NOW - 7200, opener=Opener(openrouter(10.0)))
    state = spend_sync.sync_once(env, now=NOW, opener=Opener(openrouter(10.4)))
    got = spend_sync.reported(state, now=NOW, env=env)["openrouter"]
    assert got["account_today"] == pytest.approx(0.4) and got["account_partial"] is True


def test_openai_costs_are_read_only_with_an_admin_key_and_paged() -> None:
    day = int(datetime(2026, 9, 24, tzinfo=timezone.utc).timestamp())
    pages = [{"data": [{"start_time": day - 86400, "results": [{"amount": {"value": 1.5, "currency": "usd"}}]}],
              "has_more": True, "next_page": "p2"},
             {"data": [{"start_time": day, "results": [{"amount": {"value": 0.25, "currency": "usd"}},
                                                       {"amount": {"value": 0.5, "currency": "usd"}}]}],
              "has_more": False, "next_page": None}]
    opener = Opener({spend_sync.OPENAI_COSTS_URL: pages})
    assert spend_sync.sync_once({}, now=NOW, opener=Opener({})) == {}               # no keys: no calls at all
    state = spend_sync.sync_once({"EVE_OPENAI_ADMIN_KEY": "admin-test"}, now=NOW, opener=opener)
    first, second = opener.seen
    assert "bucket_width=1d" in first[0] and "start_time=" in first[0] and "page=p2" in second[0]
    assert first[1]["Authorization"] == "Bearer admin-test"
    got = spend_sync.reported(state, now=NOW, env={"EVE_OPENAI_ADMIN_KEY": "admin-test"})["openai"]
    assert got["today"] == pytest.approx(0.75) and got["month"] == pytest.approx(2.25) and got["status"] == "ok"


def test_anthropic_amounts_are_cents_and_its_headers_are_the_admin_ones() -> None:
    report = {"data": [{"starting_at": "2026-09-24T00:00:00Z", "ending_at": "2026-09-25T00:00:00Z",
                        "results": [{"amount": "123.45", "currency": "USD"}, {"amount": "10", "currency": "USD"}]},
                       {"starting_at": "2026-09-23T00:00:00Z", "results": []}],
              "has_more": False, "next_page": None}
    opener = Opener({spend_sync.ANTHROPIC_COST_URL: report})
    env = {"EVE_ANTHROPIC_ADMIN_KEY": "sk-ant-admin-test"}
    state = spend_sync.sync_once(env, now=NOW, opener=opener)
    url, headers = opener.seen[0]
    assert "starting_at=2026-08-25T00%3A00%3A00Z" in url and "bucket_width=1d" in url
    assert headers["X-api-key"] == "sk-ant-admin-test" and headers["Anthropic-version"] == "2023-06-01"
    got = spend_sync.reported(state, now=NOW, env=env)["anthropic"]
    assert got["today"] == pytest.approx(1.3345) and got["status"] == "ok"


def test_a_failure_is_logged_once_kept_as_a_reason_and_breaks_nothing(caplog) -> None:
    refused = urllib.error.HTTPError("u", 403, "Forbidden", {}, None)   # type: ignore[arg-type]
    env = {"OPENROUTER_API_KEY": "or-test", "EVE_OPENAI_ADMIN_KEY": "admin-test"}
    routes = {**openrouter(10.0), spend_sync.OPENAI_COSTS_URL: [refused, refused]}
    opener = Opener(routes)
    spend_sync.sync_once(env, now=NOW, opener=opener)
    routes.update(openrouter(10.0))
    state = spend_sync.sync_once(env, now=NOW + 60, opener=opener)
    got = spend_sync.reported(state, now=NOW, env=env)
    assert got["openai"]["status"] == "error" and got["openai"]["error"] == "HTTP 403"
    assert got["openrouter"]["status"] == "ok"                                   # the other provider still read
    assert sum("openai" in r.message and "HTTP 403" in r.message for r in caplog.records) == 1
    assert all("admin-test" not in r.message for r in caplog.records)


def test_no_admin_key_says_which_one_to_add() -> None:
    got = spend_sync.reported({}, now=NOW, env={})
    assert got["openai"] == {"status": "no_key", "key_name": "EVE_OPENAI_ADMIN_KEY"}
    assert got["anthropic"] == {"status": "no_key", "key_name": "EVE_ANTHROPIC_ADMIN_KEY"}
    assert got["openrouter"] == {"status": "no_key"}


def test_the_loop_syncs_at_start_and_stops_on_shutdown(monkeypatch) -> None:
    import asyncio

    calls: list[Any] = []
    monkeypatch.setattr(spend_sync, "sync_once", lambda env=None: calls.append(env))

    async def go() -> None:
        stop = asyncio.Event()
        task = asyncio.create_task(spend_sync.loop(stop, interval_secs=3600, env={}))
        for _ in range(100):
            if calls:
                break
            await asyncio.sleep(0.01)
        stop.set()
        await asyncio.wait_for(task, 2)

    asyncio.run(go())
    assert calls == [{}]


# ---- the chief's card tag (spend.job; chief design G3.3, 2026-09-29) ---------------------------------------

def test_rows_written_inside_a_job_carry_its_card_id_and_rows_outside_do_not(_spend_ledger_in_tmp: Path) -> None:
    spend.record("openai", "gpt-6-luna", spend.CHAT, 0.001, now=noon(DAY))
    with spend.job("c7"):
        assert spend.current_job() == "c7"
        spend.record("openrouter", "m", spend.CHIEF, 0.002, now=noon(DAY))
        with spend.job("c8"):
            spend.record("openrouter", "m", spend.CHIEF, 0.003, now=noon(DAY))
        spend.record("openrouter", "m", spend.CHIEF, 0.004, now=noon(DAY))
    spend.record("openai", "gpt-6-luna", spend.CHAT, 0.005, now=noon(DAY))
    assert spend.current_job() == ""
    got = [(r.get("job"), r["usd"]) for r in rows(_spend_ledger_in_tmp, DAY)]
    assert got == [(None, 0.001), ("c7", 0.002), ("c8", 0.003), ("c7", 0.004), (None, 0.005)]
    assert "job" not in json.loads((_spend_ledger_in_tmp / "2026-09-24.jsonl").read_text().splitlines()[0])


def test_a_job_tag_survives_asyncio_to_thread_and_is_gone_after(_spend_ledger_in_tmp: Path) -> None:
    import asyncio

    async def go() -> None:
        with spend.job("c7"):
            await asyncio.to_thread(spend.record, "openrouter", "m", spend.CHIEF, 0.01, now=noon(DAY))
        await asyncio.to_thread(spend.record, "openrouter", "m", spend.CHIEF, 0.02, now=noon(DAY))

    asyncio.run(go())
    assert [r.get("job") for r in rows(_spend_ledger_in_tmp, DAY)] == ["c7", None]


def test_a_job_tag_survives_the_search_routers_worker_thread(_spend_ledger_in_tmp: Path) -> None:
    from eve_bridge import search_router

    def work(abandoned: Any) -> dict[str, Any]:
        spend.record("openrouter", "m", spend.CHIEF, 0.01, now=noon(DAY))
        return {"ok": True}

    with spend.job("c9"):
        assert search_router.bounded(work, 5.0) == {"ok": True}
    # positive control: a bare thread started inside the job loses the tag, which is why bounded copies it
    with spend.job("c9"):
        t = threading.Thread(target=spend.record, args=("openrouter", "m", spend.CHIEF, 0.02), kwargs={"now": noon(DAY)})
        t.start()
        t.join()
    assert [r.get("job") for r in rows(_spend_ledger_in_tmp, DAY)] == ["c9", None]


def test_a_bad_card_id_tags_nothing_and_job_total_sums_one_cards_rows(_spend_ledger_in_tmp: Path) -> None:
    with spend.job("c7; rm -rf ~"):
        spend.record("openrouter", "m", spend.CHIEF, 0.5, now=noon(DAY))
    with spend.job("c7"):
        spend.record("openrouter", "m", spend.CHIEF, 0.25, now=noon(DAY))
        spend.record("openrouter", "m", spend.CHIEF, None, now=noon(DAY))           # unpriced: counts nothing
        spend.record("openrouter", "m", spend.CHIEF, 0.125, now=noon(DAY + timedelta(days=1)))
    with spend.job("c70"):
        spend.record("openrouter", "m", spend.CHIEF, 1.0, now=noon(DAY))
    assert "job" not in rows(_spend_ledger_in_tmp, DAY)[0]
    assert spend.job_total("c7", DAY, DAY + timedelta(days=1), _spend_ledger_in_tmp) == 0.375
    assert spend.job_total("c7", DAY + timedelta(days=1), DAY + timedelta(days=1), _spend_ledger_in_tmp) == 0.125
    assert spend.totals(rows(_spend_ledger_in_tmp, DAY))["by_feature"] == {"chief of staff": 1.75}
