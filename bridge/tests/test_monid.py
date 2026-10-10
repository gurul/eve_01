"""monid.py: the Monid CLI as the web search's last resort, with a fake runner (no network, no real CLI)."""

from __future__ import annotations

import asyncio
import io
import json
import logging
import urllib.error

import pytest

from eve_bridge import monid, spend
from eve_bridge import websearch as ws

QUERY = "who is the current president of the united states"

# The shape `monid run -p dataforseo -e /serp/google-organic -j` printed on 2026-10-08, trimmed.
RUN_OUTPUT = {
    "runId": "01TESTRUN", "provider": "dataforseo", "endpoint": "/serp/google-organic", "status": "COMPLETED",
    "output": [{
        "keyword": QUERY, "type": "organic", "items_count": 5,
        "items": [
            {"type": "ai_overview", "markdown": "The current president is Example Person, the 47th president."
                                                "[[1]](https://example.org/a)\n- Name: Example"},
            {"type": "organic", "title": "President", "url": "https://example.org/a",
             "description": "Example Person is the 47th and current president."},
            {"type": "people_also_ask", "items": []},
            {"type": "organic", "title": "The office", "url": "https://example.gov/b", "description": "About it."},
            {"type": "organic", "title": "dup", "url": "https://example.org/a", "description": "dup"},
        ]}],
    "billing": {"reportedCost": {"currency": "USD", "value": 2000, "unit": "MICRO_DOLLAR"}},
}
KEYS = [{"label": "main", "prefix": "<prefix>", "key": "<masked>", "added_at": "2026-10-08", "active": True}]


class FakeCli:
    """Answers `keys list`, `run` and `balance` like the 0.1.7 CLI; records each argv."""

    def __init__(self, run_reply: object = RUN_OUTPUT, keys: object = KEYS, rc: int = 0) -> None:
        self.run_reply, self.keys, self.rc = run_reply, keys, rc
        self.calls: list[list[str]] = []
        self.envs: list[dict[str, str]] = []
        self.timeouts: list[float] = []

    def runs(self) -> list[list[str]]:
        return [a for a in self.calls if a[1] == "run"]

    def __call__(self, argv: list[str], timeout: float, env: dict[str, str]) -> tuple[int, str, str]:
        self.calls.append(argv)
        self.envs.append(env)
        self.timeouts.append(timeout)
        if argv[1] == "keys":
            return 0, json.dumps(self.keys), ""
        if argv[1] == "balance":
            return 0, json.dumps({"balance": {"value": 0.998, "currency": "USD"}, "held": {"value": 0}}), ""
        reply = self.run_reply
        return self.rc, reply if isinstance(reply, str) else json.dumps(reply), ""


@pytest.fixture
def cli(monkeypatch: pytest.MonkeyPatch) -> FakeCli:
    fake = FakeCli()
    monkeypatch.setenv("EVE_MONID", "fallback")
    monkeypatch.setenv("EVE_MONID_CLI", "/fake/bin/monid")
    monkeypatch.delenv("EVE_MONID_PER_DAY", raising=False)
    monkeypatch.delenv("EVE_MONID_USD_PER_DAY", raising=False)
    monkeypatch.setattr(monid, "RUNNER", fake)
    return fake


def _both_fail(code: int | None = None, second: int | None = None):
    """An opener where both engines fail: unreachable, or HTTP ``code`` (the second engine ``second``)."""
    def opener(req, timeout):
        engine = json.loads(req.data)["tools"][0]["parameters"]["engine"]
        c = code if engine == "perplexity" or second is None else second
        if c is None:
            raise urllib.error.URLError(OSError("no route"))
        raise urllib.error.HTTPError(req.full_url, c, "x", {}, io.BytesIO(b""))
    return opener


CFG = ws.SearchConfig(engine="openrouter-perplexity", timeout_secs=25.0)
SHAPE = {"ok", "answer", "sources", "usage", "ms", "cost_usd"}


def test_the_fallback_fires_only_when_both_engines_failed(cli: FakeCli) -> None:
    out = ws.search(QUERY, CFG, key="r", opener=_both_fail())
    assert out["ok"] and out["fallback"] == "monid" and set(out) == SHAPE | {"fallback"}
    assert out["sources"] == [
        {"title": "President", "url": "https://example.org/a",
         "snippet": "Example Person is the 47th and current president."},
        {"title": "The office", "url": "https://example.gov/b", "snippet": "About it."}]
    assert out["answer"].startswith("From Google results: The current president is Example Person, the 47th "
                                    "president. (Google AI overview) Example Person is the 47th")
    assert "[[1]]" not in out["answer"] and "Name:" not in out["answer"]
    assert out["cost_usd"] == 0.002 and out["usage"] == {"in": 0, "out": 0, "cost": 0.002}
    assert isinstance(out["ms"], int)
    (run,) = cli.runs()
    assert run[:5] == ["/fake/bin/monid", "run", "-p", "dataforseo", "-e"] and run[-1] == "-j"
    body = json.loads(run[run.index("-i") + 1])
    assert body == {"keyword": QUERY, "location_code": 2840, "language_code": "en", "depth": 10}
    assert int(run[run.index("-w") + 1]) <= 12 and cli.timeouts[-1] <= 12.0 + 0.01
    assert cli.envs[-1]["NO_COLOR"] == "1" and "/opt/homebrew/bin" in cli.envs[-1]["PATH"].split(":")


def test_a_rate_limit_on_both_engines_falls_back_too(cli: FakeCli) -> None:
    out = ws.search(QUERY, CFG, key="r", opener=_both_fail(429))
    assert out["ok"] and out["fallback"] == "monid" and len(cli.runs()) == 1


def test_no_fallback_on_a_success(cli: FakeCli) -> None:
    from test_websearch import PAYLOAD, Reply

    out = ws.search(QUERY, CFG, key="r", opener=lambda req, timeout: Reply(PAYLOAD))
    assert out["ok"] and "fallback" not in out and cli.calls == []

    def first_fails(req, timeout):
        if json.loads(req.data)["tools"][0]["parameters"]["engine"] == "perplexity":
            raise urllib.error.URLError(OSError("no route"))
        return Reply(PAYLOAD)

    out = ws.search(QUERY, CFG, key="r", opener=first_fails)
    assert out["ok"] and out["fallback"] == "openrouter-exa" and cli.calls == []


def test_no_fallback_after_a_4xx_other_than_429(cli: FakeCli) -> None:
    assert ws.search(QUERY, CFG, key="r", opener=_both_fail(400)) == {
        "ok": False, "reason": "the search service answered HTTP 400"}                 # not even the other engine
    out = ws.search(QUERY, CFG, key="r", opener=_both_fail(503, second=404))
    assert out == {"ok": False, "reason": "the search service answered HTTP 503"}
    assert cli.calls == []


def test_openrouter_refusing_the_account_still_gets_monid(cli: FakeCli) -> None:
    """401/402/403 (credits gone, key revoked) are when OpenRouter stops for everything: Monid can still answer."""
    out = ws.search(QUERY, CFG, key="r", opener=_both_fail(402, second=401))
    assert out["ok"] and out["fallback"] == "monid"


def test_a_unitless_cost_is_unpriced_not_dollars() -> None:
    assert monid._cost({"billing": {"reportedCost": {"value": 2000}}}) is None
    assert monid._cost({"billing": {"reportedCost": {"value": 2000, "unit": "MICRO_DOLLAR"}}}) == 0.002


def test_odd_settings_never_raise(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVE_MONID_PER_DAY", "inf")
    monkeypatch.setenv("EVE_MONID_USD_PER_DAY", "1e999")
    assert monid.per_day() == monid.DEFAULT_PER_DAY
    assert 0.0 <= monid.usd_per_day() <= 100.0


def test_the_cli_never_gets_eves_secrets(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("OPENROUTER_API_KEY", "sk-or-test")
    monkeypatch.setenv("EVE_TELEGRAM_TOKEN", "123:abc")
    env = monid._env("/opt/homebrew/bin/monid")
    assert "OPENROUTER_API_KEY" not in env and "EVE_TELEGRAM_TOKEN" not in env
    assert env["NO_COLOR"] == "1" and "/opt/homebrew/bin" in env["PATH"].split(":")


def test_the_reader_never_pays_for_monid(cli: FakeCli) -> None:
    out = ws.engine_search(QUERY, CFG, key="r", opener=_both_fail(), monid_ok=False)
    assert out["ok"] is False and cli.calls == []


def test_switched_off_never_runs(cli: FakeCli, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVE_MONID", "0")
    assert ws.search(QUERY, CFG, key="r", opener=_both_fail()) == {
        "ok": False, "reason": "the search service is unreachable"}
    assert cli.calls == []


def test_the_daily_run_cap(cli: FakeCli, monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture) -> None:
    monkeypatch.setenv("EVE_MONID_PER_DAY", "2")
    assert ws.search(QUERY, CFG, key="r", opener=_both_fail())["ok"]
    assert ws.search(QUERY, CFG, key="r", opener=_both_fail())["ok"]
    with caplog.at_level(logging.INFO, logger="eve_bridge.monid"):
        out = ws.search(QUERY, CFG, key="r", opener=_both_fail())
    assert out == {"ok": False, "reason": "the search service is unreachable"} and len(cli.runs()) == 2
    capped = [r for r in caplog.records if "used up" in r.getMessage()]
    assert len(capped) == 1 and "EVE_MONID_PER_DAY" in capped[0].getMessage()
    # a restart reads the day's count back: no fresh cap
    with monid._LOCK:
        monid._USED.clear()
        monid._LOADED.clear()
    assert monid.used_today()["runs"] == 2
    assert not ws.search(QUERY, CFG, key="r", opener=_both_fail())["ok"] and len(cli.runs()) == 2


def test_the_daily_dollar_cap(cli: FakeCli, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVE_MONID_USD_PER_DAY", "0.003")
    assert ws.search(QUERY, CFG, key="r", opener=_both_fail())["ok"]
    assert monid.used_today() == {"runs": 1, "usd": pytest.approx(0.002)}
    assert not ws.search(QUERY, CFG, key="r", opener=_both_fail())["ok"]                # 0.002 + 0.002 > 0.003
    assert len(cli.runs()) == 1
    monkeypatch.setenv("EVE_MONID_USD_PER_DAY", "0")
    assert monid.run("p", "/e", {}, 5, estimate_usd=0.0)["capped"]                      # $0 a day stops them all


def test_a_reported_cost_replaces_the_estimate(cli: FakeCli) -> None:
    cli.run_reply = {**RUN_OUTPUT, "billing": {"reportedCost": {"value": 1000, "unit": "MICRO_DOLLAR"}}}
    assert monid.web_search(QUERY, 12.0)["cost_usd"] == 0.001
    assert monid.used_today()["usd"] == pytest.approx(0.001)


@pytest.mark.parametrize("case", ["missing", "no key", "timeout", "bad json", "failed run", "no results",
                                  "no time"])
def test_every_monid_failure_returns_the_original_reason(cli: FakeCli, monkeypatch: pytest.MonkeyPatch,
                                                          case: str) -> None:
    budget = 25.0
    if case == "missing":
        monkeypatch.setenv("EVE_MONID_CLI", "")
        monkeypatch.setattr(monid.shutil, "which", lambda name: None)
        monkeypatch.setattr(monid, "CLI_PATHS", ())
        monkeypatch.setattr(monid, "NPM_BIN_DIRS", ())
        monkeypatch.delenv("NPM_CONFIG_PREFIX", raising=False)
    elif case == "no key":
        cli.keys = [{"label": "main", "active": False}]
    elif case == "timeout":
        def hangs(argv, timeout, env):
            cli.calls.append(argv)
            return (monid.RC_TIMEOUT, "", "") if argv[1] == "run" else (0, json.dumps(KEYS), "")
        monkeypatch.setattr(monid, "RUNNER", hangs)
    elif case == "bad json":
        cli.run_reply = "Error: something {not json"
    elif case == "failed run":
        cli.run_reply = {**RUN_OUTPUT, "status": "BLOCKED", "output": None}
    elif case == "no results":
        cli.run_reply = {**RUN_OUTPUT, "output": [{"items": [{"type": "related_searches"}]}]}
    elif case == "no time":
        budget = 2.0
    if case == "no time":
        out = monid.web_search(QUERY, budget)
        assert not out["ok"] and cli.runs() == []
        return
    out = ws.search(QUERY, CFG, key="r", opener=_both_fail())
    assert out == {"ok": False, "reason": "the search service is unreachable"}
    if case in ("missing", "no key"):
        assert cli.runs() == []


def test_a_run_is_recorded_in_the_spend_ledger(cli: FakeCli, _spend_ledger_in_tmp) -> None:
    assert ws.search(QUERY, CFG, key="r", opener=_both_fail())["ok"]
    rows = [r for f in _spend_ledger_in_tmp.glob("*.jsonl") for r in map(json.loads, f.read_text().splitlines())]
    (row,) = [r for r in rows if r["p"] == "monid"]
    assert row["usd"] == 0.002 and row["src"] == "reported" and row["f"] == spend.SEARCH
    assert row["m"] == "dataforseo/serp/google-organic"


def test_an_unpriced_run_is_flagged_and_counted_at_its_list_price(cli: FakeCli, _spend_ledger_in_tmp) -> None:
    cli.run_reply = {k: v for k, v in RUN_OUTPUT.items() if k != "billing"}
    assert monid.web_search(QUERY, 12.0)["cost_usd"] is None
    rows = [json.loads(line) for f in _spend_ledger_in_tmp.glob("*.jsonl") for line in f.read_text().splitlines()]
    (row,) = [r for r in rows if r["p"] == "monid"]
    assert row["usd"] is None and row["unpriced"]
    assert monid.used_today()["usd"] == pytest.approx(monid.SEARCH_USD)


def test_logs_carry_the_shape_never_the_query_or_the_key(cli: FakeCli, caplog: pytest.LogCaptureFixture) -> None:
    cli.keys = [{"label": "main", "key": "SECRET-SHAPED-VALUE", "active": True}]
    with caplog.at_level(logging.DEBUG):
        assert ws.search(QUERY, CFG, key="r", opener=_both_fail())["ok"]
    text = "\n".join(r.getMessage() for r in caplog.records)
    assert "monid: search ok, 2 results in" in text and "$0.0020" in text
    assert QUERY not in text and "president" not in text and "SECRET-SHAPED-VALUE" not in text


def test_available_needs_the_cli_and_an_active_key(cli: FakeCli) -> None:
    assert monid.available()
    monid.reset_for_tests()
    cli.keys = []
    assert not monid.available()
    cli.keys = {"items": [{"active": True}]}
    assert monid.available()


def test_balance(cli: FakeCli) -> None:
    assert monid.balance() == {"ok": True, "usd": 0.998, "held_usd": 0.0}


def test_the_async_wrappers_run_off_the_loop(cli: FakeCli) -> None:
    async def go() -> tuple[dict, dict]:
        out = await monid.arun(monid.SEARCH_PROVIDER, monid.SEARCH_ENDPOINT, monid.search_body(QUERY), 10,
                               estimate_usd=monid.SEARCH_USD)
        return out, await monid.abalance()

    out, bal = asyncio.run(go())
    assert out["ok"] and out["cost_usd"] == 0.002 and out["status"] == "COMPLETED" and bal["ok"]


def test_the_real_runner_kills_on_timeout_and_reports_a_missing_binary(tmp_path) -> None:
    rc, _, _ = monid.subprocess_runner(["/bin/sleep", "5"], 0.2, {"PATH": "/bin"})
    assert rc == monid.RC_TIMEOUT
    rc, _, err = monid.subprocess_runner([str(tmp_path / "nope")], 1.0, {"PATH": "/bin"})
    assert rc == monid.RC_NO_CLI and "could not start" in err


def test_the_cli_is_found_where_launchd_cannot_see_it(monkeypatch: pytest.MonkeyPatch, tmp_path) -> None:
    fake = tmp_path / "monid"
    fake.write_text("#!/bin/sh\n")
    fake.chmod(0o755)
    monkeypatch.setattr(monid.shutil, "which", lambda name: None)
    monkeypatch.setattr(monid, "CLI_PATHS", (str(tmp_path / "absent"), str(fake)))
    assert monid.find_cli({}) == str(fake)
    assert monid.find_cli({"EVE_MONID_CLI": "/x/monid"}) == "/x/monid"
