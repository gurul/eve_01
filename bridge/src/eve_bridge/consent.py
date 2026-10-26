"""Reading the owner's yes or no, for every reply that gates an action.

A permission prompt from Claude Code, a Composio app action, a browser step's approval: each waits for the
owner's next message and acts on a yes. The rule used to be a prefix match ("y" admitted "yikes, no"), in
two different word orders at two sites and an exact set at a third. It is one rule now, and it fails
closed:

* **yes**: the first word is a yes-word (or the reply opens with "do it" / "go ahead"), AND no word of
  the reply negates or holds it ("yeah no", "ok wait", "sure, but don't", "do not").
* **no**: the first word is a no-word.
* anything else is neither: nothing is approved, and a caller that can defer (a permission prompt) leaves
  the decision to the dialog on the Mac.
* a ``Withdrawn`` answer (the owner typed something else while a firm question waited, EVE_GATE_WITHDRAW) is a
  no under every reading here, whatever the owner's words were.
"""

from __future__ import annotations

import re

YES_WORDS = frozenset({"yes", "y", "yeah", "yep", "yup", "ya", "ok", "okay", "k", "sure", "go", "allow", "allowed",
                       "approve", "approved", "confirm", "confirmed", "affirmative", "proceed", "absolutely", "definitely"})
YES_OPENINGS = ("do it", "go ahead", "go for it", "sounds good", "of course")
NO_WORDS = frozenset({"no", "n", "nope", "nah", "deny", "denied", "stop", "don't", "dont", "block", "cancel",
                      "decline", "reject", "never", "negative"})
# A yes that also says one of these is not a yes: it takes it back, or asks to hold.
HOLD_WORDS = frozenset({"no", "not", "nope", "nah", "don't", "dont", "never", "stop", "wait", "cancel", "hold",
                        "deny", "block", "but", "unless", "later"})

_WORD = re.compile(r"[a-z]+(?:'[a-z]+)?")


class Withdrawn(str):
    """The answer a withdrawable question gets when the owner types something else instead of answering it
    (telegram._withdraw, EVE_GATE_WITHDRAW). Its string value is a fixed marker, never the owner's words, so even a
    reader that only sees a plain string finds no yes in it; the words ride on ``text``. Every rule below reads it
    as a no."""

    MARK = "\x00withdrawn"
    text: str

    def __new__(cls, text: str) -> "Withdrawn":
        made = super().__new__(cls, cls.MARK)
        made.text = str(text)
        return made


def _words(reply: str) -> list[str]:
    return _WORD.findall(str(reply).lower().replace("’", "'"))


def approves(reply: str) -> bool:
    """The reply is an unambiguous yes."""
    if isinstance(reply, Withdrawn):
        return False
    words = _words(reply)
    if not words:
        return False
    opens = words[0] in YES_WORDS or " ".join(words).startswith(YES_OPENINGS)
    return opens and not any(w in HOLD_WORDS for w in words)


def denies(reply: str) -> bool:
    """The reply is a no."""
    if isinstance(reply, Withdrawn):
        return True
    words = _words(reply)
    return bool(words) and words[0] in NO_WORDS


def decision(reply: str) -> str:
    """"allow", "deny", or "" when the reply is neither (the caller defers)."""
    return "allow" if approves(reply) else "deny" if denies(reply) else ""


# A bare answer is short and made only of these: the yes and no words, the words of the yes openings, and a
# little politeness. "yes", "no, leave it", "go ahead please" are bare; "ok, also update the README" and
# "go check the logs" are sentences that happen to open with a yes-word (owner, 2026-09-23).
BARE_MAX_WORDS = 4
BARE_EXTRA_WORDS = frozenset({"please", "thanks", "thank", "you", "leave", "it", "that", "this", "one", "now"})
_BARE_WORDS = (YES_WORDS | NO_WORDS | BARE_EXTRA_WORDS
               | frozenset(w for opening in YES_OPENINGS for w in opening.split()))


def bare_decision(reply: str) -> str:
    """"allow" or "deny" only when the whole reply is a yes or a no, else "". For a prompt that has another
    way to be answered (Allow/Deny buttons on the screen), where anything that is not plainly the answer
    must go on to where it was meant to go instead of being eaten. ``decision`` stays the rule where the
    next message is the answer by construction. A ``Withdrawn`` answer is "deny"."""
    if isinstance(reply, Withdrawn):
        return "deny"
    words = _words(reply)
    if not words or len(words) > BARE_MAX_WORDS or any(w not in _BARE_WORDS for w in words):
        return ""
    return decision(reply)
