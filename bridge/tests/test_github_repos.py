"""github_repos.py: the owner's repositories fetched through the GitHub app's tools, kept a day, shown to the brain
in one line, and their unusual names opening GitHub. Every name here is made up."""

from __future__ import annotations

import json
import stat
from pathlib import Path
from typing import Any, Mapping

from eve_bridge import apps_mcp
from eve_bridge import github_repos as G

REPOS = [{"name": "zorbcar", "description": "autonomous car from a kit, step by step to self-driving and more words"},
         {"name": "remix", "description": "a music thing"}, {"name": "qbt", "description": ""},
         {"name": "plantpal-x", "description": "plant watering"}]


def caller(repos: list[dict[str, Any]], login: str = "someone", ok: bool = True) -> tuple[Any, list]:
    calls: list = []

    def call(name: str, args: Mapping[str, Any]) -> dict[str, Any]:
        calls.append((name, dict(args)))
        if name == "github__get_me":
            return {"ok": ok, "result": json.dumps({"login": login})}
        return {"ok": ok, "result": json.dumps({"total_count": len(repos), "items": repos})}
    return call, calls


def test_fetch_reads_the_owners_login_then_their_repositories() -> None:
    call, calls = caller([{"full_name": "someone/zorbcar", "description": "car  kit\n"}, {"name": "qbt"}, "junk"])
    assert G.fetch(call) == [{"name": "zorbcar", "description": "car kit"}, {"name": "qbt", "description": ""}]
    assert calls[1] == ("github__search_repositories", {"query": "user:someone fork:true sort:updated",
                                                        "perPage": G.FETCH_MAX})
    assert G.fetch(caller([], login="bad login; rm")[0]) is None          # a login that is not one is not searched
    assert G.fetch(caller([], ok=False)[0]) is None


def test_refresh_keeps_a_day_and_keeps_the_old_list_when_github_fails(tmp_path: Path) -> None:
    path = tmp_path / G.FILE
    call, calls = caller(REPOS)
    assert G.refresh(path, call, clock=lambda: 1000.0) == [{"name": r["name"], "description": r["description"]}
                                                           for r in REPOS]
    assert stat.S_IMODE(path.stat().st_mode) == 0o600
    n = len(calls)
    G.refresh(path, call, clock=lambda: 1000.0 + G.REFRESH_SECS - 1)       # fresh: no request
    assert len(calls) == n
    broken, _ = caller([], ok=False)
    kept = G.refresh(path, broken, clock=lambda: 1000.0 + G.REFRESH_SECS + 1)
    assert [r["name"] for r in kept] == ["zorbcar", "remix", "qbt", "plantpal-x"]


def test_the_line_names_each_repository_with_what_it_is_clipped() -> None:
    line = G.line(REPOS)
    desc = REPOS[0]["description"][:G.DESC_CLIP - 1].rstrip() + "…"
    assert f"zorbcar ({desc})" in line and len(desc) == G.DESC_CLIP
    assert "; qbt;" in line and "\"my car repo\"" in line and line.endswith("plantpal-x (plant watering).")
    assert G.line([]) == ""
    many = [{"name": f"r{i}", "description": ""} for i in range(G.SHOWN + 5)]
    assert f"r{G.SHOWN - 1}" in G.line(many) and f"r{G.SHOWN}." not in G.line(many)


def test_only_unusual_names_open_github_as_words() -> None:
    names = G.keyword_names(REPOS)
    assert "zorbcar" in names and "plantpal-x" in names
    assert "remix" not in names and "qbt" not in names                       # a word, and too short
    assert G.names_repo("any new commits on zorbcar?", names)
    assert G.names_repo("is plantpal-x green", names)
    assert not G.names_repo("can you remix this song", names) and not G.names_repo("zorbcars", names)


def test_the_bridge_loads_them_and_the_brain_and_the_router_see_them(tmp_path: Path, monkeypatch: Any) -> None:
    from test_apps_mcp import bridge

    (tmp_path / G.FILE).write_text(json.dumps({"fetched": 9e12, "repos": REPOS}))       # kept and fresh
    monkeypatch.setattr(apps_mcp, "REPO_NAMES", frozenset())
    b, _ = bridge(tmp_path)
    assert [r["name"] for r in b.repos] == ["zorbcar", "remix", "qbt", "plantpal-x"]
    assert "zorbcar (autonomous car" in b.instructions()
    assert apps_mcp.names_app("github", "what's failing on zorbcar")
    from eve_bridge import toolset_router

    assert "github" in toolset_router.keyword_toolsets("what's failing on zorbcar")
    assert "github" not in toolset_router.keyword_toolsets("can you remix this")
    # GitHub down: no line, no names
    monkeypatch.setattr(apps_mcp, "REPO_NAMES", frozenset())
    down, _ = bridge(tmp_path, fail=("github",))
    assert down.repos == [] and "zorbcar" not in down.instructions() and not apps_mcp.REPO_NAMES
