"""Desktop notes widget: the robot's idle-explorer observations on the Mac desktop.

The idle explorer writes one line per observation to
``~/.config/eve-bridge/notes/YYYY-MM-DD.md``::

    - HH:MM yaw=+20 pitch=40 — <sentence>

This module shows the newest of those lines in a borderless, non-activating
panel that sits just above the desktop icons — a wallboard, not an app. It has
no Dock icon and no menu-bar entry; right-click gives "Open notes folder" and
"Quit". It is dragged by its background and remembers where it was put in
``~/.config/eve-bridge/widget.json``.

The module splits into two halves so the file logic is testable without a
GUI:

* pure helpers — note file discovery, parsing, ordering, rendering, position
  persistence. No AppKit import; safe on any platform.
* :func:`run` — the AppKit panel. Imports Cocoa lazily and refuses to start
  anywhere but macOS.

Refresh: a ``watchfiles`` thread watches the notes directory and a 30 s
``NSTimer`` polls as a backstop (a file written by ``open(..., "a")`` from
another process can arrive without a FSEvents callback in edge cases). Both
paths land on the main thread via ``performSelectorOnMainThread``.
"""

from __future__ import annotations

import json
import logging
import os
import re
import sys
import threading
from dataclasses import dataclass
from datetime import date, timedelta
from pathlib import Path
from typing import Optional, Sequence

log = logging.getLogger(__name__)

NOTES_DIR_ENV = "EVE_NOTES_DIR"
MEMORY_DIR_ENV = "EVE_MEMORY_DIR"
CONFIG_DIR = Path.home() / ".config" / "eve-bridge"
DEFAULT_NOTES_DIR = CONFIG_DIR / "notes"
DEFAULT_MEMORY_DIR = CONFIG_DIR / "memory"
POSITION_PATH = CONFIG_DIR / "widget.json"

TITLE = "eve"
EMPTY_TEXT = ("Nothing yet. eve writes here after it has looked around, and after you have talked to it.\n\n"
              "Say \u201chey eve\u201d to talk. Say \u201cremember that\u201d to keep something for good.")
# The two provenances, kept apart on purpose: what eve heard you say is
# quotable, what it thinks it saw is not. The widget labels them rather than
# mixing them into one list (see recall.py).
SAID = "said"
SEEN = "seen"
KIND_LABELS = {SAID: "Talking", SEEN: "Looking"}
STARS_LABEL = "Remembered for good"
MAX_STARS = 8
NO_TIME = "--:--"                 # a journal line: the day's page, not a moment
FORGOTTEN = "(forgotten)"         # what forget leaves in a journal: never shown
MAX_NOTES = 40
WIDGET_SIZE = (360.0, 420.0)
SCREEN_MARGIN = 24.0
POLL_SECS = 30.0
FONT_SIZE = 13.0

# ``- HH:MM <rest>``. The rest is shown verbatim (yaw/pitch prefix included) —
# the explorer decides what a note looks like, the widget only lists them.
LINE_RE = re.compile(r"^- (\d{1,2}:\d{2}) (.+?)\s*$")
FILE_RE = re.compile(r"^(\d{4})-(\d{2})-(\d{2})\.md$")


@dataclass(frozen=True)
class Note:
    day: date
    time: str
    text: str
    kind: str = SEEN          # SEEN: an observation. SAID: from a conversation.


# ---------------------------------------------------------------------------
# Pure helpers
# ---------------------------------------------------------------------------


def notes_dir() -> Path:
    """Where the explorer writes. ``EVE_NOTES_DIR`` overrides for tests/smoke runs."""
    raw = os.environ.get(NOTES_DIR_ENV)
    return Path(raw).expanduser() if raw else DEFAULT_NOTES_DIR


def memory_dir() -> Path:
    """eve's memory folder (recall.py). ``EVE_MEMORY_DIR`` overrides."""
    raw = os.environ.get(MEMORY_DIR_ENV)
    return Path(raw).expanduser() if raw else DEFAULT_MEMORY_DIR


def _recall_cfg(store: Path):
    from .recall import RecallConfig

    return RecallConfig(store=store, notes=notes_dir())


def stars(store: Path, limit: int = MAX_STARS) -> list[str]:
    """The claims the owner promoted by saying "remember that" (records/starred.md). Newest last."""
    try:
        from . import records

        return records.stars(_recall_cfg(store), limit)
    except Exception:  # noqa: BLE001 - a widget shows less, it never breaks
        return []


def parse_conversation(text: str) -> list[str]:
    """A dream journal (records/days/<day>.md) as the one or two lines worth showing.

    The title says what it was about; a debt of eve's own is the line the owner
    most wants to see, because it is the thing eve has not done yet.
    """
    title = ""
    owes: list[str] = []
    for line in text.splitlines():
        stripped = line.strip()
        if not title and stripped.startswith("# "):
            title = stripped[2:].strip()
        elif stripped.startswith("- ") and stripped[2:].lower().startswith("eve owes"):
            owes.append(stripped[2:].strip())
    out = [title] if title and FORGOTTEN not in title else []
    out.extend(o for o in owes[:2] if FORGOTTEN not in o)
    return out


def journal_dir(store: Path) -> Path:
    """The dream journal: one file per day eve talked, written the night after (dream.py)."""
    return store / "records" / "days"


def collect_conversations(store: Path, today: Optional[date] = None,
                          limit: int = MAX_NOTES) -> list[Note]:
    """What was said, from the dream journals of today and yesterday, newest day first. A journal has no
    clock time: it is one page for the whole day, written the night after."""
    today = today or date.today()
    out: list[Note] = []
    for day in (today, today - timedelta(days=1)):
        path = journal_dir(store) / f"{day.isoformat()}.md"
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for line in parse_conversation(text):
            out.append(Note(day=day, time=NO_TIME, text=line, kind=SAID))
            if len(out) >= limit:
                return out
    return out


def note_files(directory: Path, today: Optional[date] = None) -> list[Path]:
    """Today's and yesterday's note files that exist, newest day first."""
    today = today or date.today()
    out: list[Path] = []
    for day in (today, today - timedelta(days=1)):
        path = directory / f"{day.isoformat()}.md"
        if path.is_file():
            out.append(path)
    return out


def day_of(path: Path) -> Optional[date]:
    m = FILE_RE.match(path.name)
    if not m:
        return None
    try:
        return date(int(m.group(1)), int(m.group(2)), int(m.group(3)))
    except ValueError:
        return None


def parse_notes(text: str, day: date) -> list[Note]:
    """Note lines in file order (oldest first). Non-matching lines are skipped."""
    notes: list[Note] = []
    for line in text.splitlines():
        m = LINE_RE.match(line)
        if m:
            notes.append(Note(day=day, time=m.group(1), text=m.group(2)))
    return notes


def collect_notes(directory: Path, today: Optional[date] = None,
                  limit: int = MAX_NOTES) -> list[Note]:
    """Newest ``limit`` notes across today + yesterday, newest first."""
    out: list[Note] = []
    for path in note_files(directory, today):
        day = day_of(path)
        if day is None:
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError as e:
            log.warning("notes-widget: cannot read %s: %s", path, e)
            continue
        out.extend(reversed(parse_notes(text, day)))
        if len(out) >= limit:
            break
    return out[:limit]


def day_label(day: date, today: Optional[date] = None) -> str:
    today = today or date.today()
    if day == today:
        return f"Today · {day.isoformat()}"
    if day == today - timedelta(days=1):
        return f"Yesterday · {day.isoformat()}"
    return day.isoformat()


def render_sections(notes: Sequence[Note], today: Optional[date] = None) -> list[tuple[str, list[Note]]]:
    """Group newest-first notes by day, and within a day by provenance.

    A day that holds both kinds gets a sub-header for each, because what eve
    heard and what eve saw are different kinds of claim and the widget must not
    let them read as one list.
    """
    sections: list[tuple[str, list[Note]]] = []
    by_day: list[tuple[str, list[Note]]] = []
    for note in notes:
        label = day_label(note.day, today)
        if not by_day or by_day[-1][0] != label:
            by_day.append((label, []))
        by_day[-1][1].append(note)
    for label, group in by_day:
        kinds = [k for k in (SAID, SEEN) if any(n.kind == k for n in group)]
        if len(kinds) < 2:
            sections.append((label, group))
            continue
        for i, kind in enumerate(kinds):
            head = f"{label} · {KIND_LABELS[kind]}" if i == 0 else KIND_LABELS[kind]
            sections.append((head, [n for n in group if n.kind == kind]))
    return sections


def render_text(notes: Sequence[Note], today: Optional[date] = None,
                starred: Sequence[str] = ()) -> str:
    """Plain-text rendering — what ``--once`` prints and what the panel shows."""
    if not notes and not starred:
        return EMPTY_TEXT
    lines: list[str] = []
    if starred:
        lines.append(STARS_LABEL)
        lines.extend(f"\u2605  {claim}" for claim in reversed(list(starred)))
    for label, group in render_sections(notes, today):
        if lines:
            lines.append("")
        lines.append(label)
        lines.extend(f"{n.time}  {n.text}" for n in group)
    return "\n".join(lines)


def collect_everything(notes_directory: Path, store: Path, today: Optional[date] = None,
                       limit: int = MAX_NOTES) -> tuple[list[Note], list[str]]:
    """Everything eve remembers of the last two days, plus the starred layer.

    The widget is the interface for all of it (owner instruction 2026-09-11), so
    this is the one call that gathers it. Either source being absent is normal.
    """
    said = collect_conversations(store, today, limit)
    seen = collect_notes(notes_directory, today, limit)
    both = sorted(said + seen, key=lambda n: (n.day, n.time), reverse=True)[:limit]
    return both, stars(store)


def load_position(path: Path = POSITION_PATH) -> Optional[tuple[float, float]]:
    """Saved bottom-left origin in screen points, or None when absent/invalid."""
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        return float(data["x"]), float(data["y"])
    except (OSError, ValueError, KeyError, TypeError):
        return None


def save_position(x: float, y: float, path: Path = POSITION_PATH) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps({"x": round(x, 1), "y": round(y, 1)}), encoding="utf-8")
    os.replace(tmp, path)


Rect = tuple[float, float, float, float]  # x, y, w, h — AppKit bottom-left origin


def default_position(screen: Rect, size: tuple[float, float] = WIDGET_SIZE,
                     margin: float = SCREEN_MARGIN) -> tuple[float, float]:
    """Top-right corner of ``screen`` (the visible frame), inset by ``margin``."""
    sx, sy, sw, sh = screen
    w, h = size
    return (sx + sw - w - margin, sy + sh - h - margin)


def position_on_screens(pos: Optional[tuple[float, float]], screens: Sequence[Rect],
                        size: tuple[float, float] = WIDGET_SIZE) -> bool:
    """True when the widget's centre lands on some screen. A saved position
    from an unplugged external display must not strand the widget off-screen."""
    if pos is None:
        return False
    cx, cy = pos[0] + size[0] / 2, pos[1] + size[1] / 2
    return any(sx <= cx <= sx + sw and sy <= cy <= sy + sh for sx, sy, sw, sh in screens)


# ---------------------------------------------------------------------------
# AppKit panel
# ---------------------------------------------------------------------------


def run(once: bool = False, directory: Optional[Path] = None,
        position_path: Path = POSITION_PATH, store: Optional[Path] = None) -> int:
    """Show the widget. Blocks in ``NSApp.run()`` unless ``once``.

    ``once`` builds the panel, spins the run loop for one second so the window
    is actually created and drawn, prints what it rendered, and exits — a
    smoke test for non-interactive runs.
    """
    if sys.platform != "darwin":
        print("eve-bridge: notes-widget is macOS-only (AppKit)", file=sys.stderr)
        return 2
    try:
        import AppKit
        import Foundation
        import objc
        import Quartz
    except ImportError as e:
        print(f"eve-bridge: notes-widget needs pyobjc-framework-Cocoa "
              f"(pip install pyobjc-framework-Cocoa): {e}", file=sys.stderr)
        return 2

    directory = directory or notes_dir()
    store = store or memory_dir()
    directory.mkdir(parents=True, exist_ok=True)

    level = Quartz.kCGDesktopIconWindowLevel + 1
    behaviour = (AppKit.NSWindowCollectionBehaviorCanJoinAllSpaces
                 | AppKit.NSWindowCollectionBehaviorStationary
                 | AppKit.NSWindowCollectionBehaviorIgnoresCycle)

    class NotesTextView(AppKit.NSTextView):
        """Read-only text view whose right-click menu is ours, not the editor's."""

        def menuForEvent_(self, event):  # noqa: N802 — ObjC selector
            return self.window().contentView().menu()

    class Controller(AppKit.NSObject):
        panel = objc.ivar()
        text_view = objc.ivar()
        last_render = objc.ivar()

        def build(self):
            w, h = WIDGET_SIZE
            def as_rect(frame) -> Rect:
                return (frame.origin.x, frame.origin.y, frame.size.width, frame.size.height)

            screens = [as_rect(s.visibleFrame()) for s in AppKit.NSScreen.screens()]
            main_rect = as_rect(AppKit.NSScreen.mainScreen().visibleFrame())
            saved = load_position(position_path)
            pos = saved if position_on_screens(saved, screens) else default_position(main_rect)

            rect = Foundation.NSMakeRect(pos[0], pos[1], w, h)
            style = AppKit.NSWindowStyleMaskBorderless | AppKit.NSWindowStyleMaskNonactivatingPanel
            panel = AppKit.NSPanel.alloc().initWithContentRect_styleMask_backing_defer_(
                rect, style, AppKit.NSBackingStoreBuffered, False)
            panel.setLevel_(level)
            panel.setCollectionBehavior_(behaviour)
            panel.setOpaque_(False)
            panel.setBackgroundColor_(AppKit.NSColor.clearColor())
            panel.setHasShadow_(False)
            panel.setMovableByWindowBackground_(True)
            panel.setHidesOnDeactivate_(False)
            panel.setFloatingPanel_(False)
            panel.setBecomesKeyOnlyIfNeeded_(True)
            panel.setReleasedWhenClosed_(False)
            panel.setDelegate_(self)

            # Background: dark, rounded, 75 % alpha. A plain layer beats
            # NSVisualEffectView here — no vibrancy dependence on what sits
            # behind a desktop-level window.
            content = AppKit.NSView.alloc().initWithFrame_(Foundation.NSMakeRect(0, 0, w, h))
            content.setWantsLayer_(True)
            layer = content.layer()
            layer.setBackgroundColor_(AppKit.NSColor.colorWithCalibratedWhite_alpha_(0.08, 0.75).CGColor())
            layer.setCornerRadius_(14.0)
            layer.setMasksToBounds_(True)
            content.setMenu_(self._context_menu())
            panel.setContentView_(content)

            pad = 16.0
            title = AppKit.NSTextField.labelWithString_(TITLE)
            title.setFont_(AppKit.NSFont.boldSystemFontOfSize_(14.0))
            title.setTextColor_(AppKit.NSColor.colorWithCalibratedWhite_alpha_(1.0, 0.92))
            title.setFrame_(Foundation.NSMakeRect(pad, h - pad - 18, w - 2 * pad, 18))
            title.setAutoresizingMask_(AppKit.NSViewMinYMargin | AppKit.NSViewWidthSizable)
            content.addSubview_(title)

            scroll = AppKit.NSScrollView.alloc().initWithFrame_(
                Foundation.NSMakeRect(pad, pad, w - 2 * pad, h - 3 * pad - 18))
            scroll.setHasVerticalScroller_(True)
            scroll.setAutohidesScrollers_(True)
            scroll.setDrawsBackground_(False)
            scroll.setBorderType_(AppKit.NSNoBorder)
            scroll.setAutoresizingMask_(AppKit.NSViewWidthSizable | AppKit.NSViewHeightSizable)
            scroll.setScrollerKnobStyle_(AppKit.NSScrollerKnobStyleLight)

            tv = NotesTextView.alloc().initWithFrame_(scroll.contentView().bounds())
            tv.setEditable_(False)
            tv.setSelectable_(True)
            tv.setDrawsBackground_(False)
            tv.setRichText_(False)
            tv.setFont_(AppKit.NSFont.systemFontOfSize_(FONT_SIZE))
            tv.setTextColor_(AppKit.NSColor.colorWithCalibratedWhite_alpha_(0.95, 1.0))
            tv.setVerticallyResizable_(True)
            tv.setHorizontallyResizable_(False)
            tv.setAutoresizingMask_(AppKit.NSViewWidthSizable)
            tv.textContainer().setWidthTracksTextView_(True)
            tv.setTextContainerInset_(Foundation.NSMakeSize(0, 2))
            scroll.setDocumentView_(tv)
            content.addSubview_(scroll)

            self.panel = panel
            self.text_view = tv
            self.refresh_(None)
            panel.orderFrontRegardless()
            return self

        def openLearning_(self, sender):  # noqa: N802
            import webbrowser
            port = int(os.environ.get("EVE_LEARNING_PORT", "48766"))
            webbrowser.open(f"http://127.0.0.1:{port}/")

        def _context_menu(self):
            menu = AppKit.NSMenu.alloc().initWithTitle_(TITLE)
            learning_item = menu.addItemWithTitle_action_keyEquivalent_("Open learning dashboard", "openLearning:", "")
            learning_item.setTarget_(self)
            open_item = menu.addItemWithTitle_action_keyEquivalent_("Open what it saw", "openNotes:", "")
            open_item.setTarget_(self)
            said_item = menu.addItemWithTitle_action_keyEquivalent_("Open what was said", "openSaid:", "")
            said_item.setTarget_(self)
            star_item = menu.addItemWithTitle_action_keyEquivalent_(
                "Remembered for good\u2026", "openStars:", "")
            star_item.setTarget_(self)
            menu.addItem_(AppKit.NSMenuItem.separatorItem())
            quit_item = menu.addItemWithTitle_action_keyEquivalent_("Quit", "quit:", "")
            quit_item.setTarget_(self)
            return menu

        def openNotes_(self, sender):  # noqa: N802 — ObjC selector
            directory.mkdir(parents=True, exist_ok=True)
            AppKit.NSWorkspace.sharedWorkspace().openURL_(
                Foundation.NSURL.fileURLWithPath_(str(directory)))

        # Never create anything under the memory folder from here: the daemon's one-time move skips a
        # target that already exists, so a folder made by the widget would strand the owner's records.
        @objc.python_method
        def _open_first(self, *paths):
            for path in paths:
                if path.exists():
                    AppKit.NSWorkspace.sharedWorkspace().openURL_(
                        Foundation.NSURL.fileURLWithPath_(str(path)))
                    return

        def openSaid_(self, sender):  # noqa: N802 — ObjC selector
            self._open_first(journal_dir(store), store)

        def openStars_(self, sender):  # noqa: N802 — ObjC selector
            self._open_first(store / "records" / "starred.md", store / "records", store)

        def quit_(self, sender):  # noqa: N802 — ObjC selector
            AppKit.NSApp.terminate_(None)

        def refresh_(self, sender):  # noqa: N802 — ObjC selector; called on the main thread only
            today = date.today()
            notes, starred = collect_everything(directory, store, today)
            rendered = self._attributed(notes, today, starred)
            plain = render_text(notes, today, starred)
            if plain == self.last_render:
                return
            self.last_render = plain
            storage = self.text_view.textStorage()
            storage.setAttributedString_(rendered)
            self.text_view.scrollPoint_(Foundation.NSMakePoint(0, 0))

        @objc.python_method
        def _attributed(self, notes, today, starred=()):
            body = AppKit.NSFont.systemFontOfSize_(FONT_SIZE)
            head = AppKit.NSFont.boldSystemFontOfSize_(FONT_SIZE - 1)
            light = AppKit.NSColor.colorWithCalibratedWhite_alpha_(0.95, 1.0)
            dim = AppKit.NSColor.colorWithCalibratedWhite_alpha_(0.75, 1.0)
            out = AppKit.NSMutableAttributedString.alloc().init()

            def add(text, font, colour):
                out.appendAttributedString_(AppKit.NSAttributedString.alloc().initWithString_attributes_(
                    text, {AppKit.NSFontAttributeName: font, AppKit.NSForegroundColorAttributeName: colour}))

            if not notes and not starred:
                add(EMPTY_TEXT, body, dim)
                return out
            first = True
            if starred:
                add(STARS_LABEL + "\n", head, dim)
                add("\n".join(f"\u2605  {claim}" for claim in reversed(list(starred))), body, light)
                first = False
            for label, group in render_sections(notes, today):
                add(("" if first else "\n\n") + label + "\n", head, dim)
                first = False
                add("\n".join(f"{n.time}  {n.text}" for n in group), body, light)
            return out

        def windowDidMove_(self, note):  # noqa: N802 — NSWindowDelegate
            origin = self.panel.frame().origin
            try:
                save_position(origin.x, origin.y, position_path)
            except OSError as e:
                log.warning("notes-widget: cannot save position: %s", e)

    app = AppKit.NSApplication.sharedApplication()
    # Prohibited = LSBackgroundOnly: no Dock tile, no menu bar, never
    # activates. A non-activating panel still receives mouse events, which
    # is all dragging and the context menu need.
    app.setActivationPolicy_(AppKit.NSApplicationActivationPolicyProhibited)

    controller = Controller.alloc().init().build()

    # Backstop poll in case the file watcher misses a write.
    AppKit.NSTimer.scheduledTimerWithTimeInterval_target_selector_userInfo_repeats_(
        POLL_SECS, controller, "refresh:", None, True)

    stop = threading.Event()

    def watch() -> None:
        try:
            from watchfiles import watch as wf_watch
            records_dir = store / "records"
            watched = [str(directory)] + ([str(records_dir)] if records_dir.exists() else [])
            for _changes in wf_watch(*watched, stop_event=stop, debounce=300):
                controller.performSelectorOnMainThread_withObject_waitUntilDone_(
                    "refresh:", None, False)
        except Exception as e:  # noqa: BLE001 — a dead watcher degrades to polling
            log.warning("notes-widget: file watcher stopped (%s); polling every %.0fs", e, POLL_SECS)

    threading.Thread(target=watch, name="notes-watch", daemon=True).start()

    frame = controller.panel.frame()
    print(f"notes-widget: dir={directory} level={level} "
          f"frame=({frame.origin.x:.0f},{frame.origin.y:.0f} {frame.size.width:.0f}x{frame.size.height:.0f}) "
          f"position_file={position_path}", flush=True)

    if once:
        Foundation.NSRunLoop.currentRunLoop().runUntilDate_(
            Foundation.NSDate.dateWithTimeIntervalSinceNow_(1.0))
        print(f"notes-widget: window visible={bool(controller.panel.isVisible())} "
              f"number={controller.panel.windowNumber()}", flush=True)
        print("--- rendered ---")
        print(controller.last_render)
        stop.set()
        return 0

    try:
        app.run()
    except KeyboardInterrupt:
        pass
    finally:
        stop.set()
    return 0
