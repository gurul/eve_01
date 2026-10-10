"""What a computer-use agent tells its callers, as one small leaf module.

The voice session, the Telegram door, the reflex wrapper and every agent (Codex's, the
legacy planner in computer_agent.py) speak ``AgentEvent``. It lived in computer_agent.py, so each of them
imported the 1,400-line legacy planner (and, through it, the lanes and Jev) to get a three-field dataclass.
computer_agent re-exports it, so ``from .computer_agent import AgentEvent`` keeps working.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class AgentEvent:
    """What the voice session and the board get told. kind: started | turn |
    commentary | exec | progress | ask | final | cancelled | error."""

    kind: str
    text: str = ""
    turn: int = 0
