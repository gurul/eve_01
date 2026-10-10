"""In-memory aggregated state across all live Claude Code sessions."""

from __future__ import annotations

import time
from dataclasses import dataclass
from typing import Optional

# Claude Code Notification kinds (the hook's ``notification_type``):
# permission_prompt and elicitation_dialog mean a session is blocked on the
# human, and any kind not in NON_WAITING_NOTIFICATION_KINDS counts as waiting
# (notification_waits). Daemon log counts: idle_prompt 566,
# permission_prompt 155, auth_success 19, idle 2, elicitation_response 1. The
# owner's rule (firmware main.cpp derive()): Claude idle -> the pet sleeps; a
# session blocked on a permission -> attention. So an idle reminder must not
# light the attention pose over a live voice conversation.
# Kinds that are news, not a request: they do not mark the session waiting.
NON_WAITING_NOTIFICATION_KINDS = frozenset({
    "idle_prompt", "idle", "auth_success", "elicitation_response",
})


def notification_waits(kind: Optional[str]) -> bool:
    """True when a Notification of this kind means Claude waits on the human.

    A missing or unknown kind counts as waiting — the old behavior, kept so a
    new blocking kind from Claude Code still gets the human's attention.
    """
    if not kind:
        return True
    return kind not in NON_WAITING_NOTIFICATION_KINDS


@dataclass
class Session:
    session_id: str
    started_at: float
    transcript_path: Optional[str] = None
    cwd: Optional[str] = None
    running: bool = False
    # Set by the Notification hook when Claude is blocked on the user
    # (permission prompt in the terminal, idle waiting for input). Drives
    # the firmware's attention animation via the heartbeat's ``waiting``
    # count. 0.0 = not waiting. Cleared by turn_begin/posttooluse.
    needs_input_at: float = 0.0
    # Most recent tool this session touched, for the monitor's per-agent row.
    # Set on pre/post tool use; purely cosmetic, so a miss just blanks the
    # column rather than mis-stating the session's state.
    last_tool: str = ""


@dataclass
class Entry:
    """One transcript-style line surfaced on the stick."""
    at: float  # wall-clock epoch seconds (for formatting HH:MM)
    text: str


class State:
    """Aggregator. Hook handlers mutate this via the public methods below."""

    MAX_ENTRIES = 8  # stick's display can only hold a few

    def __init__(self) -> None:
        self.sessions: dict[str, Session] = {}
        self.entries: list[Entry] = []
        self.tokens_cumulative: int = 0
        self.tokens_today: int = 0
        # USD cost estimates derived from the same usage records that feed
        # tokens_*. See pricing.py. Displayed in the statusline.
        self.cost_cumulative: float = 0.0
        self.cost_today: float = 0.0
        # Monotonic timestamp until which heartbeats should carry
        # ``completed: true`` — the firmware's celebrate-animation trigger.
        # Pulsed by turn_end, observed by build_heartbeat.
        self.completed_until: float = 0.0

    # ---- session lifecycle ----

    def session_start(self, session_id: str, transcript_path: str | None = None, cwd: str | None = None) -> None:
        if session_id not in self.sessions:
            self.sessions[session_id] = Session(
                session_id=session_id,
                started_at=time.time(),
                transcript_path=transcript_path,
                cwd=cwd,
            )

    def session_end(self, session_id: str) -> None:
        self.sessions.pop(session_id, None)

    # ---- turn lifecycle ----

    def turn_begin(self, session_id: str) -> None:
        s = self.sessions.get(session_id)
        if s is not None:
            s.running = True
            s.needs_input_at = 0.0  # user responded — attention satisfied

    def turn_end(self, session_id: str) -> None:
        s = self.sessions.get(session_id)
        if s is not None:
            s.running = False

    # ---- needs-input (Notification hook) ----

    # Auto-expire so a notification for an abandoned session doesn't leave
    # the pet impatient forever.
    NEEDS_INPUT_TTL_SECS = 15 * 60

    def needs_input(self, session_id: str) -> None:
        s = self.sessions.get(session_id)
        if s is None:
            s = Session(session_id=session_id, started_at=time.time())
            self.sessions[session_id] = s
        s.needs_input_at = time.monotonic()

    def note_tool(self, session_id: str, tool_name: str) -> None:
        """Record the tool a session most recently touched (monitor display).

        Silently ignores unknown sessions — unlike needs_input this must not
        conjure a Session, or a stray hook would invent an agent row for a
        session that never started.
        """
        s = self.sessions.get(session_id)
        if s is not None and tool_name:
            s.last_tool = tool_name

    def input_received(self, session_id: str) -> None:
        s = self.sessions.get(session_id)
        if s is not None:
            s.needs_input_at = 0.0

    def _needs_input_live(self, s: Session) -> bool:
        return (
            s.needs_input_at > 0.0
            and (time.monotonic() - s.needs_input_at) < self.NEEDS_INPUT_TTL_SECS
        )

    def attention_cwd(self) -> str:
        """The cwd behind the pet's attention state — for tap-the-pet focus.

        The most recently flagged needs-input session (Claude idle, waiting
        on a reply in the terminal). Empty when nothing is waiting — focus
        then degrades to raising whichever terminal app is running.
        """
        waiting = [s for s in self.sessions.values() if self._needs_input_live(s)]
        if waiting:
            newest = max(waiting, key=lambda s: s.needs_input_at)
            return newest.cwd or ""
        return ""

    # ---- celebrate pulse ----

    def pulse_completed(self, duration_secs: float = 5.0) -> None:
        """Make subsequent heartbeats include ``completed: true`` for a few
        seconds — firmware reads that field as the celebrate-animation
        trigger (data.h:_applyJson → main.cpp:derive)."""
        self.completed_until = time.monotonic() + duration_secs

    @property
    def is_celebrating(self) -> bool:
        return time.monotonic() < self.completed_until

    # ---- entries ----

    def add_entry(self, text: str, at: Optional[float] = None) -> None:
        text = text.strip()
        if not text:
            return
        self.entries.insert(0, Entry(at=at if at is not None else time.time(), text=text))
        del self.entries[self.MAX_ENTRIES:]

    # ---- tokens ----

    def set_tokens(
        self,
        cumulative: int,
        today: int,
        cost_cumulative: float = 0.0,
        cost_today: float = 0.0,
    ) -> None:
        """Called by the JSONL tailer after recomputing from transcript files."""
        self.tokens_cumulative = cumulative
        self.tokens_today = today
        self.cost_cumulative = cost_cumulative
        self.cost_today = cost_today

    # ---- aggregates ----

    @property
    def total(self) -> int:
        return len(self.sessions)

    @property
    def running_count(self) -> int:
        return sum(1 for s in self.sessions.values() if s.running)

    @property
    def waiting_count(self) -> int:
        return sum(
            1 for s in self.sessions.values()
            if self._needs_input_live(s)
        )

    # ---- per-agent rows (monitor display) ----

    # How many rows the landscape monitor can show. The firmware clips to the
    # same number; sending more would only burn heartbeat bytes.
    MAX_AGENT_ROWS = 6

    # Display width of the name column on the panel, in characters.
    AGENT_NAME_CHARS = 14

    def agent_rows(self, limit: Optional[int] = None) -> list[dict[str, str]]:
        """One compact row per live session for the monitor's agent list.

        Keys are single letters because this rides in every heartbeat and the
        firmware's line buffer is 2KB shared with a 720-byte prompt detail.

        Ordering is (rank, name) — deterministic rather than recency-based, so
        a row doesn't hop between refreshes. On e-ink a reordered list costs a
        full redraw and reads as flicker, and "which agent is where" is worth
        more than "which moved last".
        """
        import os

        cap = self.MAX_AGENT_ROWS if limit is None else limit
        rows: list[tuple[int, str, dict[str, str]]] = []
        for s in self.sessions.values():
            name = os.path.basename((s.cwd or "").rstrip("/")) or s.session_id[:6] or "?"
            name = name[: self.AGENT_NAME_CHARS]
            if self._needs_input_live(s):
                rank, status, tool = 0, "wait", s.last_tool
            elif s.running:
                rank, status, tool = 1, "run", s.last_tool
            else:
                rank, status, tool = 2, "idle", s.last_tool
            row = {"n": name, "s": status}
            if tool:
                row["t"] = tool[:12]
            rows.append((rank, name, row))
        rows.sort(key=lambda r: (r[0], r[1]))
        return [r[2] for r in rows[:cap]]
