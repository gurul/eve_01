"""self_context.py: eve's live wiring as one short block, and the three doors that carry it.

The owner asked "what do you use for search" on Telegram (2026-09-29) and eve answered from a stale memory
record. These pin that the block follows the configuration (never a typed value), stays inside its budget, rides
the instructions of the Telegram brain, the voice's two halves and think_hard, and names no retired engine. No
network: every environment is a dict, the lights file a temporary one, the commit lent.
"""

from __future__ import annotations

import asyncio
import json
import logging
import re
from pathlib import Path
from types import SimpleNamespace

import pytest

from eve_bridge import (
    lights,
    search_router,
    self_context,
    system_context,
    telegram,
    think,
    voice_agent,
    websearch,
)
from eve_bridge.codex_computer import CodexComputerAgent
from eve_bridge.daemon import Daemon

# The engine eve stopped using on 2026-09-24. It lives here, in a test, and never in a prompt.
RETIRED = re.compile(r"\bexa\b", re.I)
SHA = "abc1234"
CLOCK = "\n\nSystem context:\nCLOCK."


def owner_env(**extra: str) -> dict[str, str]:
    """The owner's environment's shape (switches and models as in their env file), with placeholder keys."""
    env = {"OPENAI_API_KEY": "test-openai", "OPENROUTER_API_KEY": "test-openrouter",
           "ANTHROPIC_API_KEY": "test-anthropic",
           "EVE_TELEGRAM": "1", "EVE_TELEGRAM_TOKEN": "test-token", "EVE_TELEGRAM_OWNER": "1",
           "EVE_TELEGRAM_MODEL": "gpt-6-luna", "EVE_THINK_MODEL": "gpt-6-astra",
           "EVE_WEB_SEARCH": "openrouter-perplexity", "EVE_MEMORY": "1", "EVE_MEM0": "1",
           "EVE_SECOND_BRAIN": "1", "EVE_MINIAPP": "1", "EVE_VOICE": "0",
           "EVE_WATCH_TLS": "0", "EVE_WATCH_BROWSER": "0"}
    env.update(extra)
    return env


@pytest.fixture
def lights_file(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A Govee and a HappyLighting light, in a temporary lights file that is also the module's default."""
    path = tmp_path / "lights.json"
    lights.save([lights.Light(name="floor lamp", kind="govee", ip="192.0.2.10", device="AA:BB", sku="H6008"),
                 lights.Light(name="desk strip", kind="triones", address="uuid-1")], path)
    monkeypatch.setattr(lights, "CONFIG_PATH", path)
    return path


@pytest.fixture(autouse=True)
def no_router(monkeypatch: pytest.MonkeyPatch) -> None:
    """The search router (search_router.py) ships off until its gates pass; every test starts with it off whatever
    SHIPPED says by then. A test that wants it routing says which providers (fake_router) or turns it on for real."""
    monkeypatch.setattr(search_router, "available", lambda env=None, engine="": ())


def fake_router(monkeypatch: pytest.MonkeyPatch, providers: tuple[str, ...], shipped: bool = True) -> None:
    """available() lists `providers`, and EVE_SEARCH_ROUTER=auto routes as if the router had SHIPPED (or not)."""
    monkeypatch.setattr(search_router, "available", lambda env=None, engine="": providers)
    monkeypatch.setattr(search_router, "SHIPPED", shipped)


# ---- the block follows the configuration ------------------------------------------------------------

def test_the_owners_setup_is_described_line_by_line(lights_file: Path) -> None:
    text = self_context.block(owner_env(), lights_path=lights_file, commit=SHA)
    assert text.startswith(self_context.HEADER) and text.endswith(self_context.RULE)
    body = text.splitlines()
    assert "- Telegram brain (texts, calls, desk push-to-talk): gpt-6-luna, low effort." in body
    assert "- think_hard: gpt-6-astra, high effort." in body
    assert f"- Web search: Perplexity through OpenRouter; {websearch.DEFAULT_MODEL} writes the answer." in body
    assert "- Lights: Govee and HappyLighting." in body
    assert f"- Running code: git commit {SHA}." in body
    assert any(ln.startswith("- Memory: day transcripts, records and profile") for ln in body)
    assert "- Doors on: Telegram, the Mini App and the Meet notetaker." in body
    assert not any(ln.startswith("- Voice:") for ln in body)          # EVE_VOICE=0: no voice line, no door


def test_a_different_environment_gives_different_lines(lights_file: Path, tmp_path: Path) -> None:
    base = self_context.lines(owner_env(), lights_path=lights_file, commit=SHA)
    changed = self_context.lines(owner_env(EVE_TELEGRAM_MODEL="gpt-6-astra", EVE_TELEGRAM_EFFORT="medium",
                                           EVE_WEB_SEARCH="openai", EVE_WATCH="0", EVE_MEMORY="0",
                                           EVE_SECOND_BRAIN="0", EVE_VOICE="1", EVE_THINK="0"),
                                 lights_path=tmp_path / "none.json", commit="")
    assert base != changed
    assert "Telegram brain (texts, calls, desk push-to-talk): gpt-6-astra, medium effort." in changed
    assert "Web search: OpenAI's hosted web search." in changed
    assert "Watcher: off." in changed and "Memory: off." in changed and "Lights: none set up." in changed
    assert not any(ln.startswith(("think_hard", "Running code")) for ln in changed)
    voice = [ln for ln in changed if ln.startswith("Voice:")]
    assert voice == [f"Voice: {voice_agent.DEFAULT_MODEL} talks; {voice_agent.DEFAULT_BACKEND_MODEL} "
                     f"({voice_agent.DEFAULT_BACKEND_EFFORT} effort) answers behind it."]
    assert "voice (wake word)" in next(ln for ln in changed if ln.startswith("Doors on:"))
    # every door off: the block still says so, in plain words
    bare = self_context.lines({"EVE_LIGHTS": "0", "EVE_VOICE": "0"}, commit="")
    assert "Lights: off." in bare and "Doors on: none." in bare and "Web search: off." in bare      # no OpenRouter key: no search


def _built_by_the_daemon(monkeypatch: pytest.MonkeyPatch, env: dict[str, str]) -> object:
    """The agent Daemon._make_agent really builds for a task on the Mac under `env` (its inner agent, made once)."""
    for name in ("EVE_COMPUTER", "EVE_COMPUTER_CONTROL"):
        monkeypatch.delenv(name, raising=False)
    for name, value in env.items():
        monkeypatch.setenv(name, value)
    made: list[object] = []
    monkeypatch.setattr("eve_bridge.app_reflex.ReflexFirstAgent",
                        lambda make_inner, *a, **k: made.append(make_inner()) or SimpleNamespace())
    monkeypatch.setattr(Daemon, "_bodies", lambda self, *a: {})
    Daemon._make_agent(SimpleNamespace(_codex_warm=None, _active_agent=None), lambda e: None, None)
    return made[0]


@pytest.mark.parametrize("computer", ["codex", ""])
def test_tasks_on_the_mac_name_the_agent_the_daemon_builds(computer: str, monkeypatch: pytest.MonkeyPatch,
                                                          lights_file: Path) -> None:
    """The line names the agent _make_agent really builds (Codex, the only desktop executor), checked against it."""
    extra = {"EVE_COMPUTER": computer} if computer else {}
    agent = _built_by_the_daemon(monkeypatch, extra)
    line = next(ln for ln in self_context.lines(owner_env(**extra), lights_path=lights_file, commit="")
                if ln.startswith("Tasks on the Mac:"))
    assert isinstance(agent, CodexComputerAgent)
    assert line == "Tasks on the Mac: Codex."
    off = self_context.lines(owner_env(EVE_COMPUTER_CONTROL="0", **extra), lights_path=lights_file, commit="")
    assert "Tasks on the Mac: off." in off


def test_the_reader_and_the_watcher_ladder_follow_the_tinyfish_key(lights_file: Path) -> None:
    without = self_context.lines(owner_env(EVE_WEB_READER="1"), lights_path=lights_file, commit="")
    assert "Tasks on the Mac: Codex." in without                          # no reader without its key
    assert "Watcher ladder: plain read, Perplexity search." in without
    with_key = self_context.lines(owner_env(EVE_WEB_READER="1", TINYFISH_API_KEY="test-tinyfish"),
                                  lights_path=lights_file, commit="")
    assert any(ln.startswith("Tasks on the Mac: Codex; web reads try TinyFish (") for ln in with_key)
    assert "Watcher ladder: plain read, TinyFish, Perplexity search." in with_key


def test_web_search_says_what_jev_routes_across_when_the_router_is_on(monkeypatch: pytest.MonkeyPatch,
                                                                      lights_file: Path) -> None:
    fake_router(monkeypatch, ("perplexity", "tinyfish", "firecrawl"))
    body = self_context.lines(owner_env(EVE_WEB_READER="1", TINYFISH_API_KEY="test-tinyfish"),
                              lights_path=lights_file, commit="")
    assert "Web search: Jev picks per query among Perplexity through OpenRouter, TinyFish and Firecrawl." in body
    assert "Tasks on the Mac: Codex; web reads try the routed search first." in body
    # only the providers usable here are named: a router that can only reach two says two, in its own order
    fake_router(monkeypatch, ("firecrawl", "perplexity"))
    assert "Web search: Jev picks per query among Perplexity through OpenRouter and Firecrawl." in self_context.lines(
        owner_env(), lights_path=lights_file, commit="")
    # mode off, or auto before the router SHIPPED, routes nothing, whatever available() lists
    fake_router(monkeypatch, ("perplexity", "tinyfish", "firecrawl"))
    off_mode = self_context.lines(owner_env(EVE_SEARCH_ROUTER="off"), lights_path=lights_file, commit="")
    assert any(ln.startswith("Web search: Perplexity through OpenRouter;") for ln in off_mode)
    fake_router(monkeypatch, ("perplexity", "tinyfish", "firecrawl"), shipped=False)
    unshipped = self_context.lines(owner_env(), lights_path=lights_file, commit="")
    assert any(ln.startswith("Web search: Perplexity through OpenRouter;") for ln in unshipped)
    assert "Jev picks" in " ".join(self_context.lines(owner_env(EVE_SEARCH_ROUTER="on"),
                                                      lights_path=lights_file, commit=""))
    # the real router, turned on: it lists what this environment's keys allow, and the block follows it
    monkeypatch.undo()
    env = owner_env(EVE_SEARCH_ROUTER="on", EVE_JEV_ROUTE="openrouter", FIRECRAWL_API_KEY="test-firecrawl")
    assert websearch.configured(env).providers == ("perplexity", "firecrawl")
    assert "Web search: Jev picks per query among Perplexity through OpenRouter and Firecrawl." in self_context.lines(
        env, lights_path=lights_file, commit="")
    off = self_context.lines(owner_env(EVE_SEARCH_ROUTER="off", FIRECRAWL_API_KEY="test-firecrawl"),
                             lights_path=lights_file, commit="")
    assert any(ln.startswith("Web search: Perplexity through OpenRouter;") for ln in off)


def test_the_rule_scopes_itself_to_what_eve_runs_on_and_leaves_the_tools_to_the_tool_list() -> None:
    """2026-09-29: "if it does not cover something, say you do not know" made the brain answer "I do not know"
    about its own calendar and camera. The rule now covers models, services and providers only, and sends a
    question about what eve can do to its tools."""
    rule = self_context.RULE
    assert "source of truth for the models, services and providers you run on" in rule
    assert "never guess" in rule and "What you can do is your tools." in rule
    assert "tools" not in rule.split("never guess")[0]              # "do not know" is not about the tools


def test_a_part_that_fails_leaves_no_block_and_no_error(monkeypatch: pytest.MonkeyPatch) -> None:
    def broken(env: object) -> str:
        raise RuntimeError("boom")

    monkeypatch.setattr(self_context, "_search", broken)
    assert self_context.block(owner_env(), commit="") == ""


def test_building_it_does_not_log_the_doors_warnings_again(caplog: pytest.LogCaptureFixture, lights_file: Path) -> None:
    caplog.set_level(logging.WARNING)
    telegram.configured(owner_env(EVE_TELEGRAM_EFFORT="bogus"))
    assert caplog.text.count("EVE_TELEGRAM_EFFORT") == 1


def apps_env(tmp_path: Path, **extra: str) -> dict[str, str]:
    """The owner's environment with every app-shaped part on: the direct apps signed in to Google (a token file in a
    temporary folder) and Maps (a key), Composio with a key, file sharing."""
    folder = tmp_path / "apps"
    folder.mkdir(exist_ok=True)
    (folder / "google-token.json").write_text(json.dumps({"refresh_token": "test-refresh"}))
    on = {"EVE_APPS": "1", "EVE_APPS_DIR": str(folder), "EVE_GOOGLE_MAPS_KEY": "test-maps",
          "EVE_COMPOSIO": "1", "COMPOSIO_API_KEY": "test-composio", "EVE_SHARE": "1"}
    return owner_env(**{**on, **extra})


def test_the_owners_apps_and_sharing_each_get_one_line_while_on(tmp_path: Path, lights_file: Path) -> None:
    from eve_bridge import share

    body = self_context.lines(apps_env(tmp_path), lights_path=lights_file, commit="")
    # signed in to Google and Maps, not GitHub: GitHub is not named, and each app carries its own write rule
    assert "Own apps: Google Contacts and Google Maps read only; Gmail, Google Calendar and Google Drive routine " \
           "changes run, sends and deletes asked first." in body
    # every signed-in app is named under some rule: a policy missing from _APP_RULES would drop it silently
    from eve_bridge import apps_mcp
    assert {rule for rule, _ in self_context._APP_RULES} == set(apps_mcp.POLICIES)
    assert "Composio apps: Slack, Notion and Linear." in body
    sharing = [ln for ln in body if ln.startswith("File sharing:")]
    vias = (["iMessage"] if share.configured({"EVE_SHARE": "1"}).imessage else []) + ["email as a Drive link"]
    assert sharing == [f"File sharing: {self_context._join(vias)}, on the owner's Allow."]
    # off (or Telegram, their only door, off): none of the three lines, and the block is what it was
    for env in (owner_env(), apps_env(tmp_path, EVE_TELEGRAM="0"),
                apps_env(tmp_path, EVE_APPS="0", EVE_COMPOSIO="0", EVE_SHARE="0")):
        lines = self_context.lines(env, lights_path=lights_file, commit="")
        assert not any(ln.startswith(("Own apps", "Composio apps", "File sharing")) for ln in lines)


# ---- the budget --------------------------------------------------------------------------------------

def test_the_block_stays_under_its_budget_with_everything_on(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    path = tmp_path / "lights.json"
    lights.save([lights.Light(name="a", kind="govee", ip="192.0.2.1"), lights.Light(name="b", kind="wiz", ip="192.0.2.2"),
                 lights.Light(name="c", kind="triones", address="u"), lights.Light(name="d", kind="tuya", id="x")], path)
    fake_router(monkeypatch, ("perplexity", "tinyfish", "firecrawl"))
    env = owner_env(EVE_VOICE="1", EVE_WEB_READER="1", TINYFISH_API_KEY="test-tinyfish",
                    EVE_WATCH_TLS="1", EVE_WATCH_BROWSER="1",
                    EVE_CHIEF="on", CANVAS_BASE_URL="https://canvas.example.edu", CANVAS_API_TOKEN="test-canvas")
    env.update({k: v for k, v in apps_env(tmp_path).items() if k not in env})
    text = self_context.block(env, lights_path=path, commit="abcdef123456")
    assert len(text) < self_context.BUDGET, len(text)
    assert "Voice:" in text and "Jev picks" in text and "Sylvania (Tuya)" in text and "Tasks on the Mac: Codex" in text
    assert "Canvas (" in text and "Chief of staff: on." in text
    assert "Own apps:" in text and "Composio apps:" in text and "File sharing:" in text


# ---- the three doors carry it ----------------------------------------------------------------------

def test_the_telegram_brain_carries_it_right_after_its_instructions(lights_file: Path, monkeypatch) -> None:
    monkeypatch.setattr(system_context, "context", lambda: CLOCK)
    env = owner_env()
    cfg = telegram.configured(env)
    assert cfg.about == self_context.block(env) and cfg.about
    items = [telegram.message_item("user", "what do you use for search")]
    body = telegram.request(cfg, items, profile="PROFILE", today="TODAY")
    assert body["instructions"].startswith(telegram.INSTRUCTIONS + cfg.about + telegram.PROFILE_HEADER)
    # stable from turn to turn: the block is in the cached prefix, the clock is not
    again = telegram.request(cfg, items + [telegram.message_item("user", "and for reading pages?")],
                             profile="PROFILE", today="TODAY more")
    prefix = telegram.INSTRUCTIONS + cfg.about + telegram.PROFILE_HEADER + "\nPROFILE"
    assert body["instructions"].startswith(prefix) and again["instructions"].startswith(prefix)
    assert "CLOCK" not in body["instructions"] and body["prompt_cache_key"] == telegram.PROMPT_CACHE_KEY
    # a hand-built config (the other tests) has none, and the instructions are as before
    assert telegram.request(telegram.TelegramConfig(), items)["instructions"] == telegram.INSTRUCTIONS


def test_the_voice_carries_it_in_both_halves(lights_file: Path) -> None:
    env = owner_env(EVE_VOICE="1")
    cfg = voice_agent.configured(env)
    assert cfg.about == self_context.block(env) and "Voice:" in cfg.about
    got = voice_agent.session_config(cfg, profile="VOICE-PAGE", backend_profile="FULL-PAGE",
                                     memory_tools=[{"type": "function", "name": "memory_search"}])
    assert got["instructions"].startswith(voice_agent.INSTRUCTIONS + cfg.about + voice_agent.PROFILE_HEADER)
    back = got["delegation"]["responses"]["instructions"]
    assert back.startswith(voice_agent.BACKEND_INSTRUCTIONS + voice_agent.BACKEND_MEMORY_RULES + cfg.about
                           + voice_agent.BACKEND_PROFILE_HEADER)
    plain = voice_agent.session_config(voice_agent.VoiceConfig())
    assert self_context.HEADER not in json.dumps(plain)


def test_think_carries_it_before_the_background_in_every_round(lights_file: Path, monkeypatch) -> None:
    monkeypatch.setattr(system_context, "context", lambda: CLOCK)
    env = owner_env()
    cfg = think.configured(env)
    assert cfg.about == self_context.block(env) and cfg.about
    seen: list[str] = []
    replies = iter([
        {"status": "completed", "output": [{"type": "function_call", "name": "web_search", "call_id": "c1",
                                            "arguments": "{\"query\": \"q\"}"}]},
        {"status": "completed", "output": [{"type": "message", "role": "assistant",
                                            "content": [{"type": "output_text", "text": "Perplexity."}]}]}])

    async def create(req: dict) -> dict:
        seen.append(req["instructions"])
        return next(replies)

    thinker = think.OpenAIThinker(cfg, create=create, search=lambda q: {"ok": True})
    assert asyncio.run(thinker("what do you search with", context="BG"))["answer"] == "Perplexity."
    want = think.INSTRUCTIONS + cfg.about + think.CONTEXT_HEADER + "BG" + CLOCK
    assert seen == [want, want]


# ---- no retired name -------------------------------------------------------------------------------

def test_no_prompt_names_the_retired_engine(lights_file: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Every prompt the three doors send, tools included, on the owner's setup: the grep finds nothing. The
    positive control is the same grep on a setup that really does run the retired engine: then the block names it,
    because there it would be true (and the grep is shown to work)."""
    fake_router(monkeypatch, ("perplexity", "tinyfish", "firecrawl"))
    env = owner_env(EVE_VOICE="1")
    items = [telegram.message_item("user", "what do you use for search")]
    bodies = [telegram.request(telegram.configured(env), items),
              voice_agent.session_config(voice_agent.configured(env)),
              think.request(think.configured(env), think.question_items("q"), "BG")]
    for body in bodies:
        assert not RETIRED.search(json.dumps(body)), RETIRED.search(json.dumps(body))
    assert all(self_context.RULE in json.dumps(b) for b in bodies)
    fake_router(monkeypatch, ())
    control = self_context.block(owner_env(EVE_WEB_SEARCH="openrouter-" + "exa"), commit="")
    assert RETIRED.search(control)
