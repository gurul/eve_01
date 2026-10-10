"""meet_page.js, run for real: headless Chromium, the real script, fixture pages shaped like Meet's.

Every fixture button counts its own clicks, so each test can say not only what the script pressed but that it
pressed nothing else. The fixtures use the labels Meet uses (aria-label, as OpenClaw's plugin reads them).
Skipped where Playwright's Chromium is not installed."""
from __future__ import annotations

import re

import pytest

from eve_bridge import meet

playwright_api = pytest.importorskip("playwright.sync_api")


@pytest.fixture(scope="module")
def browser():
    with playwright_api.sync_playwright() as pw:
        try:
            b = pw.chromium.launch(headless=True)
        except Exception as e:  # noqa: BLE001
            pytest.skip(f"no Chromium for Playwright: {e}")
        yield b
        b.close()


@pytest.fixture()
def page(browser):
    p = browser.new_page()
    yield p
    p.close()


# A button that counts its clicks; a toggle flips its label between "Turn off X" and "Turn on X".
HARNESS = """
<script>
window.__hits = {};
function hit(label) { window.__hits[label] = (window.__hits[label] || 0) + 1; }
document.addEventListener('click', (e) => {
  const b = e.target.closest('button, [role=button]');
  if (!b) return;
  const label = b.getAttribute('aria-label') || b.textContent.trim();
  hit(label);
  const m = label.match(/^Turn (off|on) (microphone|camera|captions)$/);
  if (m) b.setAttribute('aria-label', 'Turn ' + (m[1] === 'off' ? 'on' : 'off') + ' ' + m[2]);
  if (label === 'Join now' || label === 'Join here too') document.body.dataset.joined = '1';
}, true);
</script>
"""


def load(page, body: str) -> None:
    page.set_content(f"<html><body>{body}{HARNESS}</body></html>")


def run(page, **opts):
    return page.evaluate(meet.PAGE_SCRIPT, {"act": True, **opts})


def hits(page) -> dict[str, int]:
    return page.evaluate("window.__hits")


def btn(label: str) -> str:
    return f'<button aria-label="{label}"><i>icon</i></button>'


def test_prejoin_turns_mic_and_camera_off_before_it_joins(page):
    load(page, btn("Turn off microphone") + btn("Turn off camera") + '<button>Join now</button>')
    first = run(page)
    assert first["clicked"] == ["mic-off", "camera-off"]
    assert not first["joinClicked"]                         # never in the same pass as turning one off
    second = run(page)
    assert second["clicked"] == ["join"] and second["joinClicked"]
    assert hits(page) == {"Turn off microphone": 1, "Turn off camera": 1, "Join now": 1}


def test_no_toggle_yet_is_not_off(page):
    load(page, '<button>Join now</button>')
    assert run(page)["clicked"] == []                       # the preview's toggles have not rendered
    assert run(page, noControlsOk=True)["clicked"] == ["join"]   # meet.py allows it after NO_CONTROLS_SECS


def test_ask_to_join_is_pressed_when_already_dark(page):
    load(page, btn("Turn on microphone") + btn("Turn on camera") + '<button>Ask to join</button>')
    assert run(page)["clicked"] == ["join"]


def test_the_same_account_elsewhere_gets_join_here_too_never_switch_here(page):
    load(page, btn("Turn on microphone") + btn("Turn on camera")
         + '<button>Switch here</button><button>Join here too</button>'
         + '<div>You are in this call on another device</div>')
    st = run(page)
    assert st["joinHereToo"] and st["switchHere"]
    assert st["clicked"] == ["join-here-too"]
    assert hits(page).get("Switch here", 0) == 0 and hits(page)["Join here too"] == 1


def test_only_switch_here_offered_presses_nothing(page):
    load(page, btn("Turn on microphone") + btn("Turn on camera") + '<button>Switch here</button>'
         + '<button>Use Companion mode</button>')
    for _ in range(3):
        assert run(page, noControlsOk=True)["clicked"] == []
    assert hits(page) == {}


def test_the_microphone_prompt_is_answered_without_it(page):
    load(page, '<div>Do you want people to hear you in the meeting?</div>'
         '<button>Use microphone</button><button>Continue without microphone</button>')
    st = run(page)
    assert "no-mic" in st["clicked"]
    assert hits(page).get("Use microphone", 0) == 0


def test_in_call_the_mic_is_muted_again_and_captions_go_on(page):
    load(page, btn("Leave call") + btn("Turn off microphone") + btn("Turn on camera")
         + btn("Turn on captions"))
    st = run(page, captions=True)
    assert st["inCall"]
    assert st["clicked"] == ["mic-off", "captions-on"]
    assert hits(page).get("Turn on camera", 0) == 0


def test_lobby_and_ended_and_sign_in_are_read(page):
    load(page, '<div>Asking to be let in…</div><div>You\'ll join the call when someone lets you in</div>'
         + btn("Turn on microphone") + btn("Turn on camera"))
    st = run(page)
    assert st["lobby"] and not st["inCall"] and st["clicked"] == []
    load(page, '<div>You left the meeting</div><button>Rejoin</button><button>Return to home screen</button>')
    assert run(page)["ended"] == "you left the meeting"
    load(page, '<div>No one responded to your request to join the call</div>')
    assert "no one responded" in run(page)["ended"]
    load(page, '<div>Choose an account to continue to Google Meet</div><button>Join now</button>')
    st = run(page, noControlsOk=True)
    assert st["signIn"] and st["clicked"] == []


def test_leave_presses_leave_call_then_the_confirmation(page):
    load(page, btn("Leave call") + btn("Turn on microphone"))
    st = run(page, leave=True)
    assert st["clicked"] == ["leave"]
    load(page, btn("Leave call") + '<button>Leave meeting</button>')
    assert run(page, leave=True)["clicked"] == ["leave-confirm"]


def test_act_false_reads_without_touching(page):
    load(page, btn("Turn off microphone") + btn("Turn off camera") + '<button>Join now</button>')
    st = page.evaluate(meet.PAGE_SCRIPT, {"act": False})
    assert st["micOn"] and st["camOn"] and st["clicked"] == [] and hits(page) == {}


# Meet's caption region as it rendered live (2026-09-25, trimmed of classes): one block per speaker turn,
# the name in a span two divs down, the words in one div as several text nodes, a hidden div and the
# "Jump to bottom" button beside the blocks. Plus eve's own tile (data-is-self) and a name with no words yet.
CAPTIONS = """
<div tabindex="0" role="region" aria-label="Captions">
  <div class="nMcdL" id="a"><div class="adE6rb"><div class="KcIKyf"><span class="NWpY1d">Alice Chen</span></div></div><div class="ygicle words">We should </div></div>
  <div class="nMcdL" id="b"><div class="adE6rb"><div class="KcIKyf"><span class="NWpY1d">Bob</span></div></div><div class="ygicle words">Sounds good</div></div>
  <div class="nMcdL" id="c" data-is-self="true"><div class="adE6rb"><div class="KcIKyf"><span class="NWpY1d">You</span></div></div><div class="ygicle words">test test</div></div>
  <div class="nMcdL" id="d"><div class="adE6rb"><div class="KcIKyf"><span class="NWpY1d">Carol</span></div></div><div class="ygicle words"></div></div>
  <div><div class="GvZY2" style="display: none;"></div></div>
  <div class="IMKgW"><div data-is-touch-wrapper="true"><button aria-label="Jump to most recent captions"><span><i aria-hidden="true">arrow_downward</i></span><span aria-hidden="true">Jump to bottom</span></button></div></div>
</div>
<script>document.querySelector('#a .words').append('ship ', 'on');</script>
"""


def test_caption_blocks_are_recorded_with_speakers_and_self_flagged(page):
    load(page, btn("Leave call") + btn("Turn on microphone") + btn("Turn on camera") + btn("Turn off captions")
         + CAPTIONS)
    st = run(page, captions=True)
    rows = st["snaps"][-1]["rows"]
    assert [(r["speaker"], r["text"], r["self"]) for r in rows] == [
        ("Alice Chen", "We should ship on", False), ("Bob", "Sounds good", False), ("You", "test test", True)]
    page.evaluate("document.querySelector('#a .words').append(' Friday')")
    page.wait_for_timeout(400)                               # the observer records on its own
    st2 = run(page, captions=True, since=st["seq"])
    assert st2["snaps"] and st2["snaps"][-1]["rows"][0]["text"] == "We should ship on Friday"
    assert st2["snaps"][-1]["rows"][0]["id"] == rows[0]["id"]    # the same block keeps its id
    assert all(s["seq"] > st["seq"] for s in st2["snaps"])


def test_captions_feed_the_merger_end_to_end(page):
    load(page, btn("Leave call") + btn("Turn on microphone") + btn("Turn on camera") + btn("Turn off captions")
         + CAPTIONS)
    merger = meet.CaptionMerger()
    lines = []
    st = run(page, captions=True)
    for s in st["snaps"]:
        lines += merger.feed(s)
    page.evaluate("document.querySelector('#a .words').append(' Friday')")
    page.wait_for_timeout(400)
    for s in run(page, captions=True, since=st["seq"])["snaps"]:
        lines += merger.feed(s)
    lines += merger.finish()
    assert [(ln.speaker, ln.text) for ln in lines] == [("Alice Chen", "We should ship on Friday"), ("Bob", "Sounds good")]


def test_only_safe_clicks_across_every_fixture(page):
    """Listen only: across every page shape, the script's click log holds only the safe names, and no control
    that could unmute, show video, present or take the call from another device is ever pressed."""
    danger = ["Turn on microphone", "Turn on camera", "Switch here", "Use Companion mode", "Present now",
              "Use microphone", "Unmute"]
    shapes = [
        btn("Turn off microphone") + btn("Turn off camera") + '<button>Join now</button>',
        btn("Turn on microphone") + btn("Turn on camera") + '<button>Switch here</button><button>Join here too</button>',
        btn("Leave call") + btn("Turn on microphone") + btn("Turn on camera") + btn("Turn on captions") + CAPTIONS,
        '<div>Do you want people to hear you?</div><button>Use microphone</button><button>Continue without microphone</button>',
        '<div>Asking to be let in</div>' + btn("Turn on microphone") + btn("Turn on camera"),
    ]
    extra = "".join(f"<button>{d}</button>" for d in danger)
    safe = set(re.findall(r'^\s*"([a-z-]+)":', _safe_block(), re.M))
    for shape in shapes:
        load(page, shape + extra)
        for opts in ({}, {"noControlsOk": True}, {"captions": True}, {"leave": True}):
            st = run(page, **opts)
            assert set(st["clicked"]) <= safe
        pressed = hits(page)
        assert not any(pressed.get(d) for d in danger), pressed


def _safe_block() -> str:
    return meet.PAGE_SCRIPT.split("const SAFE = {", 1)[1].split("};", 1)[0]


def test_only_safe_by_reading_the_source():
    """The same rule, read off the script: one element click in the whole file (inside click(), which refuses a
    name outside SAFE), every click("…") names a SAFE entry, and no SAFE pattern can match an unmute or a
    camera-on label. Positive control: the check does find the one real ``b.click()``."""
    src = meet.PAGE_SCRIPT
    code = "\n".join(ln for ln in src.splitlines() if not ln.lstrip().startswith("//"))
    assert code.count(".click()") == 1 and "b.click();" in code
    safe = dict(re.findall(r'^\s*"([a-z-]+)":\s*(/.+/i),?$', _safe_block(), re.M))
    assert set(safe) == {"mic-off", "camera-off", "no-mic", "join", "join-here-too", "captions-on",
                         "leave-confirm", "leave"}
    called = set(re.findall(r'click\("([a-z-]+)"\)', code))
    assert called and called <= set(safe)
    for bad in ("Turn on microphone", "Turn on camera", "Switch here", "Unmute", "Use microphone", "Present now"):
        for name, pattern in safe.items():
            body = pattern[1:-2]
            assert not re.search(body, bad, re.I), (name, bad)
    for py_line in open(meet.__file__, encoding="utf-8"):
        assert not re.search(r"turn on (microphone|camera)|unmute\(", py_line, re.I) or py_line.strip().startswith(("#", '"', "'")) or "never" in py_line.lower()
