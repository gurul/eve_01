"""Repos: eve answers the owner's questions about the code in their own repositories on this Mac (docs/repos.md).

Owner, 2026-11-09: "allow me to ask direct questions about my repositories through eve ... like i should be able to
follow up and such". A question about code ("how does the debrief search rank hits", "where is the composer cleared",
"what changed in the car repo this week") needs the code read, not a summary of it. So eve hands the question to
Claude Code itself, run headless in the repository's folder, and lends it only tools that look.

**One thread per repository.** The first question about a repository starts a Claude Code session there
(``claude -p --session-id <new id>``); a follow-up resumes that same session (``--resume <id>``), so "and where is
that called?" is read with everything the session already found. A thread idle for ``FRESH_SECS`` ends, and
``new_thread`` ends one on purpose (a new topic). Threads are kept in ``~/.config/eve-bridge/repo-threads.json``, so a
daemon restart keeps them.

**Read-only.** The session gets Read, Grep and Glob, and Bash for a short list of git commands that only look
(``READ_GIT``), in ``dontAsk`` mode: anything else is refused, never asked. A git command's ``--output`` (which writes
a file) is refused too. No MCP server (``--strict-mcp-config``), and only the repository's own settings
(``--setting-sources project,local``), so the owner's hooks do not run for it (a debrief capture, a CI watcher).
It answers from the checkout as it is on the Mac: the branch it is on, with any uncommitted work. It never fetches.

**Which repositories.** Every git repository (a folder with a ``.git`` folder, so not a worktree) at most
``SCAN_DEPTH`` folders below a root in ``EVE_REPOS_ROOTS`` (``:``-separated; default ``DEFAULT_ROOTS``). A name is
matched by the folder's name, or by its path below the root (``work/portfolio``) when two share one.

**What leaves the Mac.** The question and the code the session reads go to Anthropic, as in any Claude Code session
(the owner's own Claude Code login; eve's ``ANTHROPIC_API_KEY`` is not lent to it, and ``EVE_REPOS_API_KEY`` gives
it a key of its own instead). The answer goes into the turn, so it reaches the chat model like any tool result.
Logs carry the repository's name, timings and counts, never the question or the answer.

**Out loud.** Not offered: an answer takes ten seconds to a minute, too long to wait in silence.

**The switch.** ``EVE_REPOS``: off unless 1/true/yes/on.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import os
import re
import shutil
import time
import uuid
from pathlib import Path
from typing import Any, Mapping, Optional

from . import app_registry, openrouter

log = logging.getLogger(__name__)

SWITCH = "EVE_REPOS"
ROOTS_VAR = "EVE_REPOS_ROOTS"
MODEL_VAR = "EVE_REPOS_MODEL"
BIN_VAR = "EVE_CLAUDE_BIN"
OFF_REASON = "Repos is off on this Mac (EVE_REPOS), or Claude Code is not installed."

DEFAULT_ROOTS = ("~/Documents", "~/Developer", "~/code", "~/src", "~/Projects")
DEFAULT_MODEL = "sonnet"
SCAN_DEPTH = 3                     # ~/Documents/personal/eve is two below ~/Documents
SCAN_TTL_SECS = 600.0
SKIP_DIRS = frozenset({"node_modules", ".venv", "venv", "target", "build", "dist", "Library", ".cache"})
FRESH_SECS = 2 * 3600.0            # a thread idle this long ends; the next question starts a new one
ASK_SECS = 180.0                   # a link turn's stream ends at 300 s (link_chat.TURN_SECS)
MAX_QUESTION_CHARS = 2000
MAX_ANSWER_CHARS = 8000
MAX_LISTED = 80
THREADS_FILE = "repo-threads.json"
# git commands that only look. Each is allowed as a prefix (Claude Code's "Bash(git log:*)"); --output is refused.
READ_GIT = ("git log", "git show", "git diff", "git status", "git blame", "git ls-files", "git shortlog",
            "git rev-parse", "git branch --list", "git branch -a", "git branch -v", "git tag --list",
            "git remote -v", "git describe")
INHERITED_ENV = ("CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_ENTRYPOINT",
                 "CLAUDE_CODE_EXECPATH", "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN",
                 "CLAUDE_CODE_SESSION_ATTENDED", "CLAUDE_PID")
# eve's own model key is not lent: Claude Code then uses the owner's own login (their plan), unless EVE_REPOS_API_KEY
# names a key for it.
API_KEY_ENV = (openrouter.ANTHROPIC_KEY_ENV, "ANTHROPIC_AUTH_TOKEN")
KEY_VAR = "EVE_REPOS_API_KEY"
SYSTEM = ("You answer the owner's question about this repository. The owner reads your answer in a phone chat, "
          "through their assistant. You can only look: never try to change a file, run a build or fetch. Answer "
          "first, in a few plain sentences or a short list; name the file (and function) where it helps, and quote "
          "at most a few lines of code. Answer from what you read; when you did not find it, say so. When the owner "
          "asks about recent work, read the git history.")

TOOLS: list[dict[str, Any]] = [
    {
        "type": "function", "name": "repos_ask", "strict": True,
        "description": (
            "Ask a question about the code in one of the owner's own repositories on this Mac: how something "
            "works, where something is, why it is written that way, what a function does, what changed in the code "
            "recently (it reads the git history). A coding assistant "
            "reads the repository (it only looks, never changes anything) and answers. The same repository's "
            "next question continues the same thread, so a follow-up can say 'that' or 'it'. Takes ten seconds "
            "to a minute."),
        "parameters": {
            "type": "object", "additionalProperties": False, "required": ["repo", "question", "new_thread"],
            "properties": {
                "repo": {"type": "string", "description": (
                    "The repository's folder name, as repos_list shows it or as the owner says it.")},
                "question": {"type": "string", "description": (
                    "The owner's question in their words. A follow-up can stay short: the thread remembers.")},
                "new_thread": {"type": ["boolean", "null"], "description": (
                    "true when the owner moves to a new topic in the same repository; null otherwise.")},
            },
        },
    },
    {
        "type": "function", "name": "repos_list", "strict": True,
        "description": "The owner's repositories on this Mac that repos_ask can read, by folder name.",
        "parameters": {"type": "object", "additionalProperties": False, "required": [], "properties": {}},
    },
]
TOOL_NAMES = tuple(t["name"] for t in TOOLS)

# What the model reads about this app. Name only this app's own tools (AGENTS.md rule 7).
INSTRUCTIONS = (
    "Repos: the code in the owner's own repositories on this Mac. A question about what the code does, how it "
    "works, where something is in it, why it is written that way, what a function or file does, or what changed in "
    "the code (its git history) goes to repos_ask, with the repository's name and the owner's question. This holds "
    "even when the repository is a project the owner's notes also know: the notes say what the owner did and "
    "decided, while repos_ask reads the code itself. A question about what the owner worked on, decided or left open "
    "is not about the code. A follow-up about the same repository goes to repos_ask again with the same repository: "
    "the thread remembers the earlier questions, so pass the follow-up as the owner said it. Set new_thread true only "
    "when the owner moves to a different topic. When you are not sure of the repository's name, call repos_list. "
    "Give the owner the answer in plain words; keep file names, drop long code.")


def enabled(environ: Optional[Mapping[str, str]] = None) -> bool:
    """Off unless EVE_REPOS is 1, true, yes or on."""
    env = os.environ if environ is None else environ
    return str(env.get(SWITCH, "")).strip().lower() in ("1", "true", "yes", "on")


def roots(environ: Optional[Mapping[str, str]] = None) -> list[Path]:
    env = os.environ if environ is None else environ
    raw = str(env.get(ROOTS_VAR) or "").strip()
    parts = [p for p in raw.split(":") if p.strip()] if raw else list(DEFAULT_ROOTS)
    return [Path(p.strip()).expanduser() for p in parts]


def claude_bin(environ: Optional[Mapping[str, str]] = None) -> Optional[str]:
    """Claude Code's binary: EVE_CLAUDE_BIN, else PATH, else its installer's place (a launchd PATH lacks it)."""
    env = os.environ if environ is None else environ
    for cand in (env.get(BIN_VAR), shutil.which("claude"), str(Path("~/.local/bin/claude").expanduser())):
        if cand and os.access(cand, os.X_OK):
            return cand
    return None


def scan(root_dirs: list[Path], depth: int = SCAN_DEPTH) -> dict[str, Path]:
    """Each repository found under the roots, keyed by its path below its root ("personal/eve")."""
    found: dict[str, Path] = {}

    def walk(root: Path, here: Path, left: int) -> None:
        try:
            entries = sorted(here.iterdir())
        except OSError:
            return
        for p in entries:
            if not p.is_dir() or p.name.startswith(".") or p.name in SKIP_DIRS or p.is_symlink():
                continue
            if (p / ".git").is_dir():
                found.setdefault(str(p.relative_to(root)), p)
            elif left > 1:
                walk(root, p, left - 1)

    for root in root_dirs:
        if root.is_dir():
            walk(root, root, depth)
    return found


def _norm(s: str) -> str:
    return re.sub(r"[^a-z0-9]", "", s.lower())


def resolve(name: str, repos: Mapping[str, Path]) -> tuple[Optional[str], list[str]]:
    """The repository a name means: (its key, []) or (None, the candidates). Tried in order: the path below the
    root, the folder's name, both with case and punctuation ignored, then a unique part of a folder's name."""
    want = name.strip().strip("/")
    if not want:
        return None, []
    if want in repos:
        return want, []
    for test in (lambda k: Path(k).name == want,
                 lambda k: Path(k).name.lower() == want.lower(),
                 lambda k: _norm(k) == _norm(want),
                 lambda k: _norm(Path(k).name) == _norm(want),
                 lambda k: len(_norm(want)) >= 3 and _norm(want) in _norm(Path(k).name)):
        hits = [k for k in repos if test(k)]
        if len(hits) == 1:
            return hits[0], []
        if hits:
            return None, sorted(hits)
    return None, []


def argv(binary: str, question: str, sid: str, resume: bool, model: str) -> list[str]:
    """The headless, read-only Claude Code call."""
    allowed = ["Read", "Grep", "Glob", *(f"Bash({c}:*)" for c in READ_GIT)]
    refused = [f"Bash({c} *--output*)" for c in READ_GIT]
    return [binary, "-p", question, "--output-format", "json", *(("--resume", sid) if resume else ("--session-id", sid)),
            "--model", model, "--tools", "Read,Grep,Glob,Bash", "--allowedTools", *allowed,
            "--disallowedTools", *refused, "--permission-mode", "dontAsk", "--setting-sources", "project,local",
            "--strict-mcp-config", "--append-system-prompt", SYSTEM]


class Repos:
    """The app's hub: the tools, the phone's panel, and close. One is made per daemon (make)."""

    def __init__(self, binary: str, root_dirs: list[Path], state: Path, model: str = DEFAULT_MODEL,
                 clock: Any = time.time, api_key: str = "") -> None:
        self.binary = binary
        self.api_key = api_key
        self.root_dirs = root_dirs
        self.state = state
        self.model = model
        self.clock = clock
        self._repos: dict[str, Path] = {}
        self._scanned = -1e18
        self._locks: dict[str, asyncio.Lock] = {}
        self._threads: dict[str, dict[str, Any]] = self._load()

    def tools(self) -> list[dict[str, Any]]:
        return [dict(t) for t in TOOLS]

    def instructions(self) -> str:
        return INSTRUCTIONS

    def _load(self) -> dict[str, dict[str, Any]]:
        try:
            data = json.loads(self.state.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return {}
        return {k: v for k, v in data.items() if isinstance(v, dict) and isinstance(v.get("sid"), str)} \
            if isinstance(data, dict) else {}

    def _save(self) -> None:
        with contextlib.suppress(OSError):
            self.state.parent.mkdir(parents=True, exist_ok=True)
            tmp = self.state.with_suffix(".tmp")
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                json.dump(self._threads, f)
            os.replace(tmp, self.state)

    async def repos(self) -> dict[str, Path]:
        if self.clock() - self._scanned > SCAN_TTL_SECS:
            self._repos = await asyncio.to_thread(scan, self.root_dirs)
            self._scanned = self.clock()
        return self._repos

    async def list(self) -> dict[str, Any]:
        repos = await self.repos()
        if not repos:
            return {"ok": True, "text": "No repositories found on this Mac."}
        def pushed(k: str) -> float:
            try:
                return (repos[k] / ".git").stat().st_mtime
            except OSError:
                return 0.0
        keys = sorted(repos, key=pushed, reverse=True)
        shown = keys[:MAX_LISTED]
        more = f" ({len(keys) - len(shown)} more not shown)" if len(keys) > len(shown) else ""
        return {"ok": True, "text": f"{len(keys)} repositories, most recently used first{more}: " + ", ".join(shown)}

    async def ask(self, repo: str, question: str, new_thread: bool) -> dict[str, Any]:
        repos = await self.repos()
        key, candidates = resolve(repo, repos)
        if key is None and not candidates and self._scanned > -1e18:
            self._scanned = -1e18                     # a repository made since the last scan
            repos = await self.repos()
            key, candidates = resolve(repo, repos)
        if key is None:
            if candidates:
                return {"ok": False, "reason": f"'{repo}' could be {', '.join(candidates[:8])}: ask the owner which."}
            return {"ok": False, "reason": f"No repository named '{repo}' on this Mac; repos_list shows them."}
        lock = self._locks.setdefault(key, asyncio.Lock())
        async with lock:
            thread = self._threads.get(key)
            resume = bool(thread) and not new_thread and self.clock() - float(thread.get("used", 0)) < FRESH_SECS
            sid = thread["sid"] if resume else str(uuid.uuid4())
            out = await self._run(repos[key], argv(self.binary, question, sid, resume, self.model))
            if not out.get("ok") and resume and out.get("lost"):
                sid, resume = str(uuid.uuid4()), False    # the session's transcript is gone: start the thread again
                out = await self._run(repos[key], argv(self.binary, question, sid, False, self.model))
            if not out.get("ok"):
                return {"ok": False, "reason": out["reason"]}
            self._threads[key] = {"sid": sid, "used": self.clock(), "asked": int((thread or {}).get("asked", 0))
                                  * resume + 1}
            self._save()
        answer = out["text"]
        if len(answer) > MAX_ANSWER_CHARS:
            answer = answer[:MAX_ANSWER_CHARS].rsplit("\n", 1)[0] + "\n(The rest of the answer is cut.)"
        turn = self._threads[key]["asked"]
        return {"ok": True, "repo": key, "thread": "continued" if resume else "new", "question_in_thread": turn,
                "text": answer}

    async def _run(self, folder: Path, cmd: list[str]) -> dict[str, Any]:
        env = {k: v for k, v in os.environ.items() if k not in INHERITED_ENV and k not in API_KEY_ENV}
        if self.api_key:
            env[openrouter.ANTHROPIC_KEY_ENV] = self.api_key
        started = time.monotonic()
        try:
            proc = await asyncio.create_subprocess_exec(
                *cmd, cwd=str(folder), env=env, stdin=asyncio.subprocess.DEVNULL,
                stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        except OSError as e:
            log.warning("repos: could not start Claude Code (%s)", type(e).__name__)
            return {"ok": False, "reason": "Claude Code could not start on this Mac."}
        try:
            stdout, stderr = await asyncio.wait_for(proc.communicate(), ASK_SECS)
        except asyncio.TimeoutError:
            with contextlib.suppress(ProcessLookupError):
                proc.kill()
            with contextlib.suppress(Exception):
                await proc.wait()
            log.warning("repos: %s timed out after %.0f s", folder.name, ASK_SECS)
            return {"ok": False, "reason": f"The code question took longer than {int(ASK_SECS)} seconds; ask "
                                           "something narrower."}
        secs = time.monotonic() - started
        try:
            data = json.loads(stdout.decode("utf-8", "replace"))
        except ValueError:
            data = None
        if not isinstance(data, dict):
            err = stderr.decode("utf-8", "replace")
            lost = "No conversation found" in err or "session" in err.lower() and "not found" in err.lower()
            log.warning("repos: %s answered no result (exit %s, %.1f s)", folder.name, proc.returncode, secs)
            return {"ok": False, "lost": lost, "reason": "Claude Code answered nothing it could use; is it signed in "
                                                         "on this Mac?"}
        result = str(data.get("result") or "").strip()
        log.info("repos: %s answered in %.1f s, %s turns, %d chars%s", folder.name, secs, data.get("num_turns"),
                 len(result), " (error)" if data.get("is_error") else "")
        if data.get("is_error") or not result:
            return {"ok": False, "lost": "No conversation found" in result,
                    "reason": "Claude Code could not answer: " + (result[:300] or str(data.get("subtype") or ""))}
        return {"ok": True, "text": result}

    async def handle(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        """One tool call from the chat."""
        if not isinstance(args, Mapping):
            return {"ok": False, "reason": "the arguments must be an object"}
        if name == "repos_list":
            return await self.list()
        if name == "repos_ask":
            repo = " ".join(str(args.get("repo") or "").split())[:200]
            question = str(args.get("question") or "").strip()[:MAX_QUESTION_CHARS]
            if not repo:
                return {"ok": False, "reason": "say which repository"}
            if not question:
                return {"ok": False, "reason": "say what to ask"}
            return await self.ask(repo, question, args.get("new_thread") is True)
        return {"ok": False, "reason": f"unknown tool {name}"}

    async def panel(self, body: dict[str, Any]) -> dict[str, Any]:
        """The phone's generic sheet (POST /api/link/controls/repos): a status line, no controls."""
        if str(body.get("action") or "status") != "status":
            return app_registry.panel(ok=False, line="Repos only answers questions in the chat.")
        n = len(await self.repos())
        return app_registry.panel(status="On", line=f"{n} repositories. Ask the chat about their code.", controls=[])

    async def close(self) -> None:
        """The daemon stops: a question still running is killed with its turn."""


def make(environ: Optional[Mapping[str, str]] = None) -> Optional[Repos]:
    """The app, or None when EVE_REPOS is off or Claude Code is not installed."""
    if not enabled(environ):
        log.info("repos: off (EVE_REPOS)")
        return None
    binary = claude_bin(environ)
    if binary is None:
        log.warning("repos: on, but Claude Code (claude) is not installed")
        return None
    env = os.environ if environ is None else environ
    model = str(env.get(MODEL_VAR) or DEFAULT_MODEL).strip() or DEFAULT_MODEL
    return Repos(binary, roots(environ), Path("~/.config/eve-bridge").expanduser() / THREADS_FILE, model,
                 api_key=str(env.get(KEY_VAR) or "").strip())
