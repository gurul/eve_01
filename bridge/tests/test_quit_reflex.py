"""Quitting by the word "quit" (owner, 2026-09-23): the rules, Jev behind them, and what runs."""

from __future__ import annotations

import asyncio
from typing import Any

import pytest

from eve_bridge import app_reflex
from eve_bridge import task_router as tr
from eve_bridge import typed_ask as ta
from eve_bridge.app_reflex import ReflexFirstAgent

APPS = ["Spotify", "Safari", "Google Chrome", "System Settings", "Notes", "Warp", "TV", "Messages", "Slack"]


@pytest.mark.parametrize("goal,kind,app", [
    ("Quit Spotify.", "quit", "Spotify"),
    ("can you quit spotify for me", "quit", "Spotify"),
    ("go ahead and quit out of chrome", "quit", "Google Chrome"),
    ("quit system preferences", "quit", "System Settings"),
    ("Quit Apple TV please", "quit", "TV"),
    ("please quit all", "quit_all", ""),
    ("quit everything", "quit_all", ""),
    ("quit all my open apps", "quit_all", ""),
])
def test_rules_take_a_bare_quit(goal: str, kind: str, app: str) -> None:
    plan = tr.classify(goal, apps=APPS)
    assert (plan.kind, plan.app, plan.complete) == (kind, app, True)


@pytest.mark.parametrize("goal", [
    "force quit Spotify", "quit spotify and open notes", "close Safari", "exit Slack", "kill Spotify",
    "should I quit Slack?", "is Spotify still running?", "quit all the Safari tabs", "I quit!", "quit Photoshop",
    "quit Notes and Slack",
])
def test_everything_else_is_never_a_quit(goal: str) -> None:
    plan = tr.classify(goal, apps=APPS)
    assert plan.kind not in ("quit", "quit_all") and not plan.complete


def test_jev_is_asked_only_behind_the_quit_word_gate() -> None:
    asked: list[str] = []

    def model(goal: str, apps: list[str]) -> str:
        asked.append(goal)
        return "Spotify"

    assert tr.classify("Spotify, quit it.", apps=APPS, quit_model=model).app == "Spotify"
    for goal in ("close Spotify", "force quit Spotify", "is Spotify quitting?", "Quit Spotify."):
        tr.classify(goal, apps=APPS, quit_model=model)
    assert asked == ["Spotify, quit it."]                  # the rules took "Quit Spotify."; the rest never reach it
    assert tr.classify("Photoshop, quit it", apps=APPS, quit_model=lambda g, a: "Photoshop").kind == "astra"
    assert tr.classify("everything, quit it", apps=APPS, quit_model=lambda g, a: "*").kind == "quit_all"

    def boom(goal: str, apps: list[str]) -> str:
        raise TimeoutError

    assert tr.classify("Spotify, quit it.", apps=APPS, quit_model=boom).kind == "astra"


def test_decide_quit_and_the_fit_allow_no_wrong_quit() -> None:
    g = ta.QuitGates(0.5, 0.6, 0.95)
    assert ta.decide_quit(ta.QuitAnswer(0.9, 0.1, "Slack", 1.0), g) == "Slack"
    assert ta.decide_quit(ta.QuitAnswer(0.9, 0.1, "Slack", 0.9), g) == ""
    assert ta.decide_quit(ta.QuitAnswer(0.1, 0.9, "", 0.9), g) == "*"
    assert ta.decide_quit(ta.QuitAnswer(0.9, 0.9, "Slack", 1.0, error="x"), g) == ""
    answers = [ta.QuitAnswer(0.97, 0.05, "Music", 1.0), ta.QuitAnswer(0.72, 0.05, "Spotify", 1.0)]
    fitted = ta.fit_quit_gates(answers, ["Music", ""])
    assert ta.decide_quit(answers[0], fitted) == "Music" and ta.decide_quit(answers[1], fitted) == ""


def test_jev_quit_request_is_absolute_nouls_and_the_installed_list() -> None:
    q = ta.jev_quit_questions(["Spotify", "Safari"])
    assert q["quit_one"]["type"] == q["quit_all"]["type"] == "noul"
    assert q["app"]["criteria"] == {"app0": "Spotify", "app1": "Safari", "none": "no installed application is to be quit"}


class Inner:
    running = False

    def __init__(self) -> None:
        self.goals: list[str] = []

    async def run(self, goal: str) -> str:
        self.goals.append(goal)
        return "codex"


def test_the_wrapper_quits_without_codex() -> None:
    quit: list[str] = []
    inner = Inner()

    async def quitter(app: str) -> str:
        quit.append(app)
        return f"Quit {app}."

    async def everything() -> str:
        quit.append("*")
        return "Quit 3 apps."

    agent = ReflexFirstAgent(lambda: inner, lambda ev: None, apps=lambda: APPS, quitter=quitter,
                             quit_everything=everything)
    assert asyncio.run(agent.run("quit slack")) == "Quit Slack."
    assert asyncio.run(agent.run("quit all")) == "Quit 3 apps."
    assert asyncio.run(agent.run("close slack")) == "codex"
    assert quit == ["Slack", "*"] and inner.goals == ["close slack"]


def test_quit_all_keeps_eves_own_and_the_terminals(monkeypatch: Any) -> None:
    calls: list[str] = []

    async def osa(*lines: str) -> tuple[bool, str]:
        calls.append(" ".join(lines))
        if "every application process" in lines[0]:
            return True, "Finder, ChatGPT, stable, Claude, Spotify, Slack, StackChanNotes, Preview"
        return True, ""

    async def running(name: str) -> bool:
        return name == "Preview"                               # Preview asks to save

    monkeypatch.setattr(app_reflex, "_osascript", osa)
    monkeypatch.setattr(app_reflex, "_running", running)
    said = asyncio.run(app_reflex.quit_all(app_reflex.quit_keep({"EVE_QUIT_ALL_KEEP": "Slack"}), settle=0))
    quits = [c for c in calls if " to quit" in c]
    assert len(quits) == 2 and any('"Spotify"' in c for c in quits) and any('"Preview"' in c for c in quits)
    assert said.startswith("Quit 1 app: Spotify.") and "Preview didn't quit" in said
    assert "Kept ChatGPT, Warp, Claude, Slack, StackChanNotes." in said
