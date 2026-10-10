"""Tap one allowlisted key on the host, on request from the board.

A swipe on the pet sends {"cmd":"key","name":"enter"|"next"|"prev"} and the
bridge synthesizes that single keystroke, so Claude Code's option pickers can
be answered without reaching for the keyboard.

Hold-the-pet push-to-talk used to live here too — a finger on the pet held a
dictation app's global hotkey. It is gone (owner request): the board no longer
sends voice frames, and nothing here holds a modifier down any more. What is
left is the tap, which is bounded, idempotent and cannot stick a key.

Mechanics: Quartz CGEventPost at the HID tap. Taps use a NULL event source —
with a real source, Warp's global key handling swallowed Return before the
focused app saw it. EVE_KEY_SOURCE=hid forces the real source if some
other app ever needs the opposite, and EVE_KEY_METHOD=osascript routes
through System Events instead (Warp ignores CGEventPost synthetics).

Requires macOS Accessibility permission for the daemon's python — CGEventPost
is silently filtered for untrusted processes, so without the check the feature
would just mysteriously do nothing.
"""

from __future__ import annotations

import logging
import os
import sys
import time
from typing import Callable, Optional

log = logging.getLogger(__name__)

KEY_RETURN = 36    # kVK_Return

# Event flag masks (Quartz constants, hardcoded so the module imports on
# non-mac hosts for testing). Read by listen_key.py, which watches for a real
# Option press rather than synthesizing one.
FLAG_ALTERNATE = 0x00080000   # kCGEventFlagMaskAlternate
FLAG_SECONDARY_FN = 0x00800000  # kCGEventFlagMaskSecondaryFn

KEY_KEYPAD_ENTER = 76  # kVK_ANSI_KeypadEnter
KEY_DOWN_ARROW = 125   # kVK_DownArrow
KEY_UP_ARROW = 126     # kVK_UpArrow

# Keys the board may ask us to tap. Deliberately a tiny allowlist: the board
# is a peripheral on a serial line, and "synthesize any keystroke on request"
# is a much larger surface than this feature needs.
#
# "enter" resolves to the main Return key (kVK_Return 36) by default. A few
# apps distinguish it from the numeric keypad's Enter (kVK_ANSI_KeypadEnter
# 76) — notably some editors and terminal multiplexers, where Return inserts
# a newline and keypad Enter submits. EVE_ENTER_KEY=keypad switches it.
def _enter_keycode() -> int:
    choice = (os.environ.get("EVE_ENTER_KEY") or "return").strip().lower()
    if choice in ("keypad", "keypad-enter", "enter"):
        return KEY_KEYPAD_ENTER
    return KEY_RETURN


# "next"/"prev" come from horizontal swipes on the pet and navigate Claude
# Code's option pickers, which are vertical lists — so they map to Down/Up
# arrows. Kept semantic on the wire so the firmware never hardcodes keycodes.
TAPPABLE = {
    "enter": _enter_keycode(),
    "next": KEY_DOWN_ARROW,
    "prev": KEY_UP_ARROW,
}

# poster signature: (keycode, down) -> None
# EVE_KEY_SOURCE selects the event source; see _quartz_poster.
Poster = Callable[[int, bool], None]


def _quartz_poster() -> Optional[Poster]:
    """Build the real CGEvent poster; None when Quartz is unavailable."""
    if sys.platform != "darwin":
        return None
    try:
        import Quartz
    except ImportError:
        log.warning(
            "key: pyobjc-framework-Quartz not installed — swipe-to-key "
            "does nothing (pip install pyobjc-framework-Quartz)")
        return None

    # A real event source rather than None. Events created with a NULL source
    # carry no keyboard state and some apps drop them; HIDSystemState makes the
    # synthesized key look like it came from the actual keyboard.
    try:
        src = Quartz.CGEventSourceCreate(Quartz.kCGEventSourceStateHIDSystemState)
    except Exception:  # noqa: BLE001
        src = None

    # Source choice is load-bearing and app-dependent:
    #
    # * Plain TAPS want the NULL source. With a real source, Warp's global key
    #   handling swallowed Return before it reached the focused terminal app —
    #   the dictated text sat in the prompt unsent. NULL-source Return worked
    #   for hours before this was changed, so taps go back to it.
    #
    # EVE_KEY_SOURCE=hid forces the real source everywhere if some other
    # app ever needs the opposite.
    force_hid = (os.environ.get("EVE_KEY_SOURCE") or "").strip().lower() == "hid"

    def post(keycode: int, down: bool) -> None:
        ev = Quartz.CGEventCreateKeyboardEvent(
            src if force_hid else None, keycode, down)
        Quartz.CGEventPost(Quartz.kCGHIDEventTap, ev)

    return post


def _check_accessibility(prompt: bool = False) -> bool:
    """True if this process may post events.

    ``prompt`` shows the system "would like to control this computer" dialog —
    pass it at most ONCE per daemon life. Prompting on every failed attempt
    re-pops the dialog on every tap, which is indistinguishable from the grant
    not working and trains the user to dismiss it.
    """
    try:
        from ApplicationServices import (
            AXIsProcessTrusted,
            AXIsProcessTrustedWithOptions,
            kAXTrustedCheckOptionPrompt,
        )
        if prompt:
            return bool(AXIsProcessTrustedWithOptions(
                {kAXTrustedCheckOptionPrompt: True}))
        return bool(AXIsProcessTrusted())
    except ImportError:
        # Can't check — post anyway; if trust is missing the events are
        # silently dropped, and the log line below is the only breadcrumb.
        log.warning(
            "key: ApplicationServices unavailable — cannot verify "
            "Accessibility permission; if swipe-to-key does nothing, grant it "
            "to the daemon's python in System Settings > Privacy & Security")
        return True


class KeyTapper:
    """Synthesizes single keystrokes the board asks for. Stateless per tap."""

    def __init__(self, poster: Optional[Poster] = None,
                 trust_check: Optional[Callable[[], bool]] = None) -> None:
        # Injected poster (tests) skips the Accessibility machinery entirely;
        # the real Quartz poster gets the real trust check unless overridden.
        if poster is None:
            poster = _quartz_poster()
            if trust_check is None and poster is not None:
                trust_check = _check_accessibility
        self._poster = poster
        self._trust_check = trust_check
        self._prompted = False   # the system dialog is shown at most once

    def diagnose(self) -> int:
        """Print why swipe-to-key is or isn't working. Returns an exit code.

        Accessibility is granted per *binary*, and a venv's python is a symlink
        — macOS resolves it, so the path that must appear (and be toggled ON)
        in System Settings is the real interpreter, not the venv one.
        """
        import os
        import subprocess

        real = os.path.realpath(sys.executable)
        print(f"daemon python (argv):  {sys.executable}")
        print(f"resolved binary:       {real}")
        if real != sys.executable:
            print("  ^ ADD THIS ONE in System Settings — the venv path is a symlink")

        sig = "unknown"
        try:
            out = subprocess.run(["codesign", "-dv", "--verbose=2", real],
                                 capture_output=True, text=True, timeout=10)
            blob = out.stderr or out.stdout
            adhoc = "adhoc" in blob or "Signature=adhoc" in blob
            sig = "ad-hoc / linker-signed" if adhoc else "signed"
        except Exception:  # noqa: BLE001
            pass
        print(f"code signature:        {sig}")
        if sig.startswith("ad-hoc"):
            print("  note: ad-hoc-signed interpreters (uv/pyenv builds) are the")
            print("  usual cause of a grant that 'won\'t stick' — macOS keys the")
            print("  grant to the signature. Remove every stale python entry in")
            print("  the Accessibility list, then re-add the resolved path above.")

        if self._poster is None:
            print("quartz poster:         UNAVAILABLE (pyobjc not installed?)")
            return 2
        print("quartz poster:         ok")

        trusted = _check_accessibility(prompt=False)
        print(f"accessibility trusted: {trusted}")
        if not trusted:
            print("\nFIX: System Settings > Privacy & Security > Accessibility")
            print("  1. remove any existing 'python3.12' rows (stale grants)")
            print(f"  2. '+', then Cmd+Shift+G, paste: {real}")
            print("  3. make sure its toggle is ON (adding alone does not enable it)")
            print("  4. restart the daemon:")
            print("     launchctl kickstart -k gui/$(id -u)/com.github.eve-bridge.daemon")
            return 1
        print("\nReady — swipe the pet to send Enter.")
        return 0

    @staticmethod
    def _tap_via_system_events(keycode: int) -> bool:
        """Deliver a key through System Events instead of CGEventPost.

        Warp ignores CGEventPost synthetics — the Enter arrives, the log says
        it was tapped, and nothing submits, while the identical event works in
        every other app. System Events posts through a different path that
        Warp does accept. Costs a subprocess (~40ms), so it is opt-in.
        """
        import subprocess
        try:
            r = subprocess.run(
                ["osascript", "-e",
                 f'tell application "System Events" to key code {keycode}'],
                capture_output=True, text=True, timeout=5)
            if r.returncode != 0:
                log.warning("key: System Events failed: %s",
                            r.stderr.strip()[:200])
                return False
            return True
        except Exception as e:  # noqa: BLE001
            log.warning("key: System Events error: %s", e)
            return False

    def tap(self, name: str) -> bool:
        """Press and release one allowlisted key (swipe-down → Enter)."""
        key = TAPPABLE.get(name)
        if key is None:
            log.warning("key: refusing unknown key %r", name)
            return False
        if self._poster is None:
            return False
        if self._trust_check is not None:
            try:
                ok = self._trust_check(prompt=not self._prompted)  # type: ignore[call-arg]
            except TypeError:
                ok = self._trust_check()
            self._prompted = True
            if not ok:
                log.warning("key: Accessibility not granted — see `key-check`")
                return False
            self._trust_check = None
        method = (os.environ.get("EVE_KEY_METHOD") or "auto").strip().lower()
        if method in ("osascript", "system-events"):
            ok = self._tap_via_system_events(key)
            log.info("key: tapped %s via System Events (%s)", name,
                     "ok" if ok else "FAILED")
            return ok

        self._poster(key, True)
        # Hold briefly. A down/up in the same microsecond is not a keypress any
        # human could produce, and apps that debounce or sample input on a
        # frame boundary drop it entirely.
        time.sleep(0.03)
        self._poster(key, False)
        log.info("key: tapped %s", name)
        return True
