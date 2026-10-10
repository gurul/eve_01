"""Tests for the audit log module."""

from __future__ import annotations

import io
import json
import tracemalloc
from pathlib import Path

from eve_bridge.audit import (
    AuditLog,
    default_path,
    format_entry,
    iter_entries,
    render,
)


def test_default_path_returns_path():
    p = default_path()
    assert isinstance(p, Path)
    # On all platforms the parent dir is somewhere under ~ (not absolute root).
    assert str(p).startswith(str(Path.home()))


def test_default_path_respects_env_override(monkeypatch, tmp_path):
    monkeypatch.setenv("EVE_BRIDGE_AUDIT", str(tmp_path / "audit.jsonl"))
    assert default_path() == tmp_path / "audit.jsonl"


def test_record_appends_jsonl(tmp_path):
    path = tmp_path / "audit.jsonl"
    log = AuditLog(path=path)
    log.record(
        session_id="abcdef1234567890",
        tool_name="Bash",
        hint="git push origin main",
        matcher="ask",
        decision="allow",
        source="stick",
        elapsed_s=2.345,
    )
    log.record(
        session_id="ffff",
        tool_name="Bash",
        hint="ls",
        matcher="allow",
        decision="allow",
        source="auto_allow",
    )
    lines = path.read_text(encoding="utf-8").strip().splitlines()
    assert len(lines) == 2
    a, b = json.loads(lines[0]), json.loads(lines[1])
    # Session id is truncated to first 8 chars for readability.
    assert a["session"] == "abcdef12"
    assert a["tool"] == "Bash"
    assert a["hint"] == "git push origin main"
    assert a["matcher"] == "ask"
    assert a["decision"] == "allow"
    assert a["source"] == "stick"
    assert a["elapsed_s"] == 2.345
    assert "ts" in a
    # auto_allow path omits elapsed_s.
    assert "elapsed_s" not in b


def test_record_long_hint_is_truncated(tmp_path):
    path = tmp_path / "audit.jsonl"
    log = AuditLog(path=path)
    long_hint = "x" * 500
    log.record(
        session_id="x", tool_name="Bash", hint=long_hint,
        matcher="ask", decision="deny", source="stick",
    )
    entry = json.loads(path.read_text(encoding="utf-8"))
    assert len(entry["hint"]) == 200


def test_record_creates_parent_dir(tmp_path):
    path = tmp_path / "deep" / "nested" / "audit.jsonl"
    log = AuditLog(path=path)
    log.record(
        session_id="x", tool_name="Bash", hint="ls",
        matcher="allow", decision="allow", source="auto_allow",
    )
    assert path.exists()


def test_record_swallows_io_errors(tmp_path):
    """Daemon must never crash on audit IO failure.

    Force the error portably by pointing the audit path AT an existing directory
    — ``open(dir, "a")`` raises ``IsADirectoryError`` on POSIX and ``PermissionError``
    on Windows, both ``OSError`` subclasses. POSIX chmod-based simulation doesn't
    work on Windows (mode bits ignored).
    """
    dir_as_path = tmp_path / "audit_target"
    dir_as_path.mkdir()
    log = AuditLog(path=dir_as_path)
    # Should not raise even though writing is impossible.
    log.record(
        session_id="x", tool_name="Bash", hint="ls",
        matcher="allow", decision="allow", source="auto_allow",
    )
    log.record(
        session_id="x", tool_name="Bash", hint="ls",
        matcher="allow", decision="allow", source="auto_allow",
    )
    # Sticky "failure logged" flag suppresses repeated warnings.
    assert log._failure_logged


def test_record_decision_can_be_none(tmp_path):
    """defer / ble_disconnected paths return no decision."""
    path = tmp_path / "audit.jsonl"
    log = AuditLog(path=path)
    log.record(
        session_id="x", tool_name="Bash", hint="some-tool",
        matcher="default", decision=None, source="defer",
    )
    entry = json.loads(path.read_text(encoding="utf-8"))
    assert entry["decision"] is None
    assert entry["source"] == "defer"


# ---- viewer ----

def _seed(path: Path, entries: list[dict]) -> None:
    path.write_text("\n".join(json.dumps(e) for e in entries) + "\n", encoding="utf-8")


def test_iter_entries_yields_in_order_and_filters(tmp_path):
    p = tmp_path / "audit.jsonl"
    _seed(p, [
        {"ts": "2026-05-16T00:01:02.000+08:00", "tool": "Bash", "decision": "allow", "source": "auto_allow", "hint": "ls"},
        {"ts": "2026-05-16T00:02:03.000+08:00", "tool": "Bash", "decision": "deny",  "source": "stick",      "hint": "rm -rf /"},
        {"ts": "2026-05-16T00:03:04.000+08:00", "tool": "Edit", "decision": "allow", "source": "stick",      "hint": "file.txt"},
    ])
    all_e = list(iter_entries(p))
    assert [e["tool"] for e in all_e] == ["Bash", "Bash", "Edit"]

    denied = list(iter_entries(p, decision="deny"))
    assert len(denied) == 1 and denied[0]["hint"] == "rm -rf /"

    via_stick = list(iter_entries(p, source="stick"))
    assert len(via_stick) == 2

    edits = list(iter_entries(p, tool="Edit"))
    assert len(edits) == 1


def test_iter_entries_skips_corrupt_lines(tmp_path):
    p = tmp_path / "audit.jsonl"
    p.write_text(
        '{"ts":"2026-05-16T00:00:00.000+08:00","tool":"Bash","decision":"allow","source":"auto_allow","hint":"ls"}\n'
        'not json at all\n'
        '{"ts":"2026-05-16T00:00:01.000+08:00","tool":"Bash","decision":"deny","source":"stick","hint":"rm"}\n',
        encoding="utf-8",
    )
    entries = list(iter_entries(p))
    assert len(entries) == 2


def test_iter_entries_missing_file_yields_nothing(tmp_path):
    assert list(iter_entries(tmp_path / "no-such.jsonl")) == []


def test_format_entry_truncates_long_hint():
    entry = {
        "ts": "2026-05-16T00:00:00.123+08:00",
        "tool": "Bash",
        "decision": "allow",
        "source": "auto_allow",
        "hint": "a" * 500,
    }
    line = format_entry(entry, ascii_only=True, width=80)
    assert len(line) <= 80
    assert line.endswith("…")


def test_format_entry_shows_time_tool_decision_hint():
    entry = {
        "ts": "2026-05-16T01:23:45.678+08:00",
        "tool": "Bash",
        "decision": "deny",
        "source": "stick",
        "hint": "git push origin main --force",
    }
    line = format_entry(entry, ascii_only=True, width=120)
    assert "01:23:45.678" in line
    assert "Bash" in line
    assert "deny" in line
    assert "stick" in line
    assert "git push origin main --force" in line


def test_format_entry_ascii_omits_ansi():
    entry = {"ts": "2026-05-16T00:00:00.000+08:00", "tool": "Bash", "decision": "allow", "source": "auto_allow", "hint": "ls"}
    line = format_entry(entry, ascii_only=True)
    assert "\033[" not in line


def test_format_entry_color_emits_ansi():
    entry = {"ts": "2026-05-16T00:00:00.000+08:00", "tool": "Bash", "decision": "deny", "source": "stick", "hint": "rm"}
    line = format_entry(entry, ascii_only=False)
    assert "\033[31m" in line  # red for deny


def test_render_prints_header_and_last_n_entries(tmp_path):
    p = tmp_path / "audit.jsonl"
    _seed(p, [
        {"ts": f"2026-05-16T00:00:{i:02d}.000+08:00", "tool": "Bash", "decision": "allow", "source": "auto_allow", "hint": f"cmd-{i}"}
        for i in range(10)
    ])
    buf = io.StringIO()
    render(path=p, last=3, ascii_only=True, out=buf)
    text = buf.getvalue()
    assert "# audit log:" in text
    assert "cmd-9" in text
    assert "cmd-8" in text
    assert "cmd-7" in text
    assert "cmd-6" not in text  # outside last 3


def test_render_missing_file_says_empty(tmp_path):
    buf = io.StringIO()
    render(path=tmp_path / "no-such.jsonl", last=20, ascii_only=True, out=buf)
    assert "(empty" in buf.getvalue()


def test_render_tails_matching_entries_in_order(tmp_path):
    p = tmp_path / "audit.jsonl"
    _seed(p, [{"tool": "Bash", "decision": "deny" if i % 2 else "allow", "hint": f"cmd-{i}"}
              for i in range(12)])
    buf = io.StringIO()
    render(path=p, last=3, decision="deny", ascii_only=True, out=buf)
    lines = buf.getvalue().splitlines()[1:]
    assert len(lines) == 3
    assert [line.rsplit(" ", 1)[-1] for line in lines] == ["cmd-7", "cmd-9", "cmd-11"]


def test_render_non_positive_last_keeps_the_complete_history(tmp_path):
    p = tmp_path / "audit.jsonl"
    _seed(p, [{"hint": f"cmd-{i}"} for i in range(30)])
    for last in (0, -1):
        buf = io.StringIO()
        render(path=p, last=last, ascii_only=True, out=buf)
        assert len(buf.getvalue().splitlines()) == 31
        assert "cmd-0" in buf.getvalue() and "cmd-29" in buf.getvalue()


def test_render_tail_memory_does_not_scale_with_history(tmp_path):
    class Sink:
        def isatty(self):
            return False

        def write(self, text):
            return len(text)

    small, large = (tmp_path / f"{name}.jsonl" for name in ("small", "large"))
    for path, count in ((small, 400), (large, 4000)):
        with path.open("w") as file:
            for i in range(count):
                file.write(json.dumps({"tool": "Bash", "hint": str(i) + "x" * 200}) + "\n")

    def peak(path, last):
        tracemalloc.start()
        try:
            render(path=path, last=last, ascii_only=True, out=Sink())
            return tracemalloc.get_traced_memory()[1]
        finally:
            tracemalloc.stop()

    small_peak = peak(small, 20)
    large_peak = peak(large, 20)
    # Ten times the input must not retain ten times the parsed records.
    assert large_peak < 2 * small_peak + 100_000, (small_peak, large_peak)
    # Positive control: the previous approach retains every parsed record.
    tracemalloc.start()
    try:
        retained = list(iter_entries(large))
        full_peak = tracemalloc.get_traced_memory()[1]
        assert len(retained) == 4000
    finally:
        tracemalloc.stop()
    assert full_peak > 5 * large_peak, (large_peak, full_peak)


def test_iter_entries_skips_non_object_json(tmp_path):
    p = tmp_path / "audit.jsonl"
    p.write_text('null\n[]\n42\n{"decision":"allow","hint":"safe"}\n')
    assert list(iter_entries(p, decision="allow")) == [{"decision": "allow", "hint": "safe"}]
