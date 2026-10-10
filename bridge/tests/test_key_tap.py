"""KeyTapper — the swipe-to-key path.

A swipe on the pet asks the host to press one key. The allowlist is the whole
security story here ("synthesize any keystroke on request" is a much bigger
surface than this feature needs), and the event source is load-bearing: a tap
built from a real HID source is swallowed by Warp's global key handling.

Hold-the-pet push-to-talk used to live in this module and is gone, so there is
no longer any way for this code to leave a key down.
"""

from __future__ import annotations

from eve_bridge.key_tap import KEY_RETURN, KeyTapper


class FakePoster:
    def __init__(self) -> None:
        self.events: list[tuple[int, bool]] = []

    def __call__(self, keycode: int, down: bool) -> None:
        self.events.append((keycode, down))


def _tapper(trusted: bool = True) -> tuple[KeyTapper, FakePoster]:
    p = FakePoster()
    k = KeyTapper(poster=p, trust_check=(lambda: trusted) if not trusted else None)
    return k, p


def test_tap_enter_presses_and_releases() -> None:
    k, p = _tapper()
    assert k.tap("enter")
    assert p.events == [(KEY_RETURN, True), (KEY_RETURN, False)]


def test_tap_refuses_unknown_key() -> None:
    k, p = _tapper()
    assert not k.tap("rm-rf")
    assert p.events == []


def test_tap_next_prev_map_to_arrows() -> None:
    from eve_bridge.key_tap import KEY_DOWN_ARROW, KEY_UP_ARROW
    k, p = _tapper()
    assert k.tap("next")
    assert k.tap("prev")
    assert p.events == [
        (KEY_DOWN_ARROW, True), (KEY_DOWN_ARROW, False),
        (KEY_UP_ARROW, True), (KEY_UP_ARROW, False),
    ]


def test_tap_untrusted_posts_nothing() -> None:
    k, p = _tapper(trusted=False)
    assert not k.tap("enter")
    assert p.events == []


def test_no_poster_is_a_clean_noop(monkeypatch) -> None:
    """No Quartz (non-mac host, pyobjc missing): tap reports failure, never raises."""
    import eve_bridge.key_tap as kt
    monkeypatch.setattr(kt, "_quartz_poster", lambda: None)
    k = KeyTapper()
    assert not k.tap("enter")


def test_enter_defaults_to_main_return() -> None:
    from eve_bridge.key_tap import _enter_keycode
    assert _enter_keycode() == KEY_RETURN


def test_enter_can_be_switched_to_keypad(monkeypatch) -> None:
    from eve_bridge.key_tap import KEY_KEYPAD_ENTER, _enter_keycode
    monkeypatch.setenv("EVE_ENTER_KEY", "keypad")
    assert _enter_keycode() == KEY_KEYPAD_ENTER


def test_taps_use_the_null_event_source(monkeypatch) -> None:
    """Regression: Return built from a real HID source was swallowed by Warp's
    global key handling and never reached the focused app. Taps use NULL;
    EVE_KEY_SOURCE=hid forces the real source."""
    import sys
    import types

    import eve_bridge.key_tap as kt

    hid = object()
    sources: list[object] = []
    quartz = types.SimpleNamespace(
        kCGEventSourceStateHIDSystemState=1,
        kCGHIDEventTap=0,
        CGEventSourceCreate=lambda state: hid,
        CGEventCreateKeyboardEvent=lambda src, keycode, down: sources.append(src) or (keycode, down),
        CGEventPost=lambda tap, ev: None,
    )
    monkeypatch.setattr(sys, "platform", "darwin")
    monkeypatch.setitem(sys.modules, "Quartz", quartz)
    monkeypatch.delenv("EVE_KEY_METHOD", raising=False)

    monkeypatch.delenv("EVE_KEY_SOURCE", raising=False)
    assert KeyTapper(poster=kt._quartz_poster()).tap("enter")
    assert sources == [None, None]

    sources.clear()
    monkeypatch.setenv("EVE_KEY_SOURCE", "hid")
    assert KeyTapper(poster=kt._quartz_poster()).tap("enter")
    assert sources == [hid, hid]
