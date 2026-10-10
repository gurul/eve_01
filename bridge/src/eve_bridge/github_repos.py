"""The owner's own GitHub repositories, by name and what each is, so "my car repo" is found.

Owner, 2026-10-09 ("tune it", GitHub routing): two blind rounds of wording changes failed (19/26 and 17/26 against
a bar of 24). The misses were requests that name a project the brain had never heard of ("the recipe app", "the
mood tracker"): with no repo word in them it could not tell a GitHub repository from a Linear project, a note or an
app eve built, so it guessed. The brain is now told the owner's repositories themselves, and a repository's own
name in a message opens GitHub like any GitHub word.

* **Where from.** GitHub's own search through the direct app's MCP server (``search_repositories`` with
  ``user:<login>``, the login from ``get_me``): the owner's repositories, private ones included, as their
  fine-grained token sees them. No other request, no other key.
* **Kept.** ``~/.config/eve-bridge/github-repos.json`` (0600), refreshed once a day when the apps start
  (``REFRESH_SECS``). A failed refresh keeps the last list; none at all leaves the brain as it was.
* **Shown.** The ``SHOWN`` most recently pushed, each as ``name (what it is)``, the description clipped to
  ``DESC_CLIP`` characters, in one line of the apps block (``AppsBridge.instructions``). Every name in it is one
  GitHub returned: nothing is made up (the owner's rule for prompts).
* **Words.** ``names_repo``: a message holding a repository's name as a word (``zorbcar``, ``plantpal-x``) names GitHub.
  Names shorter than ``MIN_NAME``, or that are everyday words (``WORDS``, and any word in macOS's own
  ``/usr/share/dict/words``), are left out, so "remix" or "leap" in a sentence never opens GitHub, whose schemas are
  the largest of the apps; their line still teaches the brain.

The names and descriptions ride the brain's prompt, which goes to the model provider like the rest of the turn.
Logs carry counts only.
"""

from __future__ import annotations

import json
import logging
import re
import time
from pathlib import Path
from typing import Any, Callable, Mapping, Optional, Sequence

log = logging.getLogger(__name__)

REFRESH_SECS = 24 * 3600
FETCH_MAX = 100                  # GitHub's page size: the owner's repositories, most recently pushed first
SHOWN = 25                       # in the prompt
DESC_CLIP = 50
MIN_NAME = 4
# Repository names that are also everyday words: kept in the brain's line, never a keyword.
WORDS = frozenset({"atlas", "double", "candid", "lily", "eve", "echoes", "storeybox", "harbor", "notes", "site",
                   "website", "blog", "app", "bot", "test", "demo", "home", "config", "dotfiles", "resume", "delphi",
                   "handyman", "sushi"})
FILE = "github-repos.json"
DICTIONARY = Path("/usr/share/dict/words")
_dictionary: Optional[frozenset[str]] = None


def _english() -> frozenset[str]:
    """macOS's word list, lower-cased, read once; empty where there is none."""
    global _dictionary
    if _dictionary is None:
        try:
            _dictionary = frozenset(w.strip().lower() for w in DICTIONARY.read_text(encoding="utf-8").split())
        except OSError:
            _dictionary = frozenset()
    return _dictionary

Call = Callable[[str, Mapping[str, Any]], Mapping[str, Any]]


def _parse(result: Any) -> Any:
    if isinstance(result, str):
        try:
            return json.loads(result)
        except ValueError:
            return {}
    return result if result is not None else {}


def fetch(call: Call) -> Optional[list[dict[str, str]]]:
    """The owner's repositories through the GitHub app's tools (``call(name, args)`` → AppsBridge.execute's shape),
    most recently pushed first, as [{name, description}]; None when GitHub could not say."""
    me = call("github__get_me", {})
    login = _parse(me.get("result")).get("login") if me.get("ok") else None
    if not login or not re.fullmatch(r"[A-Za-z0-9-]{1,39}", str(login)):
        return None
    found = call("github__search_repositories", {"query": f"user:{login} fork:true sort:updated",
                                                 "perPage": FETCH_MAX})
    if not found.get("ok"):
        return None
    body = _parse(found.get("result"))
    items = body.get("items") or body.get("repositories") or [] if isinstance(body, dict) else []
    out = []
    for it in items:
        if not isinstance(it, dict):
            continue
        name = str(it.get("name") or str(it.get("full_name") or "").split("/")[-1]).strip()
        if name:
            out.append({"name": name, "description": " ".join(str(it.get("description") or "").split())})
    return out


def load(path: Path) -> tuple[list[dict[str, str]], float]:
    """(repositories, when fetched) from the kept file; ([], 0) when there is none or it cannot be read."""
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        repos = [r for r in data.get("repos") or [] if isinstance(r, dict) and r.get("name")]
        return [{"name": str(r["name"]), "description": str(r.get("description") or "")} for r in repos], \
            float(data.get("fetched") or 0)
    except (OSError, ValueError, AttributeError, TypeError):
        return [], 0.0


def refresh(path: Path, call: Call, clock: Callable[[], float] = time.time) -> list[dict[str, str]]:
    """The kept list, fetched again when it is older than REFRESH_SECS. A failed fetch keeps the old list."""
    from .apps_mcp import write_private_json

    repos, fetched = load(path)
    if repos and clock() - fetched < REFRESH_SECS:
        return repos
    try:
        fresh = fetch(call)
    except Exception as e:  # noqa: BLE001 — the type only
        log.warning("github repos: not refreshed (%s)", type(e).__name__)
        fresh = None
    if fresh is None:
        log.info("github repos: kept %d (refresh failed)", len(repos))
        return repos
    write_private_json(path, {"fetched": clock(), "repos": fresh})
    log.info("github repos: %d", len(fresh))
    return fresh


def line(repos: Sequence[Mapping[str, str]]) -> str:
    """The apps block's line: the owner's most recently pushed repositories, each with what it is; "" for none."""
    shown = []
    for r in list(repos)[:SHOWN]:
        desc = str(r.get("description") or "")
        desc = desc if len(desc) <= DESC_CLIP else desc[:DESC_CLIP - 1].rstrip() + "…"
        shown.append(f"{r['name']} ({desc})" if desc else str(r["name"]))
    if not shown:
        return ""
    return ("The owner's own GitHub repositories, most recently worked on first (a project they name loosely, "
            "\"my car repo\", \"the scam detector\", is one of these when it fits; read it with GitHub): "
            + "; ".join(shown) + ".")


def keyword_names(repos: Sequence[Mapping[str, str]]) -> frozenset[str]:
    """The repository names that open GitHub when a message holds one as a word."""
    out = set()
    for r in repos:
        name = str(r.get("name") or "").lower()
        if (len(name) >= MIN_NAME and name not in WORDS and name not in _english()
                and re.fullmatch(r"[a-z0-9][a-z0-9._-]*", name)):
            out.add(name)
    return frozenset(out)


def names_repo(text: str, names: frozenset[str]) -> bool:
    """Whether ``text`` holds one of ``names`` as a whole word (case ignored)."""
    if not names or not text:
        return False
    words = set(re.findall(r"[a-z0-9][a-z0-9._-]*", text.lower()))
    return bool(words & names)
