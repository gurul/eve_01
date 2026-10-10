"""link_memory.py: the Memory card over a fake memory.Memory. Every action, the off state, and that nothing is
forgotten without the preview's token. The words here are made up."""

from __future__ import annotations

import asyncio
from typing import Any, Optional

from eve_bridge import link_memory


class FakeMemory:
    def __init__(self) -> None:
        self.calls: list[tuple[str, Any]] = []
        self.pending: dict[str, str] = {}

    def profile(self) -> str:
        self.calls.append(("profile", None))
        return "# You\n- likes tea"

    def search(self, query: str, days_back: Any = None, **kw: Any) -> dict[str, Any]:
        self.calls.append(("search", query))
        if query == "nothing":
            return {"ok": True, "hits": [], "recalled": [], "said": [], "note": "nothing matched"}
        return {"ok": True, "hits": [{"id": "ana", "line": "- likes tea (2026-10-01)", "src": "2026-10-01 09:00"}],
                "recalled": ["(2026-10-01, app) likes tea"],
                "said": [{"day": "2026-10-01", "time": "09:00", "ch": "telegram", "who": "owner", "text": "i like tea",
                          "before": "hi", "after": "ok"}]}

    def forget_preview(self, query: str) -> dict[str, Any]:
        self.calls.append(("preview", query))
        if query == "nothing":
            return {"ok": True, "total": 0, "layers": {}, "token": "", "expires_in": 0, "note": "nothing in memory matches"}
        self.pending["tok1"] = query
        return {"ok": True, "total": 3, "layers": {"records": 1, "transcripts": 2, "index": 0}, "token": "tok1",
                "expires_in": 600}

    def star(self, text: str) -> Optional[str]:
        self.calls.append(("star", text))
        return None if text == "refuse me" else "★ " + text

    def forget_apply(self, token: str) -> dict[str, Any]:
        self.calls.append(("apply", token))
        if self.pending.pop(token, None) is None:
            return {"ok": False, "reason": "the token is unknown, used or expired; preview again"}
        return {"ok": True, "total": 3, "layers": {"records": 1, "transcripts": 2, "index": 0}}


def panel(memory: Optional[FakeMemory]) -> link_memory.MemoryPanel:
    return link_memory.MemoryPanel(lambda: memory)


def run(coro: Any) -> Any:
    return asyncio.run(coro)


def test_off_memory_is_unavailable_and_says_so() -> None:
    p = panel(None)
    assert p.available is False
    assert run(p.panel({"action": "search", "query": "tea"})) == {"ok": False, "line": link_memory.OFF_LINE}


def test_the_profile_is_the_default_action() -> None:
    m = FakeMemory()
    p = panel(m)
    assert p.available is True
    assert run(p.panel({})) == {"ok": True, "profile": "# You\n- likes tea", "line": ""}
    assert m.calls == [("profile", None)]


def test_a_search_returns_the_three_groups_without_the_lines_around_a_hit() -> None:
    m = FakeMemory()
    out = run(panel(m).panel({"action": "search", "query": "  tea   please "}))
    assert out["ok"] and out["hits"][0]["id"] == "ana" and out["recalled"] == ["(2026-10-01, app) likes tea"]
    assert out["said"] == [{"day": "2026-10-01", "time": "09:00", "ch": "telegram", "who": "owner", "text": "i like tea"}]
    assert m.calls == [("search", "tea please")]                      # trimmed, one space
    empty = run(panel(m).panel({"action": "search", "query": "nothing"}))
    assert empty["ok"] and empty["line"] == "nothing matched" and empty["hits"] == []
    assert run(panel(m).panel({"action": "search", "query": "   "})) == {"ok": False, "line": "Type a word or two to search for."}


def test_forget_is_a_preview_with_counts_then_the_token_once() -> None:
    m = FakeMemory()
    p = panel(m)
    preview = run(p.panel({"action": "forget", "query": "tea"}))
    assert preview["ok"] and preview["total"] == 3 and preview["token"] == "tok1"
    assert preview["line"] == "3 things match, in facts, what was said"
    assert "tea" not in str(preview["line"])                           # counts, never the words
    assert ("apply", "tok1") not in m.calls                            # nothing forgotten yet
    done = run(p.panel({"action": "apply", "token": "tok1"}))
    assert done == {"ok": True, "total": 3, "layers": {"records": 1, "transcripts": 2, "index": 0}, "line": "Forgot 3 things."}
    again = run(p.panel({"action": "apply", "token": "tok1"}))
    assert again["ok"] is False and "preview again" in again["line"]
    none = run(p.panel({"action": "forget", "query": "nothing"}))
    assert none["ok"] and none["total"] == 0 and none["token"] == "" and none["line"] == "nothing in memory matches"


def test_remember_stars_one_claim_and_says_so() -> None:
    m = FakeMemory()
    p = panel(m)
    assert run(p.panel({"action": "remember", "text": "  Guru was born in Tenali  "})) == {"ok": True, "line": "Remembered."}
    assert m.calls == [("star", "Guru was born in Tenali")]
    assert run(p.panel({"action": "remember", "text": "no"}))["ok"] is False      # too short to be a claim
    assert run(p.panel({"action": "remember", "text": "refuse me"})) == {"ok": False, "line": "That couldn't be kept just now."}
    run(p.panel({"action": "remember", "text": "x" * 2000}))
    assert len(m.calls[-1][1]) == link_memory.MAX_REMEMBER_CHARS


def test_an_unknown_action_and_an_overlong_query_are_bounded() -> None:
    m = FakeMemory()
    assert run(panel(m).panel({"action": "delete_everything"}))["ok"] is False
    run(panel(m).panel({"action": "search", "query": "x" * 1000}))
    assert len(m.calls[-1][1]) == link_memory.MAX_QUERY_CHARS


class HidingMemory(FakeMemory):
    def __init__(self) -> None:
        super().__init__()
        self.hidden_rows: list[str] = []

    def hide(self, text: str) -> Optional[str]:
        self.calls.append(("hide", text))
        if text == "refuse me":
            return None
        self.hidden_rows.append(text)
        return f"- {text} (2026-10-09)"

    def unhide(self, text: str) -> bool:
        self.calls.append(("unhide", text))
        if text in self.hidden_rows:
            self.hidden_rows.remove(text)
            return True
        return False

    def hidden(self) -> list[str]:
        self.calls.append(("hidden", None))
        return list(self.hidden_rows)


def test_hide_unhide_and_the_hidden_list(caplog: Any) -> None:
    m = HidingMemory()
    p = panel(m)
    caplog.set_level("INFO", logger="eve_bridge.link_memory")
    assert run(p.panel({"action": "hide", "text": "  likes\n tea  "})) == {
        "ok": True, "line": "eve won't bring that up again."}
    assert run(p.panel({"action": "hidden"})) == {"ok": True, "hidden": ["likes tea"], "line": ""}
    assert run(p.panel({"action": "unhide", "text": "likes tea"})) == {
        "ok": True, "line": "eve can bring that up again."}
    assert run(p.panel({"action": "unhide", "text": "likes tea"}))["ok"] is False
    assert run(p.panel({"action": "hidden"}))["hidden"] == []
    assert run(p.panel({"action": "hide", "text": "refuse me"}))["ok"] is False
    assert ("hide", "likes tea") in m.calls
    assert "hide → kept" in caplog.text and "unhide → done" in caplog.text
    assert "tea" not in caplog.text                          # logs carry counts, never the words


def test_hide_needs_words_and_is_bounded() -> None:
    m = HidingMemory()
    p = panel(m)
    assert run(p.panel({"action": "hide", "text": "   "})) == {"ok": False, "line": "Nothing to hide."}
    assert run(p.panel({"action": "unhide"})) == {"ok": False, "line": "Nothing to unhide."}
    assert not [c for c in m.calls if c[0] in ("hide", "unhide")]
    run(p.panel({"action": "hide", "text": "x" * 2000}))
    assert len(m.calls[-1][1]) == link_memory.MAX_HIDE_CHARS
