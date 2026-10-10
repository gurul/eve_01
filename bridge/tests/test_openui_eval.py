"""OpenUI in Eve Link app turns (2026-10-08): the app block's content and order, the texting style swapped for app
turns only, the trimmed openui-lang reference naming only components it describes, and tools/openui_eval.py's reading
of a reply (uses_ui) and its run of a real app turn with a fake model. No network. The tests that run the app's parser
(ios/EveLink/Web/scripts/validate.mjs) are skipped when node or the web packages are not installed."""

from __future__ import annotations

import asyncio
import importlib.util
import json
import re
import shutil
import sys
from pathlib import Path
from typing import Any

import pytest
from test_telegram import CFG, call, say

from eve_bridge import link_chat, telegram

BRIDGE = Path(__file__).resolve().parents[1]
REPO = BRIDGE.parent
WEB = REPO / "ios" / "EveLink" / "Web"
TOOL = BRIDGE / "tools" / "openui_eval.py"
GEN = WEB / "scripts" / "gen-prompt.mjs"
VALIDATOR = WEB / "scripts" / "validate.mjs"
CAN_PARSE = shutil.which("node") is not None and (WEB / "node_modules" / "@openuidev" / "react-lang").exists()
needs_parser = pytest.mark.skipif(not CAN_PARSE, reason="node or the web packages (pnpm install) are missing")


@pytest.fixture
def ev(monkeypatch: pytest.MonkeyPatch) -> Any:
    spec = importlib.util.spec_from_file_location("openui_eval", TOOL)
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    monkeypatch.setitem(sys.modules, "openui_eval", mod)
    spec.loader.exec_module(mod)
    return mod


def chat_components() -> set[str]:
    """CHAT_COMPONENTS as gen-prompt.mjs lists them."""
    src = GEN.read_text(encoding="utf-8")
    block = src[src.index("export const CHAT_COMPONENTS = ["):]
    block = block[:block.index("];")]
    return set(re.findall(r'"([A-Z][A-Za-z0-9]*)"', block))


def called(text: str) -> set[str]:
    """Component names written as calls (an @-prefixed action step is not one)."""
    return set(re.findall(r"(?<![@\w])([A-Z][A-Za-z0-9]*)\(", text))


def fenced(text: str) -> list[str]:
    return re.findall(r"```openui-lang\n(.*?)\n```", text, re.DOTALL)


# ---- the app block -------------------------------------------------------------------------------------------

def test_the_app_block_puts_the_decision_rule_first_then_the_reference_then_the_reminder() -> None:
    block = link_chat.app_block()
    guide = link_chat.PROMPT_PATH.read_text(encoding="utf-8").strip()
    assert block == link_chat.APP_RULE + "\n\n" + guide + "\n\n" + link_chat.APP_LINE
    assert block.startswith("## Replying in the Eve Link app")
    rule = link_chat.APP_RULE
    # a two-way decision: when a block helps, and plain text for the rest
    for shape in ("comparison", "options", "steps", "schedule", "chart", "tap"):
        assert shape in rule
    for chat in ("chat", "greeting", "thanks", "feelings", "single fact"):
        assert chat in rule
    assert "```openui-lang" in rule and "root = Card(" in rule
    assert len(rule) < 2500 and len(link_chat.APP_LINE) < 300                   # short, ahead of the reference
    assert link_chat.APP_EXAMPLE in rule and len(fenced(link_chat.APP_EXAMPLE)) == 1


def test_the_rule_and_its_example_name_only_components_the_reference_describes() -> None:
    kept = chat_components()
    assert called(link_chat.APP_RULE) <= kept
    assert called(link_chat.APP_LINE) == set()


def test_the_shipped_reference_names_only_its_own_components_and_lists_each_one() -> None:
    kept = chat_components()
    guide = link_chat.PROMPT_PATH.read_text(encoding="utf-8")
    assert called(guide) <= kept | {"Action"}
    for name in kept:
        assert re.search(rf"^{name}\(", guide, re.M), f"{name} has no signature in the reference"
    assert 8_000 < len(guide) < 25_000                                             # trimmed from 54,241


@needs_parser
def test_the_rule_example_and_every_reference_example_parse_with_the_apps_parser(ev: Any) -> None:
    guide = link_chat.PROMPT_PATH.read_text(encoding="utf-8")
    programs = fenced(link_chat.APP_EXAMPLE) + fenced(guide)
    assert len(programs) >= 4
    results = ev.validate(programs, VALIDATOR)
    assert all(r["ok"] and r["root"] == "Card" and not r["orphaned"] for r in results), results


@needs_parser
def test_the_shipped_reference_is_what_gen_prompt_writes() -> None:
    import subprocess

    p = subprocess.run(["node", str(GEN), "--check"], capture_output=True, text=True, timeout=120)
    assert p.returncode == 0, p.stderr


# ---- the texting style, app turns only -------------------------------------------------------------------------

def test_an_app_turn_reads_the_app_style_and_a_text_keeps_the_texting_style() -> None:
    items = [telegram.message_item("user", "compare these")]
    text = telegram.request(CFG, items)["instructions"]
    app = telegram.request(CFG, items, app_block=link_chat.app_block())["instructions"]
    assert telegram._TEXT_STYLE in text and telegram.APP_STYLE not in text
    assert telegram._TEXT_STYLE not in app and telegram.APP_STYLE in app
    assert "no lists" in text and "no lists" not in app and "no markdown" not in app
    assert "no emoji" in app and "no dashes" in app                                   # the voice stays
    # the only differences: the style, and the block at the end of the stable part
    assert app.replace("\n\n" + link_chat.app_block(), "").replace(telegram.APP_STYLE, telegram._TEXT_STYLE) == text
    assert app.index(telegram.APP_STYLE) < app.index(link_chat.APP_RULE)


def test_the_robot_instructions_swap_the_same_style(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(telegram, "robot_enabled", lambda: True)
    items = [telegram.message_item("user", "hi")]
    text = telegram.request(CFG, items)["instructions"]
    app = telegram.request(CFG, items, app_block="BLOCK")["instructions"]
    assert text.startswith(telegram.INSTRUCTIONS_ROBOT) and telegram._TEXT_STYLE in text
    assert app.startswith(telegram.app_instructions(telegram.INSTRUCTIONS_ROBOT))
    assert telegram.APP_STYLE in app and telegram._TEXT_STYLE not in app


def test_a_call_note_and_the_texting_style_are_untouched_by_the_app_style() -> None:
    assert "no Markdown, lists or emoji" in telegram.CALL_NOTE
    assert telegram.INSTRUCTIONS.count(telegram._TEXT_STYLE) == 1
    assert telegram.app_instructions(telegram.INSTRUCTIONS).count(telegram.APP_STYLE) == 1


# ---- the harness: reading a reply --------------------------------------------------------------------------------

@pytest.mark.parametrize("reply,ui,unclosed,n", [
    ("Canberra.", False, False, 0),
    ("Here:\n```openui-lang\nroot = Card([t])\nt = TextContent(\"x\")\n```\nAnything else?", True, False, 1),
    ("```openui-lang\nroot = Card([t])\nt = TextContent(\"x\")\n```", True, False, 1),
    ("root = Card([t])\nt = TextContent(\"x\")", True, False, 1),                 # a bare program
    ("Sure.\nroot = Card([t])\nt = TextContent(\"x\")", True, False, 1),          # prose, then a bare program
    ("Set the root = 5 and retry.", False, False, 0),                               # prose about root
    ("```python\nroot = Card(x)\n```", False, False, 0),                            # another language's fence
    ("Half:\n```openui-lang\nroot = Card([t])", False, True, 0),                   # unclosed: not a block
    ("A\n```openui-lang\nroot = Card([a])\n```\nB\n```openui-lang\nroot = Card([b])\n```", True, False, 2),
    ("| a | b |\n|---|---|\n| 1 | 2 |", False, False, 0),                           # a Markdown table is not UI
])
def test_uses_ui_reads_a_reply_the_way_the_app_does(ev: Any, reply: str, ui: bool, unclosed: bool, n: int) -> None:
    found, open_fence = ev.programs(reply)
    assert ev.uses_ui(reply) is ui and open_fence is unclosed and len(found) == n


def test_scoring_by_label(ev: Any) -> None:
    rows = [
        {"id": "a", "label": "ui", "reply": "```openui-lang\nroot = Card([])\n```"},
        {"id": "b", "label": "ui", "reply": "Pasta, tacos or soup?"},
        {"id": "c", "label": "plain", "reply": "Canberra."},
        {"id": "d", "label": "plain", "reply": "```openui-lang\nroot = Card(["},
        {"id": "e", "label": "other", "reply": "hi"},
    ]
    ev.score_rows(rows, None)                                                       # no validator: parse unknown
    assert [r["uses_ui"] for r in rows] == [True, False, False, False, False]
    assert rows[0]["parse_ok"] is None and rows[0]["pass"] is False                 # a block must also parse
    assert [r["pass"] for r in rows[1:]] == [False, True, False, None]


@needs_parser
def test_scoring_with_the_validator(ev: Any) -> None:
    rows = [
        {"id": "good", "label": "ui", "reply": "Here.\n```openui-lang\nroot = Card([t])\nt = TextContent(\"x\")\n```"},
        {"id": "bad", "label": "ui", "reply": "```openui-lang\nroot = Card([t])\nt = NotInTheLibrary(\"x\")\n```"},
        {"id": "plain", "label": "plain", "reply": "Hi!"},
    ]
    ev.score_rows(rows, VALIDATOR)
    assert [r["parse_ok"] for r in rows] == [True, False, None]
    assert [r["pass"] for r in rows] == [True, False, True]
    assert any("unknown-component" in e for e in rows[1]["parse_errors"])


def test_a_validator_crash_still_leaves_every_reply_on_disk(ev: Any, tmp_path: Path,
                                                             monkeypatch: pytest.MonkeyPatch) -> None:
    rows = [
        {"id": "a", "label": "ui", "reply": "```openui-lang\nroot = Card([t])\nt = TextContent(\"x\")\n```", "arm": "new"},
        {"id": "b", "label": "plain", "reply": "Canberra.", "arm": "new"},
    ]

    def crash(codes: list[str], validator: Path, node: str = "") -> list[dict[str, Any]]:
        raise SystemExit("validator failed (exit 134): out of memory")

    monkeypatch.setattr(ev, "validate", crash)
    out = tmp_path / "replies.json"
    assert ev.finish(rows, "new", tmp_path / "validate.mjs", out) == 1
    saved = json.loads(out.read_text(encoding="utf-8"))
    assert [r["reply"] for r in saved["rows"]] == [r["reply"] for r in rows]
    assert saved["summary"] is None and not (tmp_path / "replies.json.tmp").exists()

    # --rescore works from that file once the validator is back
    monkeypatch.setattr(ev, "validate", lambda codes, validator, node="": [
        {"ok": True, "root": "Card", "errors": [], "orphaned": []} for _ in codes])
    monkeypatch.setattr(ev, "find_validator", lambda arg="": tmp_path / "validate.mjs")
    assert ev.main(["--rescore", str(out)]) == 0


def test_with_no_out_the_replies_go_to_a_named_temp_file(ev: Any, monkeypatch: pytest.MonkeyPatch,
                                                         capsys: pytest.CaptureFixture[str]) -> None:
    rows = [{"id": "a", "label": "plain", "reply": "Hi.", "arm": "x"}]
    monkeypatch.setattr(ev, "score_rows", lambda rows, validator: (_ for _ in ()).throw(RuntimeError("boom")))
    assert ev.finish(rows, "x", None, None) == 1
    err = capsys.readouterr().err
    path = Path(err.split("--rescore ")[1].strip())
    try:
        assert json.loads(path.read_text(encoding="utf-8"))["rows"][0]["reply"] == "Hi."
    finally:
        path.unlink()


def test_a_scored_run_writes_the_scores_over_the_raw_file(ev: Any, tmp_path: Path) -> None:
    rows = [{"id": "c", "label": "plain", "reply": "Canberra.", "arm": "x"}]
    out = tmp_path / "r.json"
    assert ev.finish(rows, "x", None, out) == 0
    saved = json.loads(out.read_text(encoding="utf-8"))
    assert saved["rows"][0]["pass"] is True and saved["summary"]["pass"] == 1


def test_the_dev_cases_load_and_are_labelled(ev: Any) -> None:
    cases = ev.load_cases(ev.DEV_CASES)
    assert len(cases) == 10 and len({c["id"] for c in cases}) == 10
    assert {c["label"] for c in cases} == {"ui", "plain"}
    assert all(ev.case_text(c) for c in cases)


def test_cases_in_other_shapes_load(ev: Any, tmp_path: Path) -> None:
    path = tmp_path / "c.json"
    path.write_text(json.dumps([{"message": "hi", "expect_ui": False}, {"id": 7, "prompt": "plan", "label": "ui"}]))
    cases = ev.load_cases(path)
    assert [c["id"] for c in cases] == ["case-1", "7"]
    assert [ev.expected(c["label"]) for c in cases] == [False, True]
    assert [ev.case_text(c) for c in cases] == ["hi", "plan"]


# ---- the harness: a real app turn, with a fake model -------------------------------------------------------------

def test_a_case_runs_as_an_app_turn_with_the_app_block_and_refused_tools(ev: Any,
                                                                         monkeypatch: pytest.MonkeyPatch) -> None:
    for k, v in {"EVE_TELEGRAM": "1", "EVE_TELEGRAM_TOKEN": "0:test", "EVE_TELEGRAM_OWNER": "1",
                 "EVE_MEMORY": "0", "EVE_WEB_SEARCH": "off"}.items():
        monkeypatch.setenv(k, v)
    program = "Here.\n```openui-lang\nroot = Card([t])\nt = TextContent(\"x\")\n```"
    script = [call("start_task", {"goal": "open the browser"}), say(program)]
    requests: list[dict[str, Any]] = []

    async def create(payload: dict[str, Any]) -> dict[str, Any]:
        requests.append(payload)
        return script.pop(0)

    row = asyncio.run(ev.run_case({"id": "x", "label": "ui", "text": "compare two phones"}, create))
    assert row["reply"] == program and row["error"] == "" and row["tools"] == ["start_task"]
    first = requests[0]["instructions"]
    assert link_chat.APP_RULE in first and link_chat.APP_LINE in first and telegram.APP_STYLE in first
    assert row["instr_chars"] == len(first)
    ev.score_rows([row], None)
    assert row["uses_ui"] is True


def test_a_text_turn_through_the_same_inlet_gets_no_app_block(ev: Any, monkeypatch: pytest.MonkeyPatch) -> None:
    """The control: the harness's app marking is what adds the block (a Telegram turn would not have it)."""
    monkeypatch.setenv("EVE_WEB_SEARCH", "off")
    requests: list[dict[str, Any]] = []

    async def create(payload: dict[str, Any]) -> dict[str, Any]:
        requests.append(payload)
        return say("Hi.")

    inlet = telegram.TelegramInlet(CFG, ev.Recorder(), create, toolset_route=None, grader=None)
    inlet._chat_id = 1
    asyncio.run(inlet._turn(telegram.Inbound(1, 1, "hi", message_id=5)))
    assert link_chat.APP_RULE not in requests[0]["instructions"]
    assert telegram._TEXT_STYLE in requests[0]["instructions"]
