"""Repos (eve_bridge/repos.py, docs/repos.md): questions about the owner's code, answered by a read-only, headless
Claude Code session in the repository, with follow-ups in the same thread. Claude Code is a stub here."""

from __future__ import annotations

import asyncio
import json
import os
import re
import sys
from pathlib import Path

import pytest

import eve_bridge.repos as app_mod
from eve_bridge import app_registry, tool_gate

# A stand-in for claude: logs its argv, its cwd and whether it got an API key, then answers per MODE.
STUB = """import json, os, sys, time
here = os.path.dirname(os.path.abspath(__file__))
mode = open(os.path.join(here, "MODE")).read().strip() if os.path.exists(os.path.join(here, "MODE")) else "ok"
with open(os.path.join(here, "calls.jsonl"), "a") as f:
    f.write(json.dumps({"argv": sys.argv[1:], "cwd": os.getcwd(), "key": os.environ.get("ANTHROPIC_API_KEY", ""),
                        "claudecode": os.environ.get("CLAUDECODE", "")}) + "\\n")
if mode == "slow":
    time.sleep(5)
if mode == "garbage":
    print("not json"); sys.exit(1)
if mode == "lost" and "--resume" in sys.argv:
    print(json.dumps({"is_error": True, "result": "No conversation found with session ID"})); sys.exit(1)
if mode == "error":
    print(json.dumps({"is_error": True, "result": "Not logged in"})); sys.exit(1)
size = 20000 if mode == "long" else 0
print(json.dumps({"is_error": False, "num_turns": 2, "result": "The answer." + ("\\nmore" * size)}))
"""


@pytest.fixture()
def stub(tmp_path: Path) -> Path:
    d = tmp_path / "bin"
    d.mkdir()
    p = d / "claude"
    p.write_text(f"#!{sys.executable}\n" + STUB)
    p.chmod(0o755)
    return p


@pytest.fixture()
def tree(tmp_path: Path) -> Path:
    root = tmp_path / "Documents"
    for rel in ("personal/eve", "personal/claude-debrief", "personal/portfolio", "work/portfolio",
                "personal/Spotify_Knob"):
        (root / rel / ".git").mkdir(parents=True)
    (root / "personal/eve-worktree").mkdir(parents=True)
    (root / "personal/eve-worktree/.git").write_text("gitdir: elsewhere\n")       # a worktree: not listed
    (root / "personal/site/node_modules/dep/.git").mkdir(parents=True)          # inside node_modules: not listed
    (root / "a/b/c/too-deep/.git").mkdir(parents=True)                         # below SCAN_DEPTH
    return root


def on(stub: Path, tree: Path, **extra: str) -> dict[str, str]:
    return {"EVE_REPOS": "1", "EVE_CLAUDE_BIN": str(stub), "EVE_REPOS_ROOTS": str(tree), **extra}


def hub_for(stub: Path, tree: Path, tmp_path: Path, clock=None, **extra: str) -> app_mod.Repos:
    hub = app_mod.make(on(stub, tree, **extra))
    assert hub is not None
    hub.state = tmp_path / "threads.json"
    hub._threads = {}
    if clock is not None:
        hub.clock = clock
    return hub


def calls(stub: Path) -> list[dict]:
    path = stub.parent / "calls.jsonl"
    return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []


def ask(hub: app_mod.Repos, repo: str, question: str = "how does it work?", new: object = None) -> dict:
    return asyncio.run(hub.handle("repos_ask", {"repo": repo, "question": question, "new_thread": new}))


def flag(argv: list[str], name: str) -> str:
    return argv[argv.index(name) + 1]


def test_off_by_default_and_on_with_its_switch(stub: Path, tree: Path) -> None:
    assert app_mod.make({}) is None
    assert app_mod.make({"EVE_REPOS": "0", "EVE_CLAUDE_BIN": str(stub)}) is None
    assert app_mod.make(on(stub, tree)) is not None


def test_its_tools_are_its_own_and_strict(stub: Path, tree: Path) -> None:
    hub = app_mod.make(on(stub, tree))
    names = tuple(t["name"] for t in hub.tools())
    assert names == app_mod.TOOL_NAMES == ("repos_ask", "repos_list")
    for tool in hub.tools():
        params = tool["parameters"]
        assert params.get("additionalProperties") is False and tool["strict"] is True
        assert sorted(params["required"]) == sorted(params["properties"])


def test_scan_finds_repositories_not_worktrees_or_dependencies(tree: Path) -> None:
    found = app_mod.scan([tree])
    assert sorted(found) == ["personal/Spotify_Knob", "personal/claude-debrief", "personal/eve",
                             "personal/portfolio", "work/portfolio"]


def test_a_name_resolves_by_path_folder_case_punctuation_or_a_unique_part(tree: Path) -> None:
    repos = app_mod.scan([tree])
    assert app_mod.resolve("eve", repos) == ("personal/eve", [])
    assert app_mod.resolve("work/portfolio", repos) == ("work/portfolio", [])
    assert app_mod.resolve("EVE", repos) == ("personal/eve", [])
    assert app_mod.resolve("spotify knob", repos) == ("personal/Spotify_Knob", [])
    assert app_mod.resolve("debrief", repos) == ("personal/claude-debrief", [])
    assert app_mod.resolve("portfolio", repos) == (None, ["personal/portfolio", "work/portfolio"])
    assert app_mod.resolve("nothing-like-it", repos) == (None, [])
    assert app_mod.resolve("", repos) == (None, [])


def test_a_question_runs_claude_read_only_in_the_repository(stub: Path, tree: Path, tmp_path: Path) -> None:
    hub = hub_for(stub, tree, tmp_path)
    out = ask(hub, "eve")
    assert out["ok"] and out["text"] == "The answer." and out["thread"] == "new" and out["repo"] == "personal/eve"
    call = calls(stub)[0]
    argv = call["argv"]
    assert Path(call["cwd"]).resolve() == (tree / "personal/eve").resolve()
    assert argv[:2] == ["-p", "how does it work?"] and flag(argv, "--output-format") == "json"
    assert flag(argv, "--tools") == "Read,Grep,Glob,Bash"
    assert flag(argv, "--permission-mode") == "dontAsk"
    assert flag(argv, "--setting-sources") == "project,local" and "--strict-mcp-config" in argv
    allowed = argv[argv.index("--allowedTools") + 1:argv.index("--disallowedTools")]
    assert {"Read", "Grep", "Glob"} <= set(allowed)
    bash = [a for a in allowed if a.startswith("Bash(")]
    assert bash and all(re.fullmatch(r"Bash\(git [a-z-]+( -{1,2}[a-z]+)?:\*\)", a) for a in bash)
    assert not any(w in " ".join(bash) for w in ("push", "commit", "checkout", "reset", "fetch", "pull", "branch:"))
    refused = argv[argv.index("--disallowedTools") + 1:argv.index("--permission-mode")]
    assert len(refused) == len(bash) and all("--output" in r for r in refused)
    assert not any(t in argv for t in ("Edit", "Write", "--dangerously-skip-permissions"))


def test_a_follow_up_continues_the_thread_and_a_new_topic_starts_one(stub: Path, tree: Path, tmp_path: Path) -> None:
    now = [1000.0]
    hub = hub_for(stub, tree, tmp_path, clock=lambda: now[0])
    first = ask(hub, "eve")
    second = ask(hub, "eve", "and where is that called?")
    assert (first["thread"], second["thread"]) == ("new", "continued") and second["question_in_thread"] == 2
    a, b = (c["argv"] for c in calls(stub))
    sid = flag(a, "--session-id")
    assert flag(b, "--resume") == sid and "--session-id" not in b
    third = ask(hub, "eve", "something else entirely", new=True)
    assert third["thread"] == "new" and flag(calls(stub)[2]["argv"], "--session-id") != sid
    now[0] += app_mod.FRESH_SECS + 1                                  # idle too long: a new thread
    assert ask(hub, "eve")["thread"] == "new"
    other = ask(hub, "claude-debrief")                                # each repository has its own thread
    assert other["thread"] == "new"
    assert ask(hub, "eve")["thread"] == "continued"


def test_threads_survive_a_restart(stub: Path, tree: Path, tmp_path: Path) -> None:
    hub = hub_for(stub, tree, tmp_path)
    ask(hub, "eve")
    assert oct(os.stat(hub.state).st_mode & 0o777) == "0o600"
    again = app_mod.make(on(stub, tree))
    again.state = hub.state
    again._threads = again._load()
    assert ask(again, "eve")["thread"] == "continued"


def test_a_lost_session_starts_the_thread_again(stub: Path, tree: Path, tmp_path: Path) -> None:
    hub = hub_for(stub, tree, tmp_path)
    ask(hub, "eve")
    (stub.parent / "MODE").write_text("lost")
    out = ask(hub, "eve", "follow up")
    assert out["ok"] and out["thread"] == "new"
    assert "--resume" in calls(stub)[1]["argv"] and "--session-id" in calls(stub)[2]["argv"]


def test_eves_own_key_is_not_lent_unless_one_is_named(stub: Path, tree: Path, tmp_path: Path,
                                                      monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("ANTHROPIC_API_KEY", "eve-model-key")
    monkeypatch.setenv("CLAUDECODE", "1")
    ask(hub_for(stub, tree, tmp_path), "eve")
    assert calls(stub)[-1]["key"] == "" and calls(stub)[-1]["claudecode"] == ""
    ask(hub_for(stub, tree, tmp_path, EVE_REPOS_API_KEY="repos-key"), "eve", new=True)
    assert calls(stub)[-1]["key"] == "repos-key"


def test_failures_are_reasons_never_answers(stub: Path, tree: Path, tmp_path: Path,
                                            monkeypatch: pytest.MonkeyPatch) -> None:
    hub = hub_for(stub, tree, tmp_path)
    for mode in ("error", "garbage"):
        (stub.parent / "MODE").write_text(mode)
        out = ask(hub, "eve")
        assert out["ok"] is False and out["reason"]
    assert "eve" not in json.loads(hub.state.read_text()) if hub.state.exists() else True
    (stub.parent / "MODE").write_text("slow")
    monkeypatch.setattr(app_mod, "ASK_SECS", 0.5)
    out = ask(hub, "eve")
    assert out["ok"] is False and "longer than" in out["reason"]
    assert ask(hub, "portfolio")["reason"].startswith("'portfolio' could be personal/portfolio, work/portfolio")
    assert "repos_list" in ask(hub, "nope")["reason"]
    assert asyncio.run(hub.handle("repos_ask", {"repo": "eve", "question": " ", "new_thread": None}))["ok"] is False
    assert asyncio.run(hub.handle("repos_explode", {}))["ok"] is False


def test_a_long_answer_is_cut(stub: Path, tree: Path, tmp_path: Path) -> None:
    (stub.parent / "MODE").write_text("long")
    out = ask(hub_for(stub, tree, tmp_path), "eve")
    assert len(out["text"]) <= app_mod.MAX_ANSWER_CHARS + 40 and out["text"].endswith("(The rest of the answer is cut.)")


def test_the_list_names_every_repository(stub: Path, tree: Path, tmp_path: Path) -> None:
    out = asyncio.run(hub_for(stub, tree, tmp_path).handle("repos_list", {}))
    assert out["ok"] and out["text"].startswith("5 repositories") and "work/portfolio" in out["text"]


def test_the_instructions_name_only_its_own_tools(stub: Path, tree: Path) -> None:
    named = set(re.findall(r"\brepos_[a-z_]+", app_mod.make(on(stub, tree)).instructions()))
    assert named == set(app_mod.TOOL_NAMES)


def test_the_phone_sheet_meets_the_contract(stub: Path, tree: Path, tmp_path: Path) -> None:
    hub = hub_for(stub, tree, tmp_path)
    first = asyncio.run(hub.panel({"action": "status"}))
    assert app_registry.check_panel(first) == [] and first["status"] == "On" and "5 repositories" in first["line"]
    assert asyncio.run(hub.panel({"action": "nonsense"}))["ok"] is False


def test_it_is_registered_as_reads_and_left_out_loud() -> None:
    app = app_registry.get("repos")
    assert app is not None and app_registry.check(app) == []
    assert app.switch == "EVE_REPOS" and not app.default_on and not app.acting
    assert app.policies() == {"repos_ask": "read", "repos_list": "read"}
    assert app.voice_omit == frozenset(app_mod.TOOL_NAMES)
    assert tool_gate.taints("repos_ask", {}) and not tool_gate.taints("repos_list", {})
