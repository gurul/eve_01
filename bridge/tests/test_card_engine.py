"""The card engine switch (EVE_CARD_ENGINE, 2026-11-16): which format an app turn is told to write, that one engine's
instructions never name the other's, what each kind of turn's history shows of a card, eve lite's wording, and
tools/openui_eval.py's reading of a json-render reply. No network. The tests that run the app's own validator
(ios/EveLink/Web/scripts/validate-spec.mjs) are skipped when node or the web packages are not installed."""

from __future__ import annotations

import importlib.util
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest
from test_telegram import CFG

from eve_bridge import link_chat, telegram

BRIDGE = Path(__file__).resolve().parents[1]
WEB = BRIDGE.parent / "ios" / "EveLink" / "Web"
TOOL = BRIDGE / "tools" / "openui_eval.py"
VALIDATOR = WEB / "scripts" / "validate-spec.mjs"
GEN = WEB / "scripts" / "gen-json-prompt.mjs"
CAN_PARSE = shutil.which("node") is not None and (WEB / "node_modules" / "@json-render" / "core").exists()
needs_parser = pytest.mark.skipif(not CAN_PARSE, reason="node or the web packages (pnpm install) are missing")

SPEC = ('{"op":"add","path":"/root","value":"card"}\n'
        '{"op":"add","path":"/elements/card","value":{"type":"Card","props":{},"children":["t"]}}\n'
        '{"op":"add","path":"/elements/t","value":{"type":"Text","props":{"text":"x"},"children":[]}}')
JSON_REPLY = f"Here it is.\n\n```json-render\n{SPEC}\n```\n\nAnything else?"
OPENUI_REPLY = "Here it is.\n\n```openui-lang\nroot = Card([t])\nt = TextContent(\"x\")\n```\n\nAnything else?"


@pytest.fixture(autouse=True)
def engine_unset(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(link_chat.CARD_ENGINE_ENV, raising=False)
    monkeypatch.delenv("EVE_PROFILE", raising=False)


@pytest.fixture
def ev(monkeypatch: pytest.MonkeyPatch) -> Any:
    spec = importlib.util.spec_from_file_location("openui_eval", TOOL)
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    monkeypatch.setitem(sys.modules, "openui_eval", mod)
    spec.loader.exec_module(mod)
    return mod


def fenced(text: str) -> list[str]:
    return re.findall(r"```json-render\n(.*?)\n```", text, re.DOTALL)


# ---- the switch ---------------------------------------------------------------------------------------------

@pytest.mark.parametrize(("value", "engine"), [
    (None, "openui"), ("", "openui"), ("openui", "openui"), ("0", "openui"), ("anything", "openui"),
    ("json-render", "json-render"), ("JSON-Render", "json-render"), (" json_render ", "json-render"),
    ("jsonrender", "json-render"),
])
def test_only_the_json_render_values_leave_the_default(value: Any, engine: str) -> None:
    env = {} if value is None else {link_chat.CARD_ENGINE_ENV: value}
    assert link_chat.card_engine(env) == engine


def test_the_switch_is_read_from_the_process_environment_on_every_turn(monkeypatch: pytest.MonkeyPatch) -> None:
    assert link_chat.app_block().startswith(link_chat.APP_RULE)
    monkeypatch.setenv(link_chat.CARD_ENGINE_ENV, "json-render")
    assert link_chat.app_block().startswith(link_chat.JSON_APP_RULE)
    monkeypatch.delenv(link_chat.CARD_ENGINE_ENV)
    assert link_chat.app_block().startswith(link_chat.APP_RULE)


# ---- the app block ------------------------------------------------------------------------------------------

def test_each_engines_block_is_its_rule_its_reference_and_its_reminder() -> None:
    guide = link_chat.JSON_PROMPT_PATH.read_text(encoding="utf-8").strip()
    assert guide, "json_render_prompt.txt is missing: run pnpm gen-json-prompt in ios/EveLink/Web"
    block = link_chat.app_block(engine="json-render")
    assert block == link_chat.JSON_APP_RULE + "\n\n" + guide + "\n\n" + link_chat.JSON_APP_LINE
    assert link_chat.JSON_APP_EXAMPLE in block and len(fenced(link_chat.JSON_APP_EXAMPLE)) == 1
    openui = link_chat.app_block(engine="openui")
    assert openui == link_chat.app_block() and openui.startswith(link_chat.APP_RULE)


def test_one_engines_block_never_names_the_other() -> None:
    """Both ways, so each absence sits beside the presence that shows the search can find the name."""
    json_block = link_chat.app_block(engine="json-render").lower()
    openui_block = link_chat.app_block(engine="openui").lower()
    assert "```json-render" in json_block and "openui" not in json_block
    assert "```openui-lang" in openui_block and "json-render" not in openui_block


def test_the_json_render_rule_is_the_openui_rule_with_only_the_block_changed() -> None:
    """A week on this engine differs from OpenUI in the engine alone: the two-way decision reads the same."""
    def decision(rule: str) -> str:
        return rule[rule.index("Before you write"):rule.index("The block:")]

    assert decision(link_chat.JSON_APP_RULE) == decision(link_chat.APP_RULE).replace(
        "an openui-lang block", "a json-render block").replace("the openui-lang block", "the json-render block")
    assert link_chat.JSON_APP_LINE == link_chat.APP_LINE.replace("one openui-lang block", "one json-render block")


def test_a_missing_reference_means_no_block_at_all(tmp_path: Path) -> None:
    assert link_chat.app_block(tmp_path / "absent.txt", engine="json-render") == ""


@needs_parser
def test_every_block_the_brain_is_shown_passes_the_apps_validator() -> None:
    guide = link_chat.JSON_PROMPT_PATH.read_text(encoding="utf-8")
    blocks = fenced(link_chat.JSON_APP_EXAMPLE) + fenced(guide)
    assert len(blocks) >= 6
    run = subprocess.run(["node", str(VALIDATOR), "--batch"], input=json.dumps(blocks + ["not a block"]),
                         capture_output=True, text=True, timeout=120)
    results = json.loads(run.stdout)
    assert [r["ok"] for r in results] == [True] * len(blocks) + [False], results     # the last is the control
    assert all(r["root"] == "Card" and r["adopted"] == [] for r in results[:-1])


@needs_parser
def test_the_committed_reference_is_what_the_catalog_generates() -> None:
    run = subprocess.run(["node", str(GEN), "--check"], capture_output=True, text=True, timeout=120)
    assert run.returncode == 0, run.stderr or run.stdout


# ---- the whole request ---------------------------------------------------------------------------------------

def instructions(app: bool = True) -> str:
    items = [telegram.message_item("user", "hi")]
    return str(telegram.request(CFG, items, app_block=link_chat.app_block() if app else "")["instructions"])


def test_the_app_style_names_the_block_the_engine_writes(monkeypatch: pytest.MonkeyPatch) -> None:
    assert link_chat.BLOCK_WORDS[0] in telegram.APP_STYLE and link_chat.BLOCK_WORDS[0] in telegram.APP_STYLE_LITE
    assert telegram.APP_STYLE in instructions()
    monkeypatch.setenv(link_chat.CARD_ENGINE_ENV, "json-render")
    text = instructions()
    assert telegram.APP_STYLE.replace(*link_chat.BLOCK_WORDS) in text
    assert "openui" not in text.lower() and text.count("json-render") > 10
    assert text.index("Whether a reply is plain text or a json-render block") < text.index(link_chat.JSON_APP_RULE)
    # a text turn is the same whatever the engine: no app block, no block named
    assert "json-render" not in instructions(app=False) and "openui" not in instructions(app=False).lower()


def test_eve_lite_reads_the_json_render_block_in_its_web_apps_words(monkeypatch: pytest.MonkeyPatch) -> None:
    block = link_chat.app_block(engine="json-render")
    for old, new in telegram.LITE_APP_SWAPS_JSON:
        assert block.count(old) == 1, old
        assert new not in block
    assert block.count(telegram.LITE_APP_SWAPS[1][0]) == 1                 # the reminder line opens alike
    lite = telegram.lite_app_block(block)
    assert "## Replying in eve's web app" in lite and "This turn is from eve's web app:" in lite
    assert not [w for w in ("Eve Link", "iPhone", "native cards", "openui") if w in lite]
    monkeypatch.setenv("EVE_PROFILE", "lite")
    monkeypatch.setenv(link_chat.CARD_ENGINE_ENV, "json-render")
    text = instructions()
    assert telegram.APP_STYLE_LITE.replace(*link_chat.BLOCK_WORDS) in text and "openui" not in text.lower()
    assert "Eve Link" not in text


# ---- a card outside the turn that can draw it ---------------------------------------------------------------

def test_a_json_render_block_is_one_plain_line_outside_the_app() -> None:
    p = link_chat.OPENUI_PLACEHOLDER
    assert link_chat.strip_openui(JSON_REPLY) == f"Here it is.\n\n{p}\n\nAnything else?"
    assert link_chat.strip_openui(f"Look:\n```json-render\n{SPEC}") == f"Look:\n{p}"          # unclosed
    both = f"{JSON_REPLY}\n\n```openui-lang\nroot = Card([])\n```"
    assert link_chat.strip_openui(both) == f"Here it is.\n\n{p}\n\nAnything else?\n\n{p}"
    plain = "A list:\n\n- one\n- two\n\n```json\n{\"a\": 1}\n```"
    assert link_chat.strip_openui(plain) == plain                                             # other code untouched
    assert link_chat.history_text(JSON_REPLY, app=False) == link_chat.strip_openui(JSON_REPLY)


def test_an_app_turn_reads_its_own_engines_cards_whole_and_the_others_as_one_line() -> None:
    gone = link_chat.CARD_ELSEWHERE
    assert link_chat.history_text(JSON_REPLY, True, "json-render") == JSON_REPLY
    assert link_chat.history_text(OPENUI_REPLY, True, "openui") == OPENUI_REPLY
    assert link_chat.history_text(OPENUI_REPLY, True, "json-render") == f"Here it is.\n\n{gone}\n\nAnything else?"
    assert link_chat.history_text(JSON_REPLY, True, "openui") == f"Here it is.\n\n{gone}\n\nAnything else?"
    assert link_chat.history_text("root = Card([])\n", True, "json-render") == gone             # a bare program
    # the placeholder from before app history kept programs still reads as gone, on either engine
    old = f"Then: {link_chat.OPENUI_PLACEHOLDER}"
    for engine in ("openui", "json-render"):
        assert link_chat.history_text(old, True, engine) == f"Then: {link_chat.OPENUI_GONE}"


def test_an_app_turns_history_follows_the_switch(monkeypatch: pytest.MonkeyPatch) -> None:
    assert link_chat.history_text(OPENUI_REPLY, True) == OPENUI_REPLY
    monkeypatch.setenv(link_chat.CARD_ENGINE_ENV, "json-render")
    assert "openui" not in link_chat.history_text(OPENUI_REPLY, True)
    assert link_chat.history_text(JSON_REPLY, True) == JSON_REPLY


# ---- tools/openui_eval.py, --engine json-render --------------------------------------------------------------

@pytest.mark.parametrize(("reply", "uses", "unclosed", "n"), [
    (JSON_REPLY, True, False, 1),
    (f"```json-render\n{SPEC}\n```", True, False, 1),
    ("Just text.", False, False, 0),
    (f"Half:\n```json-render\n{SPEC}", False, True, 0),                                   # unclosed: not a block
    (f"A\n```json-render\n{SPEC}\n```\nB\n```json-render\n{SPEC}\n```", True, False, 2),
    (OPENUI_REPLY, False, False, 0),                                                      # the other engine's
    ("root = Card([])", False, False, 0),                                                 # no bare form here
    ("```json\n{\"a\": 1}\n```", False, False, 0),
])
def test_the_eval_reads_a_json_render_reply_as_the_app_does(ev: Any, reply: str, uses: bool, unclosed: bool,
                                                            n: int) -> None:
    found, left_open = ev.programs(reply, "json-render")
    assert (bool(found), left_open, len(found)) == (uses, unclosed, n)
    assert ev.uses_ui(reply, "json-render") is uses


def test_the_eval_marks_the_other_engines_block_and_never_counts_it(ev: Any) -> None:
    rows = [{"id": "a", "label": "ui", "reply": OPENUI_REPLY}, {"id": "b", "label": "plain", "reply": "Hi."},
            {"id": "c", "label": "plain", "reply": f"```json-render\n{SPEC}"}]
    ev.score_rows(rows, None, "json-render")
    assert [(r["uses_ui"], r["other_engine"], r["pass"]) for r in rows] == [
        (False, True, False), (False, False, True), (False, False, False)]
    assert all(r["engine"] == "json-render" for r in rows)


@needs_parser
def test_the_eval_scores_json_render_blocks_with_the_apps_validator(ev: Any) -> None:
    rows = [{"id": "good", "label": "ui", "reply": JSON_REPLY},
            {"id": "bad", "label": "ui", "reply": JSON_REPLY.replace('"Text"', '"NotInTheCatalog"')}]
    validator = ev.find_validator("", "json-render")
    assert validator is not None and validator.name == "validate-spec.mjs"
    ev.score_rows(rows, validator, "json-render")
    assert [(r["uses_ui"], r["parse_ok"], r["pass"]) for r in rows] == [(True, True, True), (True, False, False)]
    assert rows[1]["parse_errors"]


def test_the_evals_made_up_environment_sets_the_engine_only_when_asked(ev: Any, tmp_path: Path,
                                                                        monkeypatch: pytest.MonkeyPatch) -> None:
    import os

    monkeypatch.setattr(os, "environ", {"PATH": "/usr/bin", "OPENROUTER_API_KEY": "k"})
    ev.environment("", "", tmp_path)
    assert link_chat.CARD_ENGINE_ENV not in os.environ
    ev.environment("", "", tmp_path, "json-render")
    assert os.environ[link_chat.CARD_ENGINE_ENV] == "json-render"
