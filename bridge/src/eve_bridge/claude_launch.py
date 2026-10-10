"""Start a Claude Code session on the Mac from Telegram, by walking a short decision tree.

Every coding folder lives under ``~/Documents``, split into ``personal`` and ``work``. From the phone,
"new claude" asks two questions and then opens a terminal window on the Mac running ``claude`` in the
folder picked:

1. **Personal or work?** (``EVE_CODE_AREAS``, default ``personal,work``, under
   ``EVE_CODE_ROOT``, default ``~/Documents``). A recent session's number, or a folder name, skips
   ahead: the owner usually has one in mind.
2. **General, or which folder?** "general" opens the area itself; a name opens that folder; "list"
   shows what is there. A name is matched forgivingly (case, dashes and spaces ignored, then prefix,
   then substring, then a close spelling); more than one match becomes a numbered choice.

Which program starts is never a question: personal opens ``claude``, work opens ``era-code claude``
(``AREA_HARNESS``), both with ``--dangerously-skip-permissions``. Words that name the other one
explicitly — "in claude instead of era code" — switch it for that session (``split_harness``). The text
brain reaches the same tree through its ``start_coding_session`` tool, so "open era maker in work" works
without the code word.

The words are code, not a model call, as "codex on" is. ``LaunchFlow`` is pure — it reads folder names
and never starts anything — so the tests drive it with a temporary tree. ``open_session`` is the only
part that touches the Mac: a Warp launch configuration (the terminal on this desk) or, with
``EVE_CLAUDE_TERMINAL=terminal``, a Terminal.app window. A folder path is only ever one that was
listed from the tree, never text from the chat, and it is shell-quoted either way.
"""

from __future__ import annotations

import asyncio
import difflib
import json
import logging
import os
import re
import shlex
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Optional, Sequence

log = logging.getLogger(__name__)

DEFAULT_AREAS = ("personal", "work")
MAX_CHOICES = 10                     # a numbered choice longer than this is a list, not a question
MAX_RECENT = 5
FLOW_TIMEOUT_SECS = 300.0            # an unanswered question this old is dropped; the next text is eve's
WARP_CONFIG_NAME = "eve-claude"
RECENT_PATH = Path.home() / ".config" / "eve-bridge" / "claude-launch.json"

# "/new_claude" is the / menu's form (telegram.BOT_COMMANDS): a Bot API command has underscores, not spaces.
TRIGGER = re.compile(r"^/?(?:new[ _]?claude(?:[ _]session)?|claude[ _]?new|start[ _]claude|claude[ _]session)"
                     r"(?:\s+(.*))?$", re.I | re.S)
GENERAL_WORDS = ("general", "whole", "root", "all", "g", "the whole thing", "top")
LIST_WORDS = ("list", "ls", "?", "show", "folders", "list them", "show me", "which", "options")
_NUMBERED = re.compile(r"^(.+)-\d+$")    # era-maker-213: a worktree of era-maker, folded out of the list
_TAPPED = re.compile(r"^(\d+)\.\s")     # "2. eve (personal)": a tapped button carries its number


def code_root(environ: Any = None) -> Path:
    env = os.environ if environ is None else environ
    raw = str(env.get("EVE_CODE_ROOT", "")).strip()
    return Path(raw).expanduser() if raw else Path.home() / "Documents"


def area_names(environ: Any = None) -> tuple[str, ...]:
    env = os.environ if environ is None else environ
    raw = [p.strip() for p in str(env.get("EVE_CODE_AREAS", "")).split(",") if p.strip()]
    return tuple(raw) or DEFAULT_AREAS


def folders(area: Path) -> list[str]:
    """The folders in an area, by name. Hidden folders and files are not projects."""
    try:
        return sorted((p.name for p in area.iterdir() if p.is_dir() and not p.name.startswith(".")),
                      key=str.lower)
    except OSError:
        return []


def _norm(text: str) -> str:
    return re.sub(r"[\s_\-.]+", "", text.lower())


def match(query: str, names: Sequence[str]) -> list[str]:
    """The folders a typed name means, best first. One exact (normalised) hit wins outright."""
    q = _norm(query)
    if not q:
        return []
    exact = [n for n in names if _norm(n) == q]
    if exact:
        return exact[:1]
    by_len = lambda n: (len(n), n.lower())    # noqa: E731 — era-maker before era-maker-213
    prefix = sorted((n for n in names if _norm(n).startswith(q)), key=by_len)
    if prefix:
        return prefix
    inside = sorted((n for n in names if q in _norm(n)), key=by_len)
    if inside:
        return inside
    normed = {_norm(n): n for n in names}
    return [normed[c] for c in difflib.get_close_matches(q, list(normed), n=5, cutoff=0.7)]


def menu(names: Sequence[str]) -> list[tuple[str, int]]:
    """The list as the phone shows it: ``(name, hidden numbered copies)``. ``era-maker-213`` and its
    twenty siblings fold into ``era-maker`` when ``era-maker`` itself is there; they stay reachable by
    their full name."""
    present = set(names)
    folded: dict[str, int] = {}
    shown: list[str] = []
    for name in names:
        m = _NUMBERED.match(name)
        if m and m.group(1) in present:
            folded[m.group(1)] = folded.get(m.group(1), 0) + 1
        else:
            shown.append(name)
    return [(n, folded.get(n, 0)) for n in shown]




# ---- which harness --------------------------------------------------------------------------------

# The two ways the owner starts a session, and which area uses which (owner, 2026-09-23): personal is
# plain Claude Code, work is Era's. Never asked; only an explicit "in claude instead" changes it. Both
# skip Claude Code's permission prompts: the phone cannot answer them.
HARNESSES: dict[str, str] = {
    "claude": "claude --dangerously-skip-permissions",
    "era-code": "era-code claude --dangerously-skip-permissions",
}
AREA_HARNESS: dict[str, str] = {"personal": "claude", "work": "era-code"}
_H = r"(era[- ]?code|claude(?: code)?)"
# Said explicitly, anywhere in the words: "instead of era code" means the other one; "in claude",
# "with era-code", "claude instead" name it. The phrase is removed before the folder is matched.
_INSTEAD_OF = re.compile(rf"[,\s]*\b(?:but\s+)?(?:(?:i\s+)?want\s+it\s+)?(?:in\s+\S+\s+)?instead\s+of\s+{_H}\b", re.I)
_NAMED = re.compile(rf"[,\s]*\b(?:but\s+)?(?:(?:i\s+)?want\s+it\s+)?(?:in|with|using|use|via|on|as)\s+{_H}"
                    rf"(?:\s+instead)?\b", re.I)
_BARE_INSTEAD = re.compile(rf"[,\s]*\b{_H}\s+instead\b", re.I)


def harness_name(word: str) -> str:
    return "era-code" if word.lower().startswith("era") else "claude"


def split_harness(text: str) -> tuple[str, Optional[str]]:
    """(the words without the harness phrase, the harness the owner asked for or None)."""
    m = _INSTEAD_OF.search(text)
    if m:
        other = "claude" if harness_name(m.group(1)) == "era-code" else "era-code"
        return (text[:m.start()] + text[m.end():]).strip(" ,."), other
    for pattern in (_NAMED, _BARE_INSTEAD):
        m = pattern.search(text)
        if m:
            return (text[:m.start()] + text[m.end():]).strip(" ,."), harness_name(m.group(1))
    return text, None


def default_harness(folder: Path, root: Path) -> str:
    try:
        area = folder.relative_to(root).parts[0]
    except (ValueError, IndexError):
        return "claude"
    return AREA_HARNESS.get(area.lower(), "claude")


# ---- recent sessions ----------------------------------------------------------------------------

@dataclass(frozen=True)
class Recent:
    folder: Path
    harness: str


def load_recent(path: Path = RECENT_PATH) -> list[Recent]:
    try:
        data = json.loads(path.read_text())
    except (OSError, ValueError):
        return []
    rows = data.get("recent", []) if isinstance(data, dict) else []
    out = [Recent(Path(r["folder"]), r["harness"]) for r in rows
           if isinstance(r, dict) and isinstance(r.get("folder"), str) and r.get("harness") in HARNESSES]
    return out[:MAX_RECENT]


def remember(folder: Path, harness: str, path: Path = RECENT_PATH) -> None:
    rows = [Recent(folder, harness)] + [r for r in load_recent(path) if r.folder != folder]
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"recent": [{"folder": str(r.folder), "harness": r.harness}
                                               for r in rows[:MAX_RECENT]]}, indent=2) + "\n")
    except OSError as e:
        log.warning("claude-launch: could not save recent folders (%s)", type(e).__name__)


# ---- the decision tree --------------------------------------------------------------------------

@dataclass
class Step:
    """What the flow says next. ``folder`` and ``harness`` are set when it has decided: start there."""
    text: str
    folder: Optional[Path] = None
    harness: Optional[str] = None
    done: bool = False
    title: str = "New Claude session"
    buttons: list[str] = field(default_factory=list)   # tap targets on the phone; each sends its own text


@dataclass
class LaunchFlow:
    root: Path
    areas: tuple[str, ...] = DEFAULT_AREAS
    recent: list[Recent] = field(default_factory=list)
    clock: Callable[[], float] = time.monotonic
    area: Optional[str] = None
    harness: Optional[str] = None                        # said explicitly; otherwise the area decides
    choices: list[Any] = field(default_factory=list)     # what a bare number means right now
    asked_at: float = 0.0

    def expired(self) -> bool:
        return self.clock() - self.asked_at > FLOW_TIMEOUT_SECS

    def _existing_areas(self) -> list[str]:
        return [a for a in self.areas if (self.root / a).is_dir()]

    def _label(self, folder: Path) -> str:
        try:
            rel = folder.relative_to(self.root)
        except ValueError:
            return str(folder)
        parts = rel.parts
        return f"{parts[-1]} ({parts[0]})" if len(parts) > 1 else f"{parts[0]} (general)"

    def start(self, argument: str = "") -> Step:
        """The first question, or straight on when "new claude <words>" already says some of it."""
        self.asked_at = self.clock()
        return self.answer(argument) if argument.strip() else self._ask_area()

    def request(self, area: str = "", folder: str = "", harness: str = "") -> Step:
        """The text brain's way in (``start_coding_session``): what it understood, each part optional.
        Whatever is missing becomes the next question, exactly as with the code word."""
        self.asked_at = self.clock()
        if harness in HARNESSES:
            self.harness = harness
        areas = self._existing_areas()
        chosen = next((a for a in areas if a.lower() == area.strip().lower()), None)
        folder = folder.strip()
        if chosen is None:
            return self.answer(folder) if folder else self._ask_area()
        return self._scope_answer(chosen, folder) if folder else self._ask_scope(chosen)

    def _ask_area(self) -> Step:
        areas = self._existing_areas()
        if not areas:
            return Step(f"I can't find {', '.join(self.areas)} under {self.root}.", done=True)
        self.area, self.choices = None, []
        text = " or ".join(a.capitalize() for a in areas) + "?"
        buttons = [a.capitalize() for a in areas]
        recent = [r for r in self.recent if r.folder.is_dir()]
        if recent:
            self.choices = recent
            numbered = [f"{i}. {self._label(r.folder)}" for i, r in enumerate(recent, 1)]
            text += "\n\nOr a recent one:\n" + "\n".join(numbered)
            buttons += numbered
        text += "\n\nA folder name works too. cancel stops."
        return Step(text, buttons=buttons + ["Cancel"])

    def _ask_scope(self, area: str) -> Step:
        self.area, self.choices = area, []
        return Step(f"{area.capitalize()}: general, or which folder? Say list to see them.",
                    buttons=["List", "General", "Cancel"])

    def answer(self, text: str) -> Step:
        self.asked_at = self.clock()
        said, harness = split_harness(text.strip().rstrip(".!"))
        if harness is not None:
            self.harness = harness
        tapped = _TAPPED.match(said)
        if tapped and self.choices:
            said = tapped.group(1)
        word = said.lower()
        if word.isdigit() and self.choices:
            n = int(word)
            if not 1 <= n <= len(self.choices):
                return Step(f"Pick a number from 1 to {len(self.choices)}.")
            chosen = self.choices[n - 1]
            return self._open(chosen.folder if isinstance(chosen, Recent) else chosen)
        if not said:
            return self._ask_area() if self.area is None else self._ask_scope(self.area)
        areas = self._existing_areas()
        if self.area is None:
            # "work", "w", "work era-hub-api", "personal general", or just a folder name.
            head, _, rest = said.partition(" ")
            area = next((a for a in areas if a.lower() == head.lower()
                         or (len(head) == 1 and a.lower().startswith(head.lower()))), None)
            if area is not None:
                return self._scope_answer(area, rest) if rest.strip() else self._ask_scope(area)
            if word in LIST_WORDS:
                return self._ask_area()
            return self._pick(said, areas)
        return self._scope_answer(self.area, said)

    def _scope_answer(self, area: str, said: str) -> Step:
        self.area = area
        word = said.strip().lower().rstrip(".!")
        if word in GENERAL_WORDS:
            return self._open(self.root / area)
        if word in LIST_WORDS:
            return self._list(area)
        return self._pick(said, [area])

    def _list(self, area: str) -> Step:
        names = folders(self.root / area)
        if not names:
            return Step(f"{area.capitalize()} has no folders. Say general to open it.")
        shown = menu(names)
        self.choices = [self.root / area / n for n, _ in shown]
        lines = [f"{i}. {n}" + (f" (+{k} numbered)" if k else "") for i, (n, k) in enumerate(shown, 1)]
        return Step("\n".join(lines) + "\n\nTap one, or reply a number or a name, or general.",
                    title=f"{area.capitalize()} folders",
                    buttons=[f"{i}. {n}" for i, (n, _) in enumerate(shown, 1)] + ["General", "Cancel"])

    def _pick(self, said: str, areas: Sequence[str]) -> Step:
        hits = [self.root / a / n for a in areas for n in match(said, folders(self.root / a))]
        if len(hits) == 1:
            return self._open(hits[0])
        where = areas[0] if len(areas) == 1 else " or ".join(areas)
        if not hits:
            hint = " Say list to see them." if len(areas) == 1 else ""
            return Step(f"No folder like \"{said}\" in {where}.{hint}")
        self.choices = hits[:MAX_CHOICES]
        more = f"\n…and {len(hits) - MAX_CHOICES} more; type more of the name." if len(hits) > MAX_CHOICES else ""
        numbered = [f"{i}. {self._label(p)}" for i, p in enumerate(self.choices, 1)]
        return Step("Which one?\n" + "\n".join(numbered) + more, buttons=numbered + ["Cancel"])

    def _open(self, folder: Path) -> Step:
        self.choices = []
        harness = self.harness or default_harness(folder, self.root)
        return Step(f"Opening {self._label(folder)} with {HARNESSES[harness]}.", folder=folder,
                    harness=harness, done=True)


# ---- opening the terminal -----------------------------------------------------------------------

def terminal_app(environ: Any = None) -> str:
    env = os.environ if environ is None else environ
    raw = str(env.get("EVE_CLAUDE_TERMINAL", "")).strip().lower()
    if raw in ("warp", "terminal"):
        return raw
    return "warp" if Path("/Applications/Warp.app").exists() else "terminal"


def warp_config(folder: Path, command: str) -> str:
    """A Warp launch configuration: one window, one tab, ``command`` in ``folder``. Warp finds a
    configuration by its ``name`` (warp://launch/<name>; a file path is not matched, tried 2026-09-23)
    and rereads the file on each launch, so one fixed name is rewritten every time. JSON strings are
    valid YAML scalars, so a folder name with a colon or a quote cannot break the file."""
    return ("---\n"
            f"name: {json.dumps(WARP_CONFIG_NAME)}\n"
            "windows:\n"
            "  - tabs:\n"
            f"      - title: {json.dumps(folder.name)}\n"
            "        layout:\n"
            f"          cwd: {json.dumps(str(folder))}\n"
            "          commands:\n"
            f"            - exec: {json.dumps(command)}\n")


async def _run(*argv: str) -> tuple[int, str]:
    proc = await asyncio.create_subprocess_exec(*argv, stdout=asyncio.subprocess.PIPE,
                                                stderr=asyncio.subprocess.PIPE)
    out, err = await proc.communicate()
    return proc.returncode or 0, (err or out).decode(errors="replace").strip()[:200]


async def open_session(folder: Path, harness: str = "claude", *, command: Optional[str] = None,
                       app: Optional[str] = None, warp_dir: Optional[Path] = None,
                       recent_path: Path = RECENT_PATH, run: Callable[..., Any] = _run) -> str:
    """Open a terminal window on the Mac running the harness in ``folder``. Returns the line for the chat.
    ``command`` overrides the harness's command line (the live check uses an echo)."""
    folder = Path(folder).resolve()
    if not folder.is_dir():
        return f"{folder} is not a folder any more."
    command = command or HARNESSES[harness]
    app = app or terminal_app()
    if app == "warp":
        config_dir = warp_dir or Path.home() / ".warp" / "launch_configurations"
        try:
            config_dir.mkdir(parents=True, exist_ok=True)
            (config_dir / f"{WARP_CONFIG_NAME}.yaml").write_text(warp_config(folder, command))
        except OSError as e:
            return f"I couldn't write the Warp launch configuration ({type(e).__name__})."
        code, err = await run("open", f"warp://launch/{WARP_CONFIG_NAME}")
    else:
        line = f"cd {shlex.quote(str(folder))} && {command}"
        escaped = line.replace("\\", "\\\\").replace('"', '\\"')
        code, err = await run("osascript", "-e", f'tell application "Terminal" to do script "{escaped}"',
                              "-e", 'tell application "Terminal" to activate')
    if code != 0:
        log.warning("claude-launch: %s did not open (%s)", app, err)
        return f"I couldn't open {'Warp' if app == 'warp' else 'Terminal'}: {err or 'no reason given'}"
    remember(folder, harness, recent_path)
    return f"{harness} is starting in {folder.name} on the Mac. Say claude on to drive it from here."
