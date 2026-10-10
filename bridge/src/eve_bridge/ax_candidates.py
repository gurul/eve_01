"""The fast lane's senses: the frontmost window's accessibility tree as a bounded,
code-owned candidate list (fast_lane.py ranks it, decider.py picks from it).

Contract
--------
`ax_snapshot()` walks the focused window of one app and returns a `Snapshot`:
every labelled control as a `Candidate` in reading order (y, then x), tagged
with what the lane must never guess — enabled, inside a sheet or dialog,
inside page content, a secure field, an editable field. `rank_candidates()`
turns a snapshot plus an objective into a short menu: it drops what the lane
may not offer (disabled, unlabelled, dialog, secure, page links, and every
`is_sensitive()` label unless the caller allows it), scores the rest by a role
prior plus objective-token overlap, and caps the list. No app-specific boosts
anywhere: the menu is code, the pick is the model's, the judgement is code again.

`SENSITIVE_LABEL` is word-bounded and fail-closed: any label that carries a
consequential verb ("Delete Event", "Send", "Don't Save", "OK", "Remove
filter", "Export as PDF") is sensitive wherever the word sits, and the lane
answers `confirm` for it so the planner can ask the human. "Postcode",
"Composer", "Dispatcher", "Downloads" are not: the verb must be a whole word.

Raw walk shape (the injectable backend, what fixtures store)
-------------------------------------------------------------
`walk(pid, *, max_nodes, budget_secs)` returns one JSON-serialisable dict:

    {"app": str, "bundle": str, "pid": int, "title": str, "fullscreen": bool,
     "node_count": int, "truncated": bool, "secs": float,
     "root": <node> | None}          # None when the app has no window

    <node> = {"role": "AXButton", "subrole": "", "title": "Today", "description": "",
              "value": str | int | float | bool | None, "frame": [x, y, w, h] | None,
              "actions": ["AXPress"], "enabled": bool, "focused": bool,
              "children": [<node>, ...], "ancestor_roles": ["AXWindow", "AXGroup"]}

Frames are points with a top-left origin (click coordinates). `snapshot_from_raw()`
derives the same Snapshot from the same dump every time, which is what
tests/test_fixtures_ax.py checks for every committed fixture.

The real walker (`ax_walk`) uses AXUIElementCopyMultipleAttributeValues — one
IPC per node for role, subrole, title, description, value, position, size,
enabled, focused and children — plus one AXUIElementCopyActionNames only where
an action can change the answer (a node whose role is not already pressable,
outside page content, not a container). It visits content subtrees (AXWebArea,
AXScrollArea, AXContentList) before chrome (`content_first=True`, the plan's
default) so a truncated walk of a web page still carries the page, and stops at
`max_nodes` or `budget_secs` with `truncated=True`.

Measured on this Mac (2026-09-21, bridge venv, Accessibility granted, five
walks each): Calendar month view 124 nodes, 0.10-0.20 s, 59 labelled pressables
(Calendar's AX server answers a 10-attribute call in ~1 ms a node; radio buttons
carry AXValue 1 when selected; events are AXStaticText with AXPress; the three
untitled 16x16 buttons are the traffic lights). System Settings 167 nodes,
0.04-0.11 s, 38 pressables after the row/cell de-duplication (the sidebar is 39
AXRow/AXCell pairs whose label is a child AXStaticText). Safari on a Wikipedia
article 3632 nodes, 0.36-0.43 s, 79 pressables of which 9 are chrome (a web
node costs 0.07 ms; links carry their text in a child AXStaticText; the web
content appears only after the first walk). The 0.5 s default budget covers all
three; a walk capped at 800 nodes on that page reports truncated=True and, with
content first, no chrome. An app with no window (Calendar closed, Slack) reports
0 nodes. pyobjc's `hasattr` on a bridged object costs ~0.2 ms a call and was
half of the first Safari walk: the walker only uses `isinstance`.

Only the walker imports pyobjc, lazily; everything else is plain Python so the
tests run anywhere with hand-rolled raw dumps.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
from dataclasses import dataclass, field
from typing import Any, Callable, Optional

DEFAULT_MAX_NODES = 4000
DEFAULT_BUDGET_SECS = 0.5
ATTRIBUTES = ("AXRole", "AXSubrole", "AXTitle", "AXDescription", "AXValue", "AXPosition", "AXSize",
              "AXEnabled", "AXFocused", "AXChildren")
ROLE_NAMES: dict[str, str] = {
    "AXButton": "button", "AXLink": "link", "AXCheckBox": "checkbox", "AXRadioButton": "radio button",
    "AXMenuItem": "menu item", "AXMenuButton": "menu button", "AXPopUpButton": "pop up button",
    "AXTab": "tab", "AXTextField": "text field", "AXSecureTextField": "text field", "AXSearchField": "search field",
    "AXTextArea": "text area", "AXComboBox": "combo box", "AXRow": "row", "AXCell": "cell",
    "AXDisclosureTriangle": "disclosure triangle", "AXSlider": "slider", "AXImage": "image",
    "AXStaticText": "static text", "AXSwitch": "toggle", "AXToggle": "toggle",
}
# Roles the lane may click by role alone (an AXPress action makes any node pressable).
PRESSABLE_ROLES = frozenset({"button", "link", "checkbox", "radio button", "menu item", "menu button",
                             "pop up button", "tab", "toggle", "disclosure triangle", "row", "cell"})
EDITABLE_ROLES = frozenset({"text field", "search field", "text area", "combo box"})
ROLE_PRIOR: dict[str, int] = {"button": 3, "tab": 3, "radio button": 3, "checkbox": 3, "menu item": 3,
                              "menu button": 3, "pop up button": 3, "row": 2, "cell": 2, "toggle": 2,
                              "disclosure triangle": 2, "link": 1, "image": 1, "static text": 1}
CONTENT_ROLES = frozenset({"AXWebArea", "AXContentList"})
CONTENT_FIRST_ROLES = frozenset({"AXWebArea", "AXScrollArea", "AXContentList"})
CONTENT_FIRST = True                   # PLAN §1a: content subtree before chrome
DIALOG_SUBROLES = frozenset({"AXDialog", "AXSystemDialog", "AXFloatingWindow"})
# Containers never carry an action worth an IPC; their children do.
CONTAINER_ROLES = frozenset({"AXWindow", "AXGroup", "AXScrollArea", "AXSplitGroup", "AXToolbar", "AXList",
                             "AXOutline", "AXTable", "AXWebArea", "AXLayoutArea", "AXLayoutItem", "AXTabGroup",
                             "AXSplitter", "AXScrollBar", "AXBrowser", "AXDrawer", "AXSheet", "AXUnknown",
                             "AXContentList", "AXGrowArea", "AXRulerMarker", "AXRuler", "AXMenuBar"})
SKIP_DESCEND_ROLES = frozenset({"AXScrollBar", "AXMenuBar", "AXRuler"})
STOP_WORDS = frozenset({"the", "and", "for", "with", "into", "from", "this", "that", "then", "now", "please",
                        "open", "show", "switch", "view", "see", "check", "find", "click", "tap", "press",
                        "select", "choose", "make", "turn", "set", "get", "put", "want", "need", "use",
                        "let", "can", "you", "your", "its", "one", "all", "some", "any"})
MAX_VALUE_CHARS = 40
MAX_CONTEXT_LINES = 8
MAX_CONTEXT_TEXTS = 6
MAX_DIALOG_CHARS = 80
_URL = re.compile(r"(?:\b[a-z][a-z0-9+.\-]*://\S+|\bwww\.\S+)", re.IGNORECASE)
_SPACES = re.compile(r"\s+")
_SEQ = 0

_SENSITIVE_PHRASES = (
    "delete", "remove", "trash", "erase", "empty", "discard", "don[’'`]?t\\s+save", "replace", "overwrite",
    "send", "submit", "post", "reply", "pay", "buy", "purchase", "subscribe", "checkout", "check\\s+out",
    "sign\\s+in", "signin", "log\\s+in", "login", "log\\s+out", "logout", "sign\\s+out", "signout", "password",
    "authori[sz]e", "permissions?", "allow", "captcha", "upload", "share", "export", "install", "uninstall",
    "quit", "close\\s+(?:window|tab|all)", "shut\\s+down", "shutdown", "restart", "reset", "forget",
    "disconnect", "revoke", "eject", "call", "facetime", "decline", "accept", "agree", "continue", "ok",
    "okay", "yes", "apply", "archive", "unsubscribe", "block", "report", "move\\s+to\\s+trash",
    "system\\s+settings", "security",
    # 2026-09-21: the verbs the request-level gates already knew (task_router.CONSEQUENTIAL, the risky_action
    # noul) and this table did not. They were masked while the lane only pressed a control the human had named
    # word for word; a plan executor presses controls nobody named. "Forward" (the browser's and Finder's
    # navigation button) and "Format" (Notes' toolbar) are left out on purpose: whole phrases cover the risky use.
    "place\\s+(?:an?\\s+|your\\s+|my\\s+)?order", "order\\s+now", "complete\\s+(?:order|purchase)", "confirm",
    "book", "publish", "transfer", "clear\\s+(?:history|all|data|cache|browsing\\s+data)",
    "cancel\\s+(?:subscription|membership|order|plan)", "merge\\s+pull\\s+request", "squash\\s+and\\s+merge",
    "rebase\\s+and\\s+merge", "format\\s+(?:disk|drive|volume)", "turn\\s+o(?:n|ff)", "approve", "kill",
    "add\\s+to\\s+(?:cart|bag|basket)", "mark\\s+as\\s+(?:spam|junk)", "spam", "junk", "revert",
    "sign\\s+up", "signup", "create\\s+(?:an?\\s+)?account", "donate", "withdraw", "deposit", "bid",
)
# Word-bounded on both sides so "Postcode", "Composer", "Dispatcher", "Okay dokey"-style prefixes
# never fire on a fragment, while "Delete Event", "Share…", "OK" and "Don't Save" always do.
SENSITIVE_LABEL = re.compile(r"(?<![a-z0-9])(?:" + "|".join(_SENSITIVE_PHRASES) + r")(?![a-z0-9])",
                             re.IGNORECASE)


def is_sensitive(label: Any) -> bool:
    """True when a consequential verb is a whole word anywhere in the label. Fail-closed: a
    label that is not a string counts as sensitive; only an empty string is not."""
    if not isinstance(label, str):
        return True
    text = label.strip()
    if not text:
        return False
    try:
        return SENSITIVE_LABEL.search(text) is not None
    except Exception:  # noqa: BLE001 — a regex failure must never let a click through
        return True


# ---- data ------------------------------------------------------------------------------

@dataclass(frozen=True)
class Candidate:
    id: str                                # reading-order index within THIS snapshot; never reused
    role: str                              # plain words: "button", "radio button", "search field", ...
    label: str                             # AXTitle / AXDescription / descendant static text — full
    value: str                             # "selected" | "checked" | "unchecked" | "expanded" | "collapsed" | text ≤ 40 | ""
    frame: tuple[int, int, int, int]       # points, top-left origin (click coordinates)
    actions: tuple[str, ...]
    enabled: bool
    app: str
    in_content: bool                       # under an AXWebArea / AXContentList — page content, not chrome
    in_dialog: bool                        # under an AXSheet, or the window is a dialog
    secure: bool                           # AXSecureTextField
    editable: bool                         # text field / search field / text area / combo box

    def to_dict(self) -> dict[str, Any]:
        return {"id": self.id, "role": self.role, "label": self.label, "value": self.value,
                "frame": list(self.frame), "actions": list(self.actions), "enabled": self.enabled,
                "app": self.app, "in_content": self.in_content, "in_dialog": self.in_dialog,
                "secure": self.secure, "editable": self.editable}

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "Candidate":
        return cls(id=str(d["id"]), role=str(d["role"]), label=str(d.get("label", "")),
                   value=str(d.get("value", "")), frame=tuple(int(v) for v in d["frame"]),  # type: ignore[arg-type]
                   actions=tuple(str(a) for a in d.get("actions", ())), enabled=bool(d.get("enabled", True)),
                   app=str(d.get("app", "")), in_content=bool(d.get("in_content")),
                   in_dialog=bool(d.get("in_dialog")), secure=bool(d.get("secure")),
                   editable=bool(d.get("editable")))


@dataclass(frozen=True)
class Snapshot:
    seq: int
    app: str
    bundle: str
    title: str
    pid: int
    elements: tuple[Candidate, ...] = ()           # reading order (y, then x)
    context_lines: tuple[str, ...] = ()            # ≤ 8: "title: …", "focused: …", static texts
    focused: Optional[Candidate] = None
    dialog_text: str = ""                          # first static text of an open sheet/dialog, ≤ 80 chars
    fullscreen: bool = False
    node_count: int = 0
    truncated: bool = False
    secs: float = 0.0
    _by_id: dict[str, Candidate] = field(default_factory=dict, compare=False, repr=False)
    # The live AXUIElement behind each candidate id, from the walk that made this snapshot (ax_snapshot with the
    # real walker only; a fixture or a dict has none). Never serialised: it is valid only in this process, and
    # only as long as the app keeps the element. desktop_helpers' AXPress fallback re-validates one before use.
    _refs: dict[str, Any] = field(default_factory=dict, compare=False, repr=False)
    # Fresh, non-editable AXStaticText values whose descriptive label can differ from their
    # displayed text. Transport-only like _refs: the original fixture/dict schema and
    # candidate IDs stay unchanged. Decisions includes these readings in its own evidence.
    # Decisions compares these explicitly when admitting a click. Exclude transport-only
    # readings from Snapshot's archived-schema equality, just as with live element refs.
    _readouts: tuple[tuple[str, str], ...] = field(default=(), compare=False, repr=False)

    def ref(self, cid: str) -> Any:
        return self._refs.get(str(cid))

    def get(self, cid: str) -> Optional[Candidate]:
        if not self._by_id and self.elements:
            self._by_id.update({c.id: c for c in self.elements})
        return self._by_id.get(str(cid))

    def to_dict(self) -> dict[str, Any]:
        return {"seq": self.seq, "app": self.app, "bundle": self.bundle, "title": self.title, "pid": self.pid,
                "elements": [c.to_dict() for c in self.elements], "context_lines": list(self.context_lines),
                "focused": self.focused.to_dict() if self.focused else None, "dialog_text": self.dialog_text,
                "fullscreen": self.fullscreen, "node_count": self.node_count, "truncated": self.truncated,
                "secs": self.secs}


def snapshot_from_dict(d: dict[str, Any]) -> Snapshot:
    focused = d.get("focused")
    return Snapshot(seq=int(d.get("seq", 0)), app=str(d.get("app", "")), bundle=str(d.get("bundle", "")),
                    title=str(d.get("title", "")), pid=int(d.get("pid", 0)),
                    elements=tuple(Candidate.from_dict(c) for c in d.get("elements", ())),
                    context_lines=tuple(str(s) for s in d.get("context_lines", ())),
                    focused=Candidate.from_dict(focused) if focused else None,
                    dialog_text=str(d.get("dialog_text", "")), fullscreen=bool(d.get("fullscreen")),
                    node_count=int(d.get("node_count", 0)), truncated=bool(d.get("truncated")),
                    secs=float(d.get("secs", 0.0)))


def is_pressable(c: Candidate) -> bool:
    return "AXPress" in c.actions or c.role in PRESSABLE_ROLES


def pressable(snapshot: Snapshot) -> list[Candidate]:
    """The labelled, enabled controls the lane could click (before the rank drops)."""
    return [c for c in snapshot.elements if is_pressable(c) and c.label and c.enabled]


# ---- deriving a snapshot from a raw walk -------------------------------------------------

def _text(v: Any) -> str:
    if v is None or isinstance(v, bool):
        return ""
    if isinstance(v, (int, float)):
        return str(v)
    return _SPACES.sub(" ", str(v)).strip()


def _role_name(role: str, subrole: str) -> str:
    if subrole == "AXTabButton" or role == "AXTab":
        return "tab"
    if subrole == "AXSearchField":
        return "search field"
    if subrole in ("AXToggle", "AXSwitch"):
        return "toggle"
    if role in ROLE_NAMES:
        return ROLE_NAMES[role]
    return role[2:].casefold() if role.startswith("AX") and len(role) > 2 else (role.casefold() or "element")


def _descendant_text(node: dict[str, Any], depth: int = 4) -> str:
    """The first text under a node, in tree order (rows, cells, links and web buttons keep
    their label in a child AXStaticText; a Finder list row keeps the file name in an
    AXTextField). An image, button or link title is only the fallback: a Music sidebar row
    is [AXImage "clock", AXStaticText "Recently Added"] and must read "Recently Added"."""
    stack = [(child, 1) for child in reversed(node.get("children") or [])]
    fallback = ""
    while stack:
        n, d = stack.pop()
        if n.get("role") == "AXSecureTextField" or n.get("subrole") == "AXSecureTextField":
            continue
        role = n.get("role")
        if role == "AXStaticText":
            text = _text(n.get("value")) or _text(n.get("title")) or _text(n.get("description"))
            if text:
                return text
        elif role == "AXTextField":
            text = _text(n.get("value"))
            if text:
                return text
        elif d < depth and not fallback and role in ("AXImage", "AXButton", "AXLink"):
            fallback = _text(n.get("title")) or _text(n.get("description"))
        if d < depth:
            stack.extend((c, d + 1) for c in reversed(n.get("children") or []))
    return fallback


def _value_text(role: str, raw: Any) -> str:
    if role in ("radio button", "tab"):
        return "selected" if raw in (1, True, "1") else ""
    if role in ("checkbox", "toggle"):
        if raw in (1, True, "1"):
            return "checked"
        return "unchecked" if raw in (0, False, "0") else ""
    if role == "disclosure triangle":
        return "expanded" if raw in (1, True, "1") else "collapsed"
    if role == "static text":
        return ""
    text = _text(raw)
    return text[:MAX_VALUE_CHARS - 1] + "…" if len(text) > MAX_VALUE_CHARS else text


def _frame(raw: Any) -> Optional[tuple[int, int, int, int]]:
    if not raw or len(raw) != 4:
        return None
    try:
        x, y, w, h = (int(round(float(v))) for v in raw)
    except (TypeError, ValueError, OverflowError):   # System Settings reports an infinite frame on some rows
        return None
    if w <= 0 or h <= 0:
        return None
    return (x, y, w, h)


def _on_screen(frame: tuple[int, int, int, int], screen: Optional[tuple[int, int]]) -> bool:
    if screen is None:
        return True
    x, y, w, h = frame
    sw, sh = screen
    return x + w > 0 and y + h > 0 and x < sw and y < sh


def snapshot_from_raw(raw: dict[str, Any], *, seq: int = 0, screen: Optional[tuple[int, int]] = None,
                      refs: Optional[dict[int, Any]] = None) -> Snapshot:
    """Derive the Snapshot from a raw walk dump. Pure and deterministic: the same dump gives
    an equal Snapshot every time (tests/test_fixtures_ax.py relies on this). `refs` is ax_walk's
    {id(node): element} for this same dump; each candidate's element is kept as Snapshot._refs."""
    app = _text(raw.get("app"))
    bundle = _text(raw.get("bundle"))
    pid = int(raw.get("pid") or 0)
    root = raw.get("root")
    title = _text(raw.get("title")) or (_text(root.get("title")) if root else "")
    base = dict(seq=seq, app=app, bundle=bundle, title=title, pid=pid, fullscreen=bool(raw.get("fullscreen")),
                node_count=int(raw.get("node_count") or 0), truncated=bool(raw.get("truncated")),
                secs=round(float(raw.get("secs") or 0.0), 4))
    if not root:
        lines = (f"title: {title}",) if title else ()
        return Snapshot(elements=(), context_lines=lines, focused=None, dialog_text="", **base)
    window_dialog = _text(root.get("subrole")) in DIALOG_SUBROLES
    found: list[tuple[tuple[int, int, int], dict[str, Any], bool, int, str]] = []
    dialog_text = ""
    stack: list[tuple[dict[str, Any], tuple[str, ...], str, bool]] = [(root, (), "", False)]
    order = 0
    while stack:
        node, ancestors, row_label, secure_parent = stack.pop()
        ax_role = _text(node.get("role"))
        subrole = _text(node.get("subrole"))
        role = _role_name(ax_role, subrole)
        actions = tuple(str(a) for a in (node.get("actions") or ()))
        in_sheet = "AXSheet" in ancestors or ax_role == "AXSheet"
        in_content = any(a in CONTENT_ROLES for a in ancestors)
        label = _text(node.get("title")) or _text(node.get("description"))
        if not label and role == "static text":
            label = _text(node.get("value"))
        if not label and ax_role != "AXWindow" and ax_role not in CONTAINER_ROLES:
            label = _descendant_text(node)
        if role == "static text" and label and (in_sheet or window_dialog) and not dialog_text:
            dialog_text = label[:MAX_DIALOG_CHARS]
        frame = _frame(node.get("frame"))
        value = _value_text(role, node.get("value"))
        editable = role in EDITABLE_ROLES
        secure = secure_parent or ax_role == "AXSecureTextField" or subrole == "AXSecureTextField"
        # A cell that only repeats its row's label is the same control twice (System Settings
        # sidebar: 39 AXRow/AXCell pairs); the row keeps the slot.
        repeats_row = role == "cell" and bool(label) and label == row_label
        is_candidate = (ax_role != "AXWindow" and frame is not None and _on_screen(frame, screen)
                        and (label or value or editable) and not repeats_row
                        and (role in ROLE_NAMES.values() or "AXPress" in actions))
        if is_candidate:
            assert frame is not None
            fields = dict(role=role, label=label, value=value, frame=frame, actions=actions,
                          enabled=bool(node.get("enabled", True)), app=app, in_content=in_content,
                          in_dialog=window_dialog or in_sheet, secure=secure, editable=editable)
            # Static text nested in an editable field can be its user's draft, not a
            # separate result display. It must not become a new model value channel.
            under_editable = any(a in ("AXTextField", "AXSearchField", "AXTextArea", "AXComboBox")
                                 for a in ancestors)
            readout = (_value_text("readout", node.get("value"))
                       if role == "static text" and not secure and not under_editable else "")
            found.append(((frame[1], frame[0], order), fields, bool(node.get("focused")), id(node), readout))
            order += 1
        if ax_role in SKIP_DESCEND_ROLES:
            continue
        below = ancestors + (ax_role,)
        inner_row = label if role == "row" else row_label
        stack.extend((child, below, inner_row, secure) for child in reversed(node.get("children") or []))
    found.sort(key=lambda item: item[0])
    elements: list[Candidate] = []
    focused: Optional[Candidate] = None
    kept_refs: dict[str, Any] = {}
    readouts: list[tuple[str, str]] = []
    for index, (_key, fields, is_focused, node_id, readout) in enumerate(found):
        c = Candidate(id=str(index), **fields)
        elements.append(c)
        if refs and refs.get(node_id) is not None:
            kept_refs[c.id] = refs[node_id]
        if readout:
            readouts.append((c.id, readout))
        if is_focused and focused is None:
            focused = c
    lines: list[str] = []
    if title:
        lines.append(f"title: {title}")
    if focused is not None and not focused.secure:
        lines.append(f"focused: {focused.role} {focused.label}".rstrip())
    seen = {title.casefold()} if title else set()
    for c in elements:
        if len(lines) >= MAX_CONTEXT_LINES or len(lines) >= MAX_CONTEXT_TEXTS + 2:
            break
        if c.secure or c.role != "static text" or not c.label or len(c.label) > 60 or c.label.casefold() in seen:
            continue
        seen.add(c.label.casefold())
        lines.append(c.label)
    return Snapshot(elements=tuple(elements), context_lines=tuple(lines[:MAX_CONTEXT_LINES]),
                    focused=focused, dialog_text=dialog_text, _refs=kept_refs, _readouts=tuple(readouts), **base)


def ax_snapshot(pid: Optional[int] = None, *, max_nodes: int = DEFAULT_MAX_NODES,
                budget_secs: float = DEFAULT_BUDGET_SECS, walk: Optional[Callable[..., dict[str, Any]]] = None,
                screen: Optional[tuple[int, int]] = None) -> Snapshot:
    """One snapshot of an app's focused window (the frontmost app when pid is None).

    `walk(pid, *, max_nodes, budget_secs)` is the injectable backend (raw shape in the
    module docstring); the default is the real AX walker. Each call gets a fresh `seq`.
    """
    global _SEQ
    if pid is None:
        from .desktop_helpers import _focused_pid

        pid = _focused_pid()
    refs: Optional[dict[int, Any]] = None
    if walk is None:
        refs = {}
        raw = ax_walk(int(pid or 0), max_nodes=max_nodes, budget_secs=budget_secs, refs=refs)
    else:
        raw = walk(int(pid or 0), max_nodes=max_nodes, budget_secs=budget_secs)
    _SEQ += 1
    return snapshot_from_raw(raw, seq=_SEQ, screen=screen, refs=refs)


# ---- the menu -------------------------------------------------------------------------

def objective_tokens(objective: str) -> list[str]:
    """Casefolded words of ≥ 3 characters minus stop-words, in order, without duplicates."""
    out: list[str] = []
    for tok in re.findall(r"[a-z0-9]+", objective.casefold()):
        if len(tok) >= 3 and tok not in STOP_WORDS and tok not in out:
            out.append(tok)
    return out


def score_candidate(c: Candidate, tokens: list[str]) -> int:
    hay = f"{c.label} {c.value}".casefold()
    return ROLE_PRIOR.get(c.role, 0) + 4 * sum(1 for t in tokens if t in hay)


def rank_candidates(snapshot: Snapshot, objective: str, *, max_out: int, kinds: str = "pressable",
                    allow_page_links: bool = False, allow_sensitive: bool = False) -> tuple[list[Candidate], int]:
    """(kept, dropped_by_cap). Drops before scoring: disabled, empty label, in_dialog, secure,
    in_content links and images unless allow_page_links, is_sensitive(label) unless
    allow_sensitive. `kinds` is "pressable" or "editable". Stable by score, then reading order."""
    if kinds not in ("pressable", "editable"):
        raise ValueError("kinds must be 'pressable' or 'editable'")
    tokens = objective_tokens(objective)
    pool: list[Candidate] = []
    for c in snapshot.elements:
        if kinds == "pressable" and not is_pressable(c):
            continue
        if kinds == "editable" and not c.editable:
            continue
        if not c.enabled or not c.label.strip() or c.in_dialog or c.secure:
            continue
        if c.in_content and not allow_page_links and c.role in ("link", "image", "static text"):
            continue
        if not allow_sensitive and is_sensitive(c.label):
            continue
        pool.append(c)
    pool.sort(key=lambda c: -score_candidate(c, tokens))       # stable: reading order breaks ties
    cap = max(0, int(max_out))
    return pool[:cap], max(0, len(pool) - cap)


def describe(c: Candidate, *, verb: str, max_chars: int = 40) -> str:
    """"click radio button: Week (selected)" — URLs stripped, whitespace collapsed, label capped."""
    label = _SPACES.sub(" ", _URL.sub("", c.label)).strip()
    if len(label) > max_chars:
        label = label[:max(1, max_chars - 1)].rstrip() + "…"
    text = f"{verb} {c.role}: {label}".rstrip(": ")
    if c.value:
        value = c.value if len(c.value) <= 24 else c.value[:23] + "…"
        text += f" ({value})"
    return text


def context_summary(snapshot: Snapshot, *, max_chars: int = 300) -> str:
    """One line for the decider's state: app, title, focused control, a few static texts."""
    parts = [f"app: {snapshot.app}"] if snapshot.app else []
    texts: list[str] = []
    for line in snapshot.context_lines:
        if line.startswith(("title: ", "focused: ")):
            parts.append(line)
        else:
            texts.append(line)
    if snapshot.dialog_text:
        parts.append(f"dialog: {snapshot.dialog_text}")
    if texts:
        parts.append("text: " + " | ".join(texts))
    out = "; ".join(parts)
    return out if len(out) <= max_chars else out[:max(1, max_chars - 1)].rstrip() + "…"


# ---- the real walker (macOS) ------------------------------------------------------------

def _app_identity(pid: int) -> tuple[str, str]:
    try:
        from AppKit import NSRunningApplication

        app = NSRunningApplication.runningApplicationWithProcessIdentifier_(int(pid))
        if app is not None:
            return str(app.localizedName() or ""), str(app.bundleIdentifier() or "")
    except Exception:  # noqa: BLE001 — a nameless app is still walkable
        pass
    return "", ""


def pid_for_app(name: str) -> int:
    """The pid of a running app by its localized name (case-insensitive), 0 when not running."""
    from AppKit import NSWorkspace

    for app in NSWorkspace.sharedWorkspace().runningApplications():
        if str(app.localizedName() or "").casefold() == name.casefold():
            return int(app.processIdentifier())
    return 0


def main_screen_points() -> Optional[tuple[int, int]]:
    try:
        import Quartz

        bounds = Quartz.CGDisplayBounds(Quartz.CGMainDisplayID())
        return (int(bounds.size.width), int(bounds.size.height))
    except Exception:  # noqa: BLE001 — no display means no viewport filter
        return None


def ax_walk(pid: int, *, max_nodes: int = DEFAULT_MAX_NODES, budget_secs: float = DEFAULT_BUDGET_SECS,
            content_first: bool = CONTENT_FIRST, clock: Callable[[], float] = time.perf_counter,
            refs: Optional[dict[int, Any]] = None) -> dict[str, Any]:
    """The raw accessibility tree of an app's focused window (shape in the module docstring).

    `refs`, when given, is filled with {id(node): AXUIElement} for every node of the dump, so
    snapshot_from_raw can keep each candidate's live element (the dump itself stays JSON).

    `content_first` orders siblings so AXWebArea / AXScrollArea / AXContentList subtrees are
    walked before the window chrome (the plan's default); False walks chrome first, so a
    truncated walk of a long page still carries the toolbar the lane can offer.
    """
    from ApplicationServices import (
        AXUIElementCopyActionNames,
        AXUIElementCopyAttributeValue,
        AXUIElementCopyMultipleAttributeValues,
        AXUIElementCreateApplication,
        AXValueGetType,
        AXValueGetValue,
        kAXValueAXErrorType,
        kAXValueCGPointType,
        kAXValueCGSizeType,
    )

    t0 = clock()
    name, bundle = _app_identity(pid)
    raw: dict[str, Any] = {"app": name, "bundle": bundle, "pid": int(pid), "title": "", "fullscreen": False,
                           "node_count": 0, "truncated": False, "secs": 0.0, "root": None}
    if not pid:
        raw["secs"] = round(clock() - t0, 4)
        return raw
    app_el = AXUIElementCreateApplication(int(pid))
    err, window = AXUIElementCopyAttributeValue(app_el, "AXFocusedWindow", None)
    if err != 0 or window is None:
        err, windows = AXUIElementCopyAttributeValue(app_el, "AXWindows", None)
        window = windows[0] if err == 0 and windows else None
    if window is None:
        raw["secs"] = round(clock() - t0, 4)
        return raw

    import objc

    ns_array = objc.lookUpClass("NSArray")
    ns_attributed = objc.lookUpClass("NSAttributedString")

    def scalar(v: Any) -> Any:
        # isinstance against looked-up classes, never hasattr: pyobjc's hasattr on a bridged
        # object costs ~0.2 ms a call (1.4 s of a 2.8 s Safari walk, profiled 2026-09-21).
        if v is None or isinstance(v, (bool, int, float, str)):
            return v
        kind = AXValueGetType(v)                       # 0 for anything that is not an AXValue
        if kind == kAXValueAXErrorType:
            return None
        if kind == kAXValueCGPointType:
            ok, pt = AXValueGetValue(v, kAXValueCGPointType, None)
            return (float(pt.x), float(pt.y)) if ok else None
        if kind == kAXValueCGSizeType:
            ok, sz = AXValueGetValue(v, kAXValueCGSizeType, None)
            return (float(sz.width), float(sz.height)) if ok else None
        if kind:
            return None                                  # ranges, rects: not useful here
        if isinstance(v, (list, tuple, ns_array)):
            return list(v)
        if isinstance(v, ns_attributed):
            return str(v.string())[:200]
        return None

    def fetch(el: Any, ancestors: list[str]) -> Optional[tuple[dict[str, Any], list[Any]]]:
        err, values = AXUIElementCopyMultipleAttributeValues(el, ATTRIBUTES, 0, None)
        if err != 0 or values is None or len(values) != len(ATTRIBUTES):
            return None
        role, subrole, title, desc, value, pos, size, enabled, focused, children = (scalar(v) for v in values)
        role_s = str(role or "")
        frame = None
        if isinstance(pos, tuple) and isinstance(size, tuple):
            frame = [pos[0], pos[1], size[0], size[1]]
        text_value: Any = value
        if isinstance(value, str):
            text_value = value[:200]
        elif not isinstance(value, (bool, int, float)) and value is not None:
            text_value = None
        actions: list[str] = []
        # One extra IPC only where it can change the answer: a node whose role does not already
        # make it pressable (Calendar events are AXStaticText with AXPress), outside page content
        # (web action queries cost ~0.45 ms a node and page text is never offered) and not a container.
        if (role_s not in CONTAINER_ROLES and _role_name(role_s, str(subrole or "")) not in PRESSABLE_ROLES
                and not any(a in CONTENT_ROLES for a in ancestors)):
            aerr, names = AXUIElementCopyActionNames(el, None)
            if aerr == 0 and names:
                actions = [str(n) for n in names]
        node = {"role": role_s, "subrole": str(subrole or ""), "title": str(title or "")[:200],
                "description": str(desc or "")[:200], "value": text_value, "frame": frame, "actions": actions,
                "enabled": True if enabled is None else bool(enabled), "focused": bool(focused),
                "children": [], "ancestor_roles": list(ancestors)}
        kids = list(children) if isinstance(children, list) else []
        if refs is not None:
            refs[id(node)] = el
        return node, kids

    got = fetch(window, [])
    count = 1
    truncated = False
    if got is None:
        raw["secs"] = round(clock() - t0, 4)
        return raw
    root, root_kids = got
    ferr, full = AXUIElementCopyAttributeValue(window, "AXFullScreen", None)
    raw["fullscreen"] = bool(full) if ferr == 0 and isinstance(full, (bool, int)) else False
    raw["title"] = root["title"]
    stack: list[tuple[dict[str, Any], list[Any]]] = [(root, root_kids)]
    while stack and not truncated:
        node, kids = stack.pop()
        if node["role"] in SKIP_DESCEND_ROLES:
            continue
        below = node["ancestor_roles"] + [node["role"]]
        fetched: list[tuple[dict[str, Any], list[Any]]] = []
        for kid in kids:
            if count >= max_nodes or clock() - t0 >= budget_secs:
                truncated = True
                break
            rec = fetch(kid, below)
            count += 1
            if rec is not None:
                fetched.append(rec)
        # Sibling order decides what a truncated walk keeps: content subtrees first (the plan's
        # default, the page survives) or chrome first (the toolbar survives).
        fetched.sort(key=lambda rec: 0 if (rec[0]["role"] in CONTENT_FIRST_ROLES) == content_first else 1)
        node["children"] = [rec[0] for rec in fetched]
        stack.extend(reversed(fetched))
    raw["root"] = root
    raw["node_count"] = count
    raw["truncated"] = truncated
    raw["secs"] = round(clock() - t0, 4)
    return raw


# ---- CLI: capture a raw walk for the fixtures -----------------------------------------------

def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m eve_bridge.ax_candidates",
                                     description="Walk one app's focused window and print its candidates.")
    parser.add_argument("--app", help="the app's name as the menu bar shows it (default: the frontmost app)")
    parser.add_argument("--pid", type=int, help="the app's pid (overrides --app)")
    parser.add_argument("--max-nodes", type=int, default=DEFAULT_MAX_NODES)
    parser.add_argument("--budget-secs", type=float, default=DEFAULT_BUDGET_SECS)
    parser.add_argument("--objective", default="", help="rank the menu for this objective (table output)")
    parser.add_argument("--json", action="store_true", help="print {raw, snapshot, candidates} as JSON")
    args = parser.parse_args(argv)
    if args.pid:
        pid = int(args.pid)
    elif args.app:
        pid = pid_for_app(args.app)
        if not pid:
            print(f"no running app called {args.app!r}", file=sys.stderr)
            return 2
    else:
        from .desktop_helpers import _focused_pid

        pid = _focused_pid()
    screen = main_screen_points()
    raw = ax_walk(pid, max_nodes=args.max_nodes, budget_secs=args.budget_secs)
    snap = snapshot_from_raw(raw, seq=1, screen=screen)
    kept, dropped = rank_candidates(snap, args.objective, max_out=12)
    if args.json:
        json.dump({"raw": raw, "snapshot": snap.to_dict(), "screen": list(screen) if screen else None,
                   "candidates": [describe(c, verb="click") for c in kept]}, sys.stdout, ensure_ascii=False)
        sys.stdout.write("\n")
        return 0
    print(f"{snap.app} ({snap.bundle}) pid {snap.pid} — {snap.title!r}")
    print(f"nodes {snap.node_count}  elements {len(snap.elements)}  pressable {len(pressable(snap))}  "
          f"truncated {snap.truncated}  {snap.secs * 1000:.0f} ms  fullscreen {snap.fullscreen}")
    print(f"context: {context_summary(snap)}")
    print(f"menu for {args.objective!r} ({len(kept)} shown, {dropped} dropped by cap):")
    for c in kept:
        flags = "".join(f for f, on in (("C", c.in_content), ("D", c.in_dialog), ("S", c.secure),
                                        ("E", c.editable), ("!", is_sensitive(c.label))) if on)
        print(f"  {c.id:>4}  {describe(c, verb='click'):<52} {c.frame} {flags}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
