"""eve makes apps: "make me a habit tracker" becomes a small web app that opens inside Telegram.

Owner, 2026-09-24: "mini apps are to replace websites, like i want to build a habit tracker, it should do it",
then "give the opus api all the context it needs to build these simple apps, optimize heavily for this" and
"allow apps to be deleted and mutated easily".

Claude (``claude-opus-5-5``) writes one self-contained HTML file for the request; it is kept on the Mac under
``~/.config/eve-bridge/apps/<slug>/`` and served by the Mini App server (miniapp.py) at ``/apps/<slug>/``,
so it opens from a button in the chat or from the Mini App's home screen. "Add streaks to the habit tracker"
edits the same app: Claude gets the current file, a sample of its saved data and what was asked of it before,
and returns the whole new file.

A build is a loop, not one shot. Claude's file is checked in code (it keeps its data with window.eve, loads
nothing it may not) and then used for real in a headless phone-sized browser (app_check.py). Anything that
broke goes back to Claude, with the tap that broke it, for a repair round (up to ``max_repairs``), answered as
FIND/REPLACE edits to the file rather than the whole file again; the version with the fewest problems is kept. The system prompt carries everything a one-shot build needs: the Telegram
runtime, the storage contract, a design system and a reference skeleton.

An app keeps its data through ``window.eve`` (``/eve.js``, injected into every app): ``await eve.load()``
returns what it saved last (or null) and ``await eve.save(obj)`` keeps a JSON value, on the Mac, in
``data.json`` beside the app. An app is model-written code, so it is fenced in: served sandboxed, it never
holds the owner's initData, and every load and save carries an app token bound to that app alone
(miniapp.app_token), so an app reaches its own data and nothing else. A library it loads must be an npm
package at an exact version on jsdelivr (``load_issues``, a gate, not a hint). Earlier versions of an app are
kept under ``versions/`` (``MAX_VERSIONS``) for undo; a deleted app moves to ``<root>/.trash/`` and can be
restored, never removed.
"""

from __future__ import annotations

import asyncio
import datetime as _dt
import html as _html
import json
import logging
import os
import re
import shutil
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Awaitable, Callable, Mapping, Optional
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from . import app_check, jev_verify

log = logging.getLogger(__name__)

DEFAULT_ROOT = Path.home() / ".config" / "eve-bridge" / "apps"
MAX_DATA_BYTES = 1_000_000
MAX_APP_BYTES = 600_000
MAKE_MAX_TOKENS = 96_000             # thinking counts toward it too; a whole app is ~10-25k tokens of text
SYSTEM_CACHE_TTL = "1h"              # the maker prompt outlives one build's rounds (see claude_generate)
MAX_VERSIONS = 10
MAX_REQUESTS = 30                    # the newest requests kept in meta.json
MAX_BUILDS = 30                      # the newest build results kept in meta.json (the app's change chat)
DATA_SAMPLE_CHARS = 6000             # how much of an app's saved data an edit prompt shows
CHAT_PROGRESS_SECS = 20.0            # a chat build's progress lines, at most this often
SLUG_RE = re.compile(r"[a-z0-9][a-z0-9-]{0,47}")
ALLOWED_HOSTS = ("cdn.jsdelivr.net", "telegram.org")
# A library from jsdelivr is an npm package at an exact version: npm never changes a published version, so the
# code an app runs is the code it was tested with. A range, @latest, no version, or GitHub (/gh/, a branch
# anyone can push to) can all change under the app later.
PINNED_LIB_RE = re.compile(r"/npm/(?:@[a-z0-9][\w.-]*/)?[a-z0-9][\w.-]*@\d+\.\d+\.\d+(?:-[\w.]+)?(?:/|$)", re.I)
_JSDELIVR_URL = re.compile(r"(?:https?:)?//cdn\.jsdelivr\.net(/[^\s\"'`)<>\\]*)?", re.I)
_FILLER = frozenset({"the", "my", "a", "an", "app", "apps", "one", "this", "that"})
TRASH = ".trash"
UNDO_REQUEST = "(undo)"              # the request an undo leaves in meta.json
_TRASHED_RE = re.compile(r"^([a-z0-9][a-z0-9-]{0,47})-(\d{8}-\d{6})(?:-\d+)?$")
_DATA_V_RE = re.compile(rb'^\s*\{\s*"v"\s*:\s*(\d{1,9})\b')

# The reference app's journeys, as the prompt shows them. tests/test_apps_maker.py parses them and walks them.
SKELETON_JOURNEYS = """[
  {"name": "Log two glasses", "steps": [{"do": "tap", "target": "Add a glass"}, {"do": "tap", "target": "Add a glass"}],
   "expect": ["2 glasses", "6 glasses to go of 8"]},
  {"name": "Lower the goal", "steps": [{"do": "type", "target": "Daily goal, in glasses", "value": "5"},
   {"do": "tap", "target": "Add a glass"}], "expect": ["4 glasses to go of 5"]},
  {"name": "See the week", "steps": [{"do": "tap", "target": "Add a glass"},
   {"do": "tap", "target": "See the last 7 days ›"}], "expect": ["Last 7 days", "1 glass"]}
]"""

# ---- what Claude is told ----------------------------------------------------------------------------
# One reference app, shown to Claude as a pattern. tests/test_apps_maker.py runs it through app_check, so the
# example the model learns from is itself proven to pass the check it will face.
SKELETON = r"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>Water Log</title>
<meta name="eve-icon" content="💧">
<meta name="description" content="Count glasses of water and see the week.">
<style>
:root {
  color-scheme: light dark;
  --bg: var(--tg-theme-secondary-bg-color, #f2f2f7); --card: var(--tg-theme-section-bg-color, #fff);
  --text: var(--tg-theme-text-color, #000); --hint: var(--tg-theme-hint-color, #8e8e93);
  --accent: var(--tg-theme-button-color, #2481cc); --on-accent: var(--tg-theme-button-text-color, #fff);
  --danger: var(--tg-theme-destructive-text-color, #e53935); --sep: var(--tg-theme-section-separator-color, #c8c7cc);
  --r: 12px; --pad: 16px;
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--text); overflow-x: hidden; -webkit-tap-highlight-color: transparent;
  font: 16px/1.4 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  padding: calc(12px + var(--tg-safe-area-inset-top, 0px) + var(--tg-content-safe-area-inset-top, 0px)) var(--pad)
    calc(24px + var(--tg-safe-area-inset-bottom, 0px) + var(--tg-content-safe-area-inset-bottom, 0px)); }
h1 { font-size: 28px; margin: 4px 0 16px; } .hint { color: var(--hint); font-size: 14px; }
.card { background: var(--card); border-radius: var(--r); padding: var(--pad); margin-bottom: 16px; }
.big { font-size: 56px; font-weight: 700; font-variant-numeric: tabular-nums; } .unit { font-size: 20px; color: var(--hint); }
.field { display: grid; gap: 8px; }
.err { color: var(--danger); font-size: 14px; } .err:empty { display: none; }
.row { display: flex; align-items: center; gap: 12px; min-height: 44px; border-top: 0.5px solid var(--sep); }
.row:first-child { border-top: 0; } .row > span { flex: 1; min-width: 0; overflow-wrap: anywhere; }
button { font: inherit; min-height: 44px; padding: 0 16px; border: 0; border-radius: var(--r); background: var(--accent); color: var(--on-accent); }
button.plain { background: none; color: var(--accent); padding: 0 8px; } button.danger { background: none; color: var(--danger); }
button:focus-visible, input:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
input { font-size: 16px; min-height: 44px; width: 100%; padding: 0 12px; border: 1px solid var(--sep); border-radius: 10px; background: var(--card); color: var(--text); }
.bar { height: 8px; border-radius: 4px; background: var(--accent); }
.toast { position: fixed; left: 16px; right: 16px; bottom: calc(16px + var(--tg-safe-area-inset-bottom, 0px)); padding: 12px 16px;
  border-radius: var(--r); background: var(--text); color: var(--card); opacity: 0; transition: opacity .2s; pointer-events: none; }
.toast.show { opacity: 1; }
@media (prefers-reduced-motion: reduce) { * { transition: none !important; } }
[hidden] { display: none !important; }
</style>
</head>
<body>
<section class="card" id="unreadable" role="alert" hidden>Your saved data couldn't be read, so nothing is saved over it.
  <button class="plain" id="retry">Try again</button></section>
<main id="home">
  <h1>Water Log</h1>
  <section class="card"><div class="big"><span id="count">0</span> <span class="unit">glasses</span></div>
    <div class="hint" id="goalLine"></div></section>
  <section class="card" id="empty">No glasses yet today. Tap <b>Add a glass</b> below to start.</section>
  <section class="card field">
    <label for="goal">Daily goal, in glasses</label>
    <input id="goal" type="number" inputmode="numeric" placeholder="e.g. 8">
    <div class="err" id="goalErr" aria-live="polite"></div>
  </section>
  <button class="plain" id="toHistory">See the last 7 days ›</button>
</main>
<main id="history" hidden>
  <h1>Last 7 days</h1>
  <section class="card" id="week"></section>
  <button class="danger" id="reset">Reset today</button>
</main>
<div class="toast" id="toast" role="status" aria-live="polite"></div>
<script>
const tg = window.Telegram && window.Telegram.WebApp;
const has = (v) => !!tg && tg.isVersionAtLeast(v);
const buzz = (kind) => { if (!has("6.1")) return; const h = tg.HapticFeedback;
  kind === "ok" ? h.notificationOccurred("success") : kind === "warn" ? h.notificationOccurred("warning")
    : kind === "pick" ? h.selectionChanged() : h.impactOccurred("light"); };
const ask = (msg) => new Promise((done) => (has("6.2") ? tg.showConfirm(msg, done) : done(confirm(msg))));
const dayKey = (d = new Date()) =>
  `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
const $ = (id) => document.getElementById(id);
const glasses = (n) => `${n} ${n === 1 ? "glass" : "glasses"}`;
const matchTheme = () => { if (tg) document.documentElement.style.colorScheme = tg.colorScheme; };

let state = { v: 2, goal: 8, days: {} };          // days: { "2026-09-24": 5 }
let readable = true;                               // false: the saved data could not be read; never save over it
function migrate(d) {                              // every shape this app ever saved -> the current one, else null
  if (d && d.v === 2 && d.days && typeof d.days === "object") return d;
  if (d && d.v === 1) return { v: 2, goal: d.goal || 8, days: d.log || {} };
  return null;
}
async function save() {
  if (!readable) return;
  try { await eve.save(state); } catch (e) { toast("Couldn't save. It will try again with your next change."); }
}
function commit() { render(); save(); }            // right after every change: Telegram closes without warning
function toast(text) { const t = $("toast"); t.textContent = text; t.classList.add("show");
  clearTimeout(toast.timer); toast.timer = setTimeout(() => t.classList.remove("show"), 2200); }

let view = "home";                                 // navigation: BackButton appears on every view but home
function show(next) {
  view = next; $("home").hidden = next !== "home"; $("history").hidden = next !== "history";
  $("toast").classList.remove("show");             // a toast belongs to the view that raised it
  if (tg) { next === "home" ? tg.BackButton.hide() : tg.BackButton.show(); }
  render(); window.scrollTo(0, 0);
}
function render() {
  const n = state.days[dayKey()] || 0;
  $("count").textContent = n; $("empty").hidden = n > 0;
  $("goalLine").textContent = n >= state.goal ? "Goal reached today" : `${glasses(state.goal - n)} to go of ${state.goal}`;
  if (document.activeElement !== $("goal")) $("goal").value = state.goal;
  const week = $("week"); week.textContent = "";
  for (let i = 6; i >= 0; i--) {
    const d = new Date(); d.setDate(d.getDate() - i);   // local calendar days, safe across months
    const c = state.days[dayKey(d)] || 0, row = document.createElement("div");
    row.className = "row";
    row.innerHTML = `<span></span><b></b><div class="bar" style="width:${Math.min(100, (c / state.goal) * 100) * 0.3}%"></div>`;
    row.querySelector("span").textContent = d.toLocaleDateString(undefined, { weekday: "short", day: "numeric" });
    row.querySelector("b").textContent = glasses(c);
    week.appendChild(row);
  }
  if (tg) { view === "home" && readable ? tg.MainButton.setText("Add a glass").show() : tg.MainButton.hide(); }
}
async function load() {
  try {
    const saved = await eve.load();
    const next = saved === null ? null : migrate(saved);
    readable = saved === null || next !== null;
    if (next) state = next;
  } catch (e) { readable = false; }
  $("unreadable").hidden = readable;
  show(view);
}

$("goal").addEventListener("change", (e) => {
  const g = Math.round(Number(e.target.value));
  if (!(g >= 1 && g <= 30)) { $("goalErr").textContent = "Pick a goal from 1 to 30 glasses."; buzz("warn"); return; }
  $("goalErr").textContent = ""; state.goal = g; buzz("pick"); commit();
});
$("toHistory").addEventListener("click", () => { buzz(); show("history"); });
$("retry").addEventListener("click", () => { buzz(); load(); });
$("reset").addEventListener("click", async () => {
  if (!(await ask("Reset today's count?"))) return;
  delete state.days[dayKey()]; buzz("ok"); commit(); toast("Today reset");
});

(async function boot() {
  if (tg) {
    tg.expand(); matchTheme(); tg.onEvent("themeChanged", matchTheme);
    if (has("6.1")) { tg.setHeaderColor("secondary_bg_color"); tg.setBackgroundColor("secondary_bg_color"); }
    if (has("7.7")) tg.disableVerticalSwipes();
    tg.MainButton.onClick(() => { const k = dayKey(); state.days[k] = (state.days[k] || 0) + 1; buzz("ok"); commit(); });
    tg.BackButton.onClick(() => show("home"));    // one handler each, registered once
  }
  await load();
  if (tg) tg.ready();                             // after the first real render: no blank flash
})();
</script>
</body>
</html>"""

MAKER_PROMPT = """You build small, polished apps that the owner opens on their phone inside Telegram, as Telegram \
Mini Apps. Each app replaces a website or an app-store app they would otherwise use: a habit tracker, an expense \
log, a workout planner, a reading list, a countdown, a calculator for something specific. It must feel like a \
real native app on day one and still work, with every record the owner made, on day three hundred.

<output>
Answer with exactly one ```html fenced block holding one complete HTML document, then one ```journeys block (see \
<journeys>), and nothing after them. A sentence before the blocks is fine; keep it short. Settle the data shape \
and the list of views first, briefly, then write the file once.

The document is one self-contained file: all CSS in <style>, all JavaScript in <script>, icons as emoji or inline \
SVG. Its <head> has, in this order:
- <meta charset="utf-8">
- <meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
- <title> with the app's name, 1 to 4 words (for example "Habit Tracker"). It becomes the app's name in the \
owner's list, and the home view's screen title is the same text.
- <meta name="eve-icon" content="…"> with exactly one emoji that fits the app. It becomes the app's icon.
- <meta name="description" content="…"> with one plain sentence under 80 characters saying what the app does.

The only address the app may load anything from is https://cdn.jsdelivr.net/npm/ (a library, pinned to an exact \
version, such as https://cdn.jsdelivr.net/npm/chart.js@4.4.1/dist/chart.umd.min.js), and only when it clearly \
earns its place; inline SVG or a canvas covers most charts. Everything else is inline. Telegram's script and \
window.eve are added to the page by the server before your code runs; do not include them yourself.
</output>

<runtime>
The page runs in Telegram's in-app browser on a phone: WebKit on iPhone, Chrome's WebView on Android. Portrait, \
360 to 430 CSS pixels wide, touch only (no hover, no right-click, no keyboard shortcuts). It opens fresh every \
time: nothing stays in memory between opens. Telegram can close it at any moment, without warning.

window.Telegram.WebApp (call it `tg`) is always there. The parts worth using:
- tg.ready() once the first screen is rendered; tg.expand() at start so the app gets the full height.
- Theme: Telegram sets CSS variables on <html> for the owner's current theme and updates them live: \
--tg-theme-bg-color, --tg-theme-secondary-bg-color, --tg-theme-section-bg-color, --tg-theme-text-color, \
--tg-theme-hint-color, --tg-theme-link-color, --tg-theme-button-color, --tg-theme-button-text-color, \
--tg-theme-accent-text-color, --tg-theme-destructive-text-color, --tg-theme-section-header-text-color, \
--tg-theme-subtitle-text-color, --tg-theme-section-separator-color, --tg-theme-header-bg-color, \
--tg-theme-bottom-bar-bg-color. The same values are in tg.themeParams (snake_case keys such as bg_color). \
tg.colorScheme is "light" or "dark"; tg.onEvent("themeChanged", fn) fires when it flips. Declare \
`:root { color-scheme: light dark; }` and set document.documentElement.style.colorScheme = tg.colorScheme at \
start and on themeChanged, so date and time pickers, checkboxes and scrollbars take the theme too; colors you \
compute in JavaScript (a canvas chart) are redrawn then as well. tg.setHeaderColor("secondary_bg_color") and \
tg.setBackgroundColor("secondary_bg_color") match Telegram's frame to a page whose background is the secondary \
color.
- tg.MainButton: Telegram's large native button, drawn by Telegram below the page (outside it, so the page \
reserves no space for it), above the keyboard. Use it for the one primary action of the current view (Add, \
Save, Log today). setText(text), show(), hide(), enable(), disable(), showProgress(), hideProgress(), \
onClick(fn), offClick(fn), setParams({ text, color, text_color, is_active, is_visible }). tg.SecondaryButton \
(version 7.10+) is a second one for a secondary action, same methods.
- tg.BackButton: the back arrow in Telegram's header. show() on any view below the home view, hide() on home, \
onClick(fn) to go back one view. This is how an app navigates between views (list, detail, settings).
- tg.HapticFeedback: impactOccurred("light" | "medium" | "heavy" | "rigid" | "soft") on a tap that does \
something, selectionChanged() when a choice or toggle changes, notificationOccurred("success" | "warning" | \
"error") when something completes or fails.
- tg.showConfirm(message, callback(ok)) and tg.showPopup({ title, message, buttons: [{ id, type, text }] }, \
callback(buttonId)) for native dialogs; a button's type is "default", "ok", "close", "cancel" or "destructive". \
tg.showAlert(message) for a notice. Use these to confirm anything that destroys data.
- tg.disableVerticalSwipes() (7.7+): stops a downward swipe from closing the app. Call it when the app has \
dragging, sliders, or long scrolling lists where a swipe-to-close would lose the owner's place.
- Safe areas: --tg-safe-area-inset-top/-bottom/-left/-right (the device's notch and home bar) and \
--tg-content-safe-area-inset-top/-bottom/-left/-right (Telegram's own controls over the page in fullscreen). \
Outside fullscreen the top ones are 0; in fullscreen both are set. So the page's own padding adds both, with 0px \
fallbacks: top is calc(12px + var(--tg-safe-area-inset-top, 0px) + var(--tg-content-safe-area-inset-top, 0px)), \
the bottom the same with the -bottom insets, and anything fixed to an edge does the same.
- tg.initDataUnsafe.user.first_name is the owner's first name, if a greeting helps.

Older Telegram versions lack newer features, so guard them: tg.isVersionAtLeast("6.1") before BackButton, \
HapticFeedback, setHeaderColor and setBackgroundColor; "6.2" before showConfirm and showPopup (fall back to \
confirm()); "7.7" before disableVerticalSwipes; "7.10" before SecondaryButton; "8.0" before relying on the \
safe-area variables. Register each MainButton, SecondaryButton and BackButton handler exactly once at start, \
and let it act on the current view; handlers registered on every render pile up and fire many times.
</runtime>

<data>
window.eve is the app's storage, kept on the owner's Mac and shared by all their devices:
- `await eve.load()` returns the value saved last, or null the first time.
- `await eve.save(value)` stores any JSON value, up to 1 MB, replacing the previous one. Saves are sent in \
order and the newest one wins, so calling it after every change is safe and cheap.

How to use it:
- Keep all of the app's data in ONE root object with a version number: `{ v: 1, ... }`. Load it once at start, \
keep it in memory, render from it, and save the whole object right after every change the owner makes, never \
on a timer and never behind a Save button that only persists. If you delay a save while the owner types, keep \
the delay at 300ms or less and also save at once on visibilitychange (when hidden) and on pagehide, because \
Telegram closes the app without warning.
- eve is the only storage: the page is served sandboxed, where browser storage and cookies throw, and it \
can reach no address but its own eve and cdn.jsdelivr.net/npm. The owner's data lives in eve and nowhere \
else.
- Write a migrate(data) function that turns every shape the app has ever saved into the current one, and run \
what load() returns through it. Bump v when the shape changes.
- The owner's data is never lost. If load() throws, or returns data that migrate() cannot read (a broken shape, \
or a v newer than this file knows), show a short banner that says so with a Try again button, render the empty \
state, and call eve.save() no more in that session: saving would replace the data that is there. Only the \
owner deletes records: the app never trims, caps or drops them on its own, and migrate() keeps every entry.
- If save() throws (offline), show a short, friendly toast, keep working in memory, and save again with the \
next change.
- Dates: store days as local calendar keys "YYYY-MM-DD" built from getFullYear(), getMonth() + 1 and getDate() \
(the dayKey helper in the skeleton). Keys built from UTC shift the day for anyone east or west of Greenwich in \
the evening, and break streaks. Do date arithmetic on local Date objects (new Date(y, m, d), setDate(getDate() \
- 1)) so month ends and daylight-saving changes stay correct. Store times as full timestamps (Date.now()) when \
the moment matters.
- Money: store integer minor units (cents). Read typed amounts with one parser that accepts both decimal marks: \
a separator followed by one or two digits is the decimal mark, one followed by exactly three digits groups \
thousands, so "12.50", "12,50", "1,234.50" and "1.234,50" all read right. When a value still cannot be read, \
say so next to the field instead of guessing.
- Measured quantities (weight, distance, volume, temperature): store one canonical unit, convert only for \
display, take the default display unit from the owner's context, and offer a unit switch in a small settings \
row. Size step buttons to the unit shown.
- Ids: crypto.randomUUID() when it exists, else Date.now().toString(36) + Math.random().toString(36).slice(2).
- Store facts, not rendered HTML or derived totals.
</data>

<design>
Make it look like it belongs in Telegram, in light and dark mode, using this small design system:
- Colors only through variables that map to Telegram's theme, each with a fallback: page background \
--tg-theme-secondary-bg-color; cards and list sections --tg-theme-section-bg-color; text --tg-theme-text-color; \
secondary text --tg-theme-hint-color; primary buttons and highlights --tg-theme-button-color with \
--tg-theme-button-text-color; links and tinted buttons --tg-theme-link-color or --tg-theme-accent-text-color; \
delete --tg-theme-destructive-text-color; separators --tg-theme-section-separator-color. A chart may add a few \
colors of its own.
- State shows as a different hue, a fill against an outline, or an icon: done against not done, over against \
under. A faded copy of the accent turns muddy on a dark background. Check every mark of a chart against both a \
white and a near-black card.
- A selected tab, segment or chip is filled with --tg-theme-button-color and labelled in \
--tg-theme-button-text-color (or carries a 2px accent underline), and its label is heavier, so it stands out \
in light and dark mode alike.
- Type: the system font stack (-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif). Screen title \
26 to 28px bold; section header 13px uppercase in the hint color; body 16 to 17px; captions 13 to 14px hint. \
font-variant-numeric: tabular-nums on numbers that change.
- Every top-level view starts with its screen title; tabs or a segmented control sit under it. The header is \
one row: the title and at most two icon buttons. Secondary navigation (a month switcher) goes in a compact row \
below, with arrows no taller than 36px inside their 44px tap area.
- eve draws a small Change pencil over the top-right corner of every view. Keep the top-right 56x56px of every \
view, dialogs and sheets included, free of controls: the header gets padding-right: 56px, so its icon buttons sit \
to the left of that corner.
- Icon buttons in headers and toolbars are inline SVG drawn with currentColor in the link color. Emoji are for \
content (the app icon, categories, list items), and each one pictures exactly the thing it labels.
- Layout: 16px side gutters, 8/12/16/24px spacing, 12px corner radius. Content in cards or inset list sections \
on the secondary background, like Telegram's settings screens. List rows at least 44px tall with a hairline \
separator between rows; row text flexes with min-width: 0 and overflow-wrap: anywhere so long names wrap.
- Forms: fields stacked in a grid or flex column with gap (an error line between two fields must not break the \
spacing), each with a visible label or aria-label, and an error line under it. Every text field's placeholder \
is a realistic example for this app, written "e.g. " and then the example; the test types that example. A list \
row with more than two buttons holds no text field: tapping the row opens it for editing in its own view.
- Tap targets at least 44x44px. Inputs are 16px or larger (smaller makes iPhone zoom in on focus), full width, \
44px tall, with the right type and inputmode (number, decimal, date, time).
- Buttons: primary is filled with the button color; secondary is tinted text or an outline; destructive is \
text in the destructive color. The one primary action of a view goes on tg.MainButton or in one clear filled \
button: once, not again as a link or a second button elsewhere in the view.
- Empty states: one friendly line saying what the app is for, plus the action that starts it; for a list, \
offer two or three one-tap suggestions that fit the app, which the owner can add. Never pre-fill fake entries \
into their data.
- Feedback: every tap visibly responds within 100ms (state change, :active style, haptic). A short toast \
(role="status") confirms saves that are not already visible and offers Undo after a delete. A toast belongs to \
the view that raised it: hide it when the view changes, and keep it clear of controls (pointer-events: none \
except on its Undo button).
- :focus-visible outlines; a prefers-reduced-motion rule that turns animation off; motion itself short \
(150 to 250ms) and never blocking.
- Inside an SVG, shapes get their own classes with a prefix (chart-bar, chart-label) that no other rule uses: \
CSS width, height, x, y and transform override SVG attributes, so a shared class can stretch a shape.
- No horizontal scrolling at 360px: box-sizing: border-box everywhere, no fixed widths over 328px, tables and \
charts scaled to 100% width, overflow-x: hidden on body only as a last resort.
</design>

<quality>
- Build the whole app that was asked for, working end to end. No placeholders, "coming soon", lorem ipsum, \
dead buttons or TODOs. Where the request is short, add the few things any good version of that app has (a habit \
tracker: add, edit, delete and reorder habits, check off today, streaks, a week or month view) and stop there.
- Everything the owner creates can be edited and deleted. Deletes ask first (tg.showConfirm) or offer Undo.
- Validate input in your code where it is typed: trim text, ignore empty names, keep numbers and lengths in \
sane ranges, and show what is wrong in the error line under the field, with a warning haptic. A limit that an \
input attribute enforces by silently cutting the typing off hides that message; check it in code instead. \
Something already done cannot end in the future, and an end comes after its start.
- Pickers and chips start on the last choice used, or on a neutral one: never on a guess that is likely wrong.
- Computations are right: streaks count consecutive local days and survive month ends; weeks start on the day \
the owner's context names, else Monday; every number is computed over exactly the items its label names (an \
average of ratings counts only the rated items the label covers); totals and averages ignore missing days \
correctly. A summary at the top shows the exceptions too (a category over its limit, a debt not settled), not \
only the total.
- Every number shown carries its unit, and one quantity uses one unit across the whole app.
- Format dates, times, numbers and money with toLocaleDateString, toLocaleTimeString and Intl, always with \
undefined as the locale, so the phone's own locale formats all of them the same way. The owner's context \
chooses units, the currency (Intl.NumberFormat with style "currency") and the first day of the week.
- Put user-entered text on screen with textContent, or escape it before putting it into innerHTML, so a name \
with < or & shows as typed.
- Charts: inline SVG with a viewBox and width 100%, or a canvas scaled by devicePixelRatio; label the axes or \
values, with units, so the chart reads without explanation.
- Timers and countdowns store their absolute end time (Date.now() + duration), so they survive the app closing, \
and on the next open apply what finished while the owner was away. While one runs, hold \
navigator.wakeLock.request("screen") where it exists and take it again on visibilitychange.
- Fast: render from state in one pass; no polling timers; no work at load beyond load() and the first render.
- One file that a person can read: clear names, short functions, the state shape described in one comment.
</quality>

<journeys>
Passing the scripted test only proves the app does not break. The journeys prove it does what it is for. A \
separate verifier model uses the app by them on the phone-sized screen, step by step, each journey from a fresh \
app with no saved data, and then reads the screen for the text each journey expects. A journey that does not go \
through comes back to you as a problem, like a crash does.

After the html block, write one ```journeys block: a JSON list of 2 to 4 journeys covering the app's core \
purpose, the thing it exists for first (add an expense and see who owes what), then another flow that matters \
(edit or delete a record, the second view). Each journey is {"name": "<a few words>", "steps": [...], "expect": \
[...]}, and each step is one of:
- {"do": "tap", "target": "<label>"}: a button, tab, checkbox, list row or link.
- {"do": "type", "target": "<label>", "value": "<text>"}: a text, number or date field.
- {"do": "choose", "target": "<label>", "value": "<option's visible text>"}: a <select>, or a group of option \
buttons under that label (radio buttons, chips, a segmented control, a dropdown the app draws itself).

How to write them so the verifier can follow them (it is literal: it matches words, it does not infer):
- "target" is the control's visible label exactly as the page shows it: a button's text or its aria-label, a \
field's <label> or aria-label (never its placeholder), a select's label. Telegram's MainButton is tapped by the \
text it shows, and its Back button by "Back".
- Each control a journey names has a label of its own on the screen where it is used. A list that repeats a \
button in every row gives each one an aria-label with the row's name (a delete button labelled "Delete Alex"), \
and the journey uses that.
- Start from the first screen with nothing saved: open a form (tap its Add button) before typing into it, and \
fill every field the form needs. At most 10 steps each.
- Every value a journey types and every expected text uses made-up example values: the realistic examples of \
the placeholders, and names and amounts you invent. The verifier runs off the Mac, so journey text comes from \
these examples and never from the saved data a change shows you.
- "expect" holds 1 to 3 pieces of text that must be on screen after the last step, copied exactly as your code \
renders them for these values: a computed total, a balance line, the new row's text. Choose text that appears \
only if the feature worked, never text shown from the start. Each piece carries the result itself, the value the \
journey entered or the number it changed (the new row's name, the category shown on the row, the new balance), \
so it can only appear when the app kept what was entered. A toast or a confirmation line does not qualify: it \
shows whether or not the value was kept. Only literal text: the verifier reads the screen's text, it does not \
count items, compare numbers, read charts or work out dates.
</journeys>

<changes>
When you are changing an existing app, you get its current file, its journeys, a sample of the data it has \
saved, and what the owner asked for before. Return the WHOLE updated file, every line, in one ```html block, \
then the whole ```journeys block, updated so it still walks the app as it now is and covers the change. Keep \
everything the owner did not ask to change: features, look, title, icon. The owner's saved data must open in the \
new version exactly as it is: if the data shape has to change, bump v and extend migrate() so the sample shown \
converts cleanly, keeping every entry.
</changes>

<testing>
Before the owner sees it, your file is opened in a headless phone-sized browser with a stand-in Telegram and \
eve, in Telegram's dark theme: every field is filled (a text field with its placeholder's example), every \
button and tab tapped (delete buttons last, confirmations answered yes), the MainButton and BackButton pressed. \
Then it is closed the way Telegram closes it, left open for an hour on the clock, and reopened with the data it \
saved, in the light theme at 390px and 360px wide; that reopened screen, with the MainButton under it, is the \
picture the owner gets. Uncaught errors, console errors, a page reload, requests to other addresses, sideways \
scrolling, never saving, a save that only lands after closing, text or a selected tab that does not stand out \
in the dark theme, tap targets under 32px, inputs under 16px and chart shapes stretched by CSS all come back to \
you as problems to fix. Then the verifier walks your journeys (see <journeys>).
</testing>

<skeleton>
A reference app showing the pieces working together: theme variables and color-scheme, safe areas, load, \
migrate, never saving over data it could not read, render, save after every change, BackButton navigation \
between two views, the MainButton for the primary action, a labelled field with its error line, haptics, a \
native confirm before a destructive action, and a toast that goes with its view. It is a pattern to learn \
from, not a template: shape each app around its own purpose.

```html
""" + SKELETON + """
```

Its journeys:

```journeys
""" + SKELETON_JOURNEYS + """
```
</skeleton>"""


# Which units, currency and first weekday a configured location implies. A guess the prompt labels as one; the
# owner's own words in a request always win. Date and number formats are not here: the phone's locale formats
# those (the prompt says so), and a format named here is what a model hard-codes.
_LOCALES: list[tuple[re.Pattern[str], str]] = [
    (re.compile(r"\b(usa|u\.s\.a?\.?|united states|america)\b|,\s*(ca|ny|tx|wa|fl|ma|il|or|co|nj|pa)\b", re.I),
     "US customary units (miles, pounds, °F, fl oz), US dollars, weeks start on Sunday"),
    (re.compile(r"\b(uk|u\.k\.|united kingdom|england|scotland|wales|britain|london)\b", re.I),
     "metric units (kg, km, °C, ml) except miles for road distances, pounds sterling (£), weeks start on "
     "Monday"),
    (re.compile(r"\b(india|bengaluru|bangalore|mumbai|delhi|hyderabad|chennai|pune|kolkata)\b", re.I),
     "metric units, Indian rupees (₹), weeks start on Monday"),
    (re.compile(r"\b(canada|toronto|vancouver|montreal)\b", re.I),
     "metric units (kg, km, °C), Canadian dollars, weeks start on Sunday"),
    (re.compile(r"\b(australia|sydney|melbourne|brisbane|perth)\b", re.I),
     "metric units, Australian dollars, weeks start on Monday"),
    (re.compile(r"\b(germany|france|spain|italy|netherlands|ireland|portugal|austria|belgium|finland|berlin|"
                r"paris|madrid|rome|amsterdam|dublin|lisbon|vienna)\b", re.I),
     "metric units, euros (€), weeks start on Monday"),
]


def owner_context(environ: Optional[Mapping[str, str]] = None, *, now: Optional[_dt.datetime] = None) -> str:
    """Today, the time and where the owner is, for a build prompt: an app that shows "today" or a unit needs it.
    EVE_TIMEZONE (an IANA name) and EVE_LOCATION (free text), as system_context.py reads them."""
    env = os.environ if environ is None else environ
    zone_name = (env.get("EVE_TIMEZONE") or "").strip()
    try:
        zone = ZoneInfo(zone_name) if zone_name else None
    except (ZoneInfoNotFoundError, ValueError):
        zone = None
    local = (now or _dt.datetime.now().astimezone()).astimezone(zone)
    location = " ".join((env.get("EVE_LOCATION") or "").split())[:200]
    lines = [f"Today is {local.strftime('%A')}, {local.day} {local.strftime('%B %Y')} ({local.date().isoformat()}); "
             f"the owner's local time is {local.strftime('%H:%M')}, time zone {zone.key if zone else local.tzname()}."]
    if location:
        guess = next((units for pattern, units in _LOCALES if pattern.search(location)),
                     "metric units (kg, km, °C, ml), weeks start on Monday")
        lines.append(f"The owner lives in {location}. Unless they say otherwise, assume {guess}.")
    else:
        lines.append("No location is configured: use metric units.")
    return "\n".join(lines)


# ---- the store ------------------------------------------------------------------------------------

def _cut(text: str, limit: int) -> str:
    return text if len(text) <= limit else text[:limit - 1].rstrip() + "…"


def owner_issues(report: Optional[app_check.CheckReport], issues: list[str], limit: int = 300) -> list[str]:
    """``issues`` as the owner reads them (the change chat, the Telegram caption): a journey's problem is its
    short line ("Couldn't tap "Add expense" in "Split a dinner"."), not the repair round's text with its control
    list and screen dump; anything longer than ``limit`` ends in "…"."""
    run = report.journeys if report is not None else None
    plain = run.owner_lines() if run is not None else {}
    return [_cut(plain.get(i, i), limit) for i in issues]


def build_record(*, ok: bool, report: Optional[app_check.CheckReport], issues: list[str], usd: float,
                 secs: float, rounds: int, reason: str = "") -> dict[str, Any]:
    """How one build went, as meta.json keeps it for the app's change chat: whether it saved a page, the phone
    check's line ("passed: 3 fields filled, 9 taps, 4 saves, journeys 3/3"), the journeys Jev walked, what may
    still be wrong, and what it cost. The store adds when, the request and the version number."""
    run = report.journeys if report is not None else None
    return {"ok": ok, "check": report.summary() if report is not None else "not tested",
            "tested": report is not None and not report.skipped,
            "journeys": ({"passed": run.passed, "total": run.total, "not_run": run.not_run}
                         | ({"ran": len(run.results)} if run.not_run and run.results else {})
                         if run is not None and run.total else None),
            "problems": len(issues), "issues": owner_issues(report, issues[:2]), "usd": round(usd, 4),
            "secs": round(secs), "rounds": rounds, "reason": reason}


_RESULT_KEYS = ("ok", "version", "check", "tested", "journeys", "problems", "issues", "usd", "secs", "rounds", "reason")


def _result(build: dict[str, Any]) -> dict[str, Any]:
    return {k: build.get(k) for k in _RESULT_KEYS}


@dataclass
class AppInfo:
    slug: str
    title: str
    created: str
    updated: str
    icon: str = ""
    description: str = ""
    versions: int = 0                 # how many earlier versions undo can restore
    # The saved data has a newer shape (its "v") than the version undo would restore was written for: that
    # page would show its "can't read this data" banner, so an undo says so before it runs.
    undo_warning: bool = False

    def as_dict(self) -> dict[str, Any]:
        return {"slug": self.slug, "title": self.title, "created": self.created, "updated": self.updated,
                "icon": self.icon, "description": self.description, "versions": self.versions,
                "undo_warning": self.undo_warning}


def slugify(text: str) -> str:
    words = re.findall(r"[a-z0-9]+", text.lower())[:6]
    return ("-".join(words) or "app")[:40].strip("-") or "app"


def _clean(text: str, limit: int) -> str:
    return re.sub(r"\s+", " ", _html.unescape(text or "")).strip()[:limit]


def title_of(html: str, fallback: str = "App") -> str:
    m = re.search(r"<title[^>]*>(.*?)</title>", html, re.I | re.S)
    return _clean(m.group(1) if m else "", 60) or fallback[:60]


def _meta(html: str, name: str) -> str:
    """The content of ``<meta name="<name>" content="…">``, whatever the attribute order."""
    for tag in re.findall(r"<meta\b[^>]*>", html, re.I):
        attrs = {k.lower(): v1 or v2 for k, v1, v2 in re.findall(r"([\w-]+)\s*=\s*(?:\"([^\"]*)\"|'([^']*)')", tag)}
        if attrs.get("name", "").lower() == name:
            return attrs.get("content", "")
    return ""


def icon_of(html: str) -> str:
    """The app's emoji (``<meta name="eve-icon">``): a short run of non-ASCII symbols, else nothing."""
    icon = _clean(_meta(html, "eve-icon"), 16).replace(" ", "")
    return icon if icon and len(icon) <= 8 and not re.search(r"[\x00-\x7f]", icon.replace("‍", "")) else ""


def description_of(html: str) -> str:
    return _clean(_meta(html, "description"), 120)


def extract_html(text: str) -> Optional[str]:
    """The HTML document in Claude's answer: the ```html block, else the whole answer if it is a document."""
    m = re.search(r"```html\s*\n(.*?)```", text, re.S | re.I) or re.search(r"```\s*\n(<!doctype.*?)```", text, re.S | re.I)
    html = (m.group(1) if m else text).strip()
    low = html.lower()
    if ("<!doctype html" in low or "<html" in low) and "</html>" in low and len(html.encode()) <= MAX_APP_BYTES:
        return html
    return None


# Where a page actually loads something from: a src/href on a loading tag, CSS url()/@import, fetch/import/
# WebSocket/EventSource/sendBeacon. Plain links and namespace strings (the SVG xmlns) are not loads.
_LOADS = [
    re.compile(r"<(?:script|link|img|iframe|audio|video|source|embed|object|track)\b[^>]*?\b(?:src|href|data)\s*=\s*"
               r"[\"']?\s*(?:https?:)?//([a-z0-9.-]+)", re.I),
    re.compile(r"(?:url\(|@import)\s*[\"']?\s*(?:https?:)?//([a-z0-9.-]+)", re.I),
    re.compile(r"\b(?:fetch|import|WebSocket|EventSource|sendBeacon)\s*\(\s*[\"'`](?:(?:https?|wss?):)?//([a-z0-9.-]+)",
               re.I),
    re.compile(r"\.open\s*\(\s*[\"'](?:GET|POST|PUT|PATCH|DELETE)[\"']\s*,\s*[\"'`](?:https?:)?//([a-z0-9.-]+)", re.I),
    re.compile(r"\bfrom\s*[\"'](?:https?:)?//([a-z0-9.-]+)", re.I),
]


def load_issues(html: str) -> list[str]:
    """Code or data the file would load from anywhere but a pinned npm package on jsdelivr. Not a repair hint
    but a gate: a version with one of these is never saved (AppMaker._run), because a script from outside runs
    with the app's access to the owner's data."""
    issues = []
    hosts = {h.lower() for rx in _LOADS for h in rx.findall(html)}
    for host in sorted(h for h in hosts if not any(h == a or h.endswith("." + a) for a in ALLOWED_HOSTS)):
        issues.append(f"The app loads from {host}; the only address it may load from is cdn.jsdelivr.net "
                      "(everything else inline).")
    for path in sorted({(m.group(1) or "/") for m in _JSDELIVR_URL.finditer(html)}):
        if not PINNED_LIB_RE.match(path):
            issues.append(f"The app loads https://cdn.jsdelivr.net{path[:80]}, which is not an npm package at an "
                          "exact version, so the code behind it can change after the app was tested. Load "
                          "https://cdn.jsdelivr.net/npm/<package>@<exact version>/<file>, or do it inline.")
    return issues


def static_issues(html: str) -> list[str]:
    """What is wrong with a file before it is even run: the storage contract, its name, where it loads from."""
    issues = []
    if not re.search(r"<title[^>]*>\s*\S", html, re.I):
        issues.append("The file has no <title>, so the app has no name in the owner's list.")
    if not re.search(r"\beve\s*\.\s*load\s*\(", html):
        issues.append("The app never calls eve.load(), so it cannot show anything it saved before.")
    if not re.search(r"\beve\s*\.\s*save\s*\(", html):
        issues.append("The app never calls eve.save(), so nothing the owner enters survives closing it.")
    return issues + load_issues(html)


def _sample(value: Any, depth: int = 0) -> Any:
    """``value`` with long lists and strings shortened, keeping its shape readable for a migration."""
    if depth > 8:
        return "…"
    if isinstance(value, list):
        if len(value) > 8:
            return ([_sample(v, depth + 1) for v in value[:4]] + [f"… {len(value) - 6} more items …"]
                    + [_sample(v, depth + 1) for v in value[-2:]])
        return [_sample(v, depth + 1) for v in value]
    if isinstance(value, dict):
        items = list(value.items())
        out = {str(k): _sample(v, depth + 1) for k, v in items[:40]}
        if len(items) > 40:
            out["…"] = f"{len(items) - 40} more keys, e.g. {', '.join(repr(k) for k, _ in items[-3:])}"
        return out
    if isinstance(value, str) and len(value) > 200:
        return value[:200] + "…"
    return value


def data_sample(value: Any, limit: int = DATA_SAMPLE_CHARS) -> str:
    raw = json.dumps(value, ensure_ascii=False)
    if len(raw) <= limit:
        return raw
    text = json.dumps(_sample(value), ensure_ascii=False)
    return text if len(text) <= limit else text[:limit] + " …(cut)"


class AppStore:
    """The owner's apps on disk: ``<root>/<slug>/index.html``, ``meta.json``, ``data.json``, ``journeys.json`` and
    ``versions/``. A page's journeys (jev_verify.py) are versioned with it: ``versions/<n>.html`` keeps its
    ``versions/<n>.journeys.json`` beside it, so an undo restores both. An app built before journeys has none.

    Every path is built from a slug that matches SLUG_RE in full and is not a symlink, so nothing a request or
    a URL carries can reach outside ``root``."""

    def __init__(self, root: Path = DEFAULT_ROOT) -> None:
        self.root = Path(root)

    def _dir(self, slug: str) -> Optional[Path]:
        if not isinstance(slug, str) or not SLUG_RE.fullmatch(slug):
            return None
        d = self.root / slug
        return None if d.is_symlink() else d

    def _app_dir(self, slug: str) -> Path:
        """The folder of an app that exists, else KeyError."""
        d = self._dir(slug)
        if d is None or not (d / "index.html").is_file():
            raise KeyError(slug)
        return d

    def _meta(self, d: Path) -> dict[str, Any]:
        try:
            m = json.loads((d / "meta.json").read_text())
        except (OSError, ValueError):
            return {}
        if isinstance(m, dict) and "requests" not in m and m.get("request"):      # meta written before requests
            m["requests"] = [{"at": m.get("created", ""), "text": str(m["request"])}]
        return m if isinstance(m, dict) else {}

    def _write_meta(self, d: Path, meta: dict[str, Any]) -> None:
        meta.pop("request", None)
        tmp = d / "meta.tmp"
        tmp.write_text(json.dumps(meta, ensure_ascii=False))
        tmp.replace(d / "meta.json")

    def _versions(self, d: Path) -> list[Path]:
        """Earlier versions, oldest first. The single ``index.prev.html`` of the first builds becomes version 1."""
        vdir = d / "versions"
        prev = d / "index.prev.html"
        if prev.is_file():
            vdir.mkdir(exist_ok=True)
            n = max((int(p.stem) for p in vdir.glob("*.html") if p.stem.isdigit()), default=0)
            if n == 0:
                prev.replace(vdir / "1.html")
            else:
                prev.unlink()
        return sorted((p for p in vdir.glob("*.html") if p.stem.isdigit()), key=lambda p: int(p.stem))

    def list(self) -> list[AppInfo]:
        apps = []
        for meta in self.root.glob("*/meta.json"):
            info = self.info(meta.parent.name)
            if info is not None:
                apps.append(info)
        return sorted(apps, key=lambda a: a.updated, reverse=True)

    @staticmethod
    def _data_v(d: Path) -> Optional[int]:
        """The ``v`` of the app's saved data, read from the start of data.json only (every app keeps one root
        object that starts with it, as the prompt asks): cheap enough for every listing. None when unknown."""
        try:
            with (d / "data.json").open("rb") as f:
                m = _DATA_V_RE.match(f.read(256))
        except OSError:
            return None
        return int(m.group(1)) if m else None

    def info(self, slug: str) -> Optional[AppInfo]:
        d = self._dir(slug)
        if d is None or not (d / "index.html").is_file():
            return None
        m = self._meta(d)
        versions = self._versions(d)
        warn = False
        if versions:
            seen = m.get("data_v")
            then = seen.get(versions[-1].stem) if isinstance(seen, dict) else None
            now = self._data_v(d) if isinstance(then, int) else None
            warn = now is not None and now > then
        try:
            return AppInfo(slug, str(m["title"]), str(m["created"]), str(m["updated"]), icon=str(m.get("icon", "")),
                           description=str(m.get("description", "")), versions=len(versions), undo_warning=warn)
        except KeyError:
            return None

    def html(self, slug: str) -> Optional[str]:
        d = self._dir(slug)
        try:
            return (d / "index.html").read_text() if d is not None else None
        except OSError:
            return None

    def journeys(self, slug: str) -> Optional[list[jev_verify.Journey]]:
        """The journeys of the app's current page, or None when it has none (an app built before them)."""
        d = self._dir(slug)
        try:
            raw = json.loads((d / "journeys.json").read_text()) if d is not None else None
        except (OSError, ValueError):
            return None
        if raw is None:
            return None
        found, _ = jev_verify.parse_journeys(raw)
        return found or None

    @staticmethod
    def _journeys_of(version: Path) -> Path:
        return version.with_name(f"{version.stem}.journeys.json")

    def requests(self, slug: str) -> list[dict[str, str]]:
        """What the owner asked of this app, oldest first."""
        d = self._dir(slug)
        return list(self._meta(d).get("requests", [])) if d is not None else []

    @staticmethod
    def _exact(apps: list[AppInfo], name: str) -> list[AppInfo]:
        """The apps ``name`` names exactly. A slug is checked across every app before any title: two builds of
        "Notes" are ``notes`` and ``notes-2``, both titled Notes, and "notes" is the first one."""
        wanted = (name or "").strip().lower()
        s = slugify(name or "")
        for test in (lambda a: a.slug == wanted, lambda a: a.title.lower() == wanted, lambda a: a.slug == s):
            hits = [a for a in apps if test(a)]
            if hits:
                return hits
        return []

    @staticmethod
    def _words(text: str) -> set[str]:
        """The words that tell apps apart ("my habit trackers" -> {habit, tracker})."""
        return {w[:-1] if len(w) > 3 and w.endswith("s") else w
                for w in re.findall(r"[a-z0-9]+", (text or "").lower())} - _FILLER

    def find(self, name: str) -> Optional[AppInfo]:
        """The app a request names ("the habit tracker", "habit-tracker"): exact slug, then exact title, else the
        best word overlap (a tie goes to the newest). For a change, which keeps the old page as a version; a
        delete or an undo goes through ``resolve``."""
        apps = self.list()
        exact = self._exact(apps, name)
        if exact:
            return exact[0]
        words = self._words(name)
        scored = [(len(words & self._words(a.title + " " + a.slug)), a) for a in apps]
        scored = [x for x in scored if x[0] > 0]
        return max(scored, key=lambda x: x[0])[1] if scored else None

    def resolve(self, name: str) -> tuple[Optional[AppInfo], list[AppInfo]]:
        """The one app ``name`` surely means, for a delete, undo or rename: an exact slug or title, or the only
        app whose name holds every word asked for. Else (None, the apps it might mean), so the owner is asked
        which; nothing is picked by a guess."""
        return self.pick(self.list(), name)

    @classmethod
    def pick(cls, apps: list[AppInfo], name: str) -> tuple[Optional[AppInfo], list[AppInfo]]:
        """``resolve`` over any list of apps (the trash's, for a restore)."""
        exact = cls._exact(apps, name)
        if len(exact) == 1:
            return exact[0], []
        if exact:
            return None, exact
        words = cls._words(name)
        if not words:
            return None, []
        full = [a for a in apps if words <= cls._words(a.title + " " + a.slug)]
        if len(full) == 1:
            return full[0], []
        return None, full or [a for a in apps if words & cls._words(a.title + " " + a.slug)]

    def save(self, html: str, request: str, slug: str = "",
             journeys: Optional[list[jev_verify.Journey]] = None,
             build: Optional[dict[str, Any]] = None) -> AppInfo:
        """Keep ``html`` as the app ``slug`` (a new app when empty), with its ``journeys`` (None: this page has
        none). The page it replaces becomes the newest version with its journeys (at most MAX_VERSIONS are
        kept); the title follows the page's <title> unless the owner renamed the app. Each save is the app's
        next version number (``meta["version"]``); ``build`` is how the build that made the page went
        (``build_record``), kept with that number for the app's change chat (``thread``)."""
        now = _dt.datetime.now().isoformat(timespec="seconds")
        if not slug:
            base = slugify(title_of(html, fallback=request[:40] or "App"))
            slug, n = base, 2
            while (self.root / slug).exists():
                slug, n = f"{base}-{n}"[:48], n + 1
        d = self._dir(slug)
        if d is None:
            raise ValueError("bad app name")
        d.mkdir(parents=True, exist_ok=True)
        meta = self._meta(d)
        # The new page is written in full before the live one moves: a disk that fills up, or any other failed
        # write, leaves the app as it was instead of an app with no page (which would vanish from the list).
        tmp, live = d / "index.tmp", d / "index.html"
        try:
            tmp.write_text(html)
        except BaseException:
            tmp.unlink(missing_ok=True)
            raise
        kept: Optional[Path] = None
        if live.is_file():
            versions = self._versions(d)
            (d / "versions").mkdir(exist_ok=True)
            n = int(versions[-1].stem) + 1 if versions else 1
            kept = d / "versions" / f"{n}.html"
            live.replace(kept)
            if (d / "journeys.json").is_file():
                (d / "journeys.json").replace(self._journeys_of(kept))
            # which shape of data that page was written for: an undo to it can then warn (AppInfo.undo_warning)
            seen = meta.get("data_v") if isinstance(meta.get("data_v"), dict) else {}
            meta["data_v"] = {**seen, str(n): self._data_v(d)}
        try:
            tmp.replace(live)
        except BaseException:
            if kept is not None:
                kept.replace(live)                         # the page that was there comes back
                if self._journeys_of(kept).is_file():
                    self._journeys_of(kept).replace(d / "journeys.json")
            tmp.unlink(missing_ok=True)
            raise
        if journeys:
            jtmp = d / "journeys.tmp"
            jtmp.write_text(json.dumps(jev_verify.journeys_json(journeys), ensure_ascii=False, indent=1))
            jtmp.replace(d / "journeys.json")
        else:
            (d / "journeys.json").unlink(missing_ok=True)
        for old in self._versions(d)[:-MAX_VERSIONS]:
            old.unlink()
            self._journeys_of(old).unlink(missing_ok=True)
        if isinstance(meta.get("data_v"), dict):
            names = {p.stem for p in self._versions(d)}
            meta["data_v"] = {k: v for k, v in meta["data_v"].items() if k in names and isinstance(v, int)}
        before = list(meta.get("requests", []))
        # An app from before version numbers counts its earlier builds (every request but an undo) as versions.
        version = (meta["version"] if isinstance(meta.get("version"), int)
                   else sum(1 for r in before if isinstance(r, dict) and r.get("text") != UNDO_REQUEST)) + 1
        n = self._next(meta)
        requests = before + [{"at": now, "text": request[:500], "n": n}]
        meta.update({"created": meta.get("created", now), "updated": now, "requests": requests[-MAX_REQUESTS:],
                     "version": version})
        if build is not None:
            self._add_build(meta, {**build, "at": now, "request": request[:500], "version": version, "n": n})
        self._describe(meta, html, request)
        self._write_meta(d, meta)
        info = self.info(slug)
        assert info is not None
        return info

    @staticmethod
    def _next(meta: dict[str, Any]) -> int:
        """The next number in the app's own order of events (``meta["seq"]``): two builds in the same second (a
        change that failed at once, then its retry) still show in the order they happened."""
        seq = meta.get("seq") if isinstance(meta.get("seq"), int) else 0
        meta["seq"] = seq + 1
        return seq + 1

    @staticmethod
    def _add_build(meta: dict[str, Any], record: dict[str, Any]) -> None:
        builds = [b for b in meta.get("builds", []) if isinstance(b, dict)]
        meta["builds"] = (builds + [record])[-MAX_BUILDS:]

    def record_build(self, slug: str, request: str, build: dict[str, Any]) -> None:
        """A build of an existing app that did not save a page (Claude failed, or nothing it wrote was usable):
        kept for the app's change chat, so a change that cost money and did nothing still shows there. It is
        not added to the requests, which tell the next build what the app was asked to do. KeyError when the
        app is gone."""
        d = self._app_dir(slug)
        meta = self._meta(d)
        self._add_build(meta, {**build, "at": _dt.datetime.now().isoformat(timespec="seconds"),
                               "request": request[:500], "n": self._next(meta)})
        self._write_meta(d, meta)

    def thread(self, slug: str) -> list[dict[str, Any]]:
        """The app's change chat, oldest first: every request (``kind`` "request", or "undo"), each with how its
        build went (``result``: build_record's fields plus ``version``) when that was recorded, and the builds
        that saved nothing. A request from before results were recorded has ``result`` None. KeyError when the
        app is gone."""
        meta = self._meta(self._app_dir(slug))
        builds = [b for b in meta.get("builds", []) if isinstance(b, dict)]
        order = lambda x: x.get("n") if isinstance(x.get("n"), int) else 0      # noqa: E731 — 0: from before "n"
        # a saved build and its request carry the same number (AppStore.save writes both at once)
        saved = {order(b): b for b in builds if b.get("ok") and order(b)}
        items: list[tuple[tuple[str, int], dict[str, Any]]] = []
        for r in meta.get("requests", []):
            if not isinstance(r, dict):
                continue
            at, text = str(r.get("at", "")), str(r.get("text", ""))
            b = saved.pop(order(r), None) if order(r) else None
            items.append(((at, order(r)), {"at": at, "text": text,
                                           "kind": "undo" if text == UNDO_REQUEST else "request",
                                           "result": _result(b) if b is not None else None}))
        for b in builds:
            # a failed build, or a saved one whose request was trimmed off the (shorter-lived) request list
            if not b.get("ok") or order(b) in saved:
                at = str(b.get("at", ""))
                items.append(((at, order(b)), {"at": at, "text": str(b.get("request", "")), "kind": "request",
                                               "result": _result(b)}))
        return [item for _, item in sorted(items, key=lambda x: x[0])]

    def _describe(self, meta: dict[str, Any], html: str, request: str = "") -> None:
        if not meta.get("renamed"):
            meta["title"] = title_of(html, fallback=meta.get("title") or request[:40] or "App")
        meta["icon"] = icon_of(html) or meta.get("icon", "")
        meta["description"] = description_of(html) or meta.get("description", "")

    def rename(self, slug: str, title: str) -> AppInfo:
        """The owner's name for the app: kept through later changes. The slug (its address) stays."""
        d = self._app_dir(slug)
        title = _clean(title, 60)
        if not title:
            raise ValueError("an app needs a name")
        meta = self._meta(d)
        meta.update({"title": title, "renamed": True})
        self._write_meta(d, meta)
        info = self.info(slug)
        assert info is not None
        return info

    def revert(self, slug: str) -> AppInfo:
        """Undo the last change: the newest earlier version becomes the app again, and leaves the versions. The
        page it replaces is not kept (undo is a pop). KeyError when there is nothing to go back to."""
        d = self._app_dir(slug)
        versions = self._versions(d)
        if not versions:
            raise KeyError(f"{slug} has no earlier version")
        versions[-1].replace(d / "index.html")
        if self._journeys_of(versions[-1]).is_file():
            self._journeys_of(versions[-1]).replace(d / "journeys.json")
        else:
            (d / "journeys.json").unlink(missing_ok=True)       # that page was built before journeys
        html = (d / "index.html").read_text()
        meta = self._meta(d)
        if isinstance(meta.get("data_v"), dict):
            meta["data_v"].pop(versions[-1].stem, None)
        meta["updated"] = _dt.datetime.now().isoformat(timespec="seconds")
        meta["icon"] = meta["description"] = ""
        self._describe(meta, html)
        meta["requests"] = list(meta.get("requests", [])) + [{"at": meta["updated"], "text": UNDO_REQUEST,
                                                               "n": self._next(meta)}]
        self._write_meta(d, meta)
        info = self.info(slug)
        assert info is not None
        return info

    def delete(self, slug: str) -> None:
        """Move the whole app (page, data, versions) to ``<root>/.trash/<slug>-<stamp>/``: out of the list, never
        erased, so a regretted delete can be undone (``restore``)."""
        d = self._app_dir(slug)
        trash = self.root / TRASH
        trash.mkdir(parents=True, exist_ok=True)
        stamp = _dt.datetime.now().strftime("%Y%m%d-%H%M%S")
        target, n = trash / f"{slug}-{stamp}", 2
        while target.exists():
            target, n = trash / f"{slug}-{stamp}-{n}", n + 1
        shutil.move(str(d), str(target))

    def trashed(self) -> list[AppInfo]:
        """Deleted apps that can come back, the most recently deleted first; one per slug (its newest delete)."""
        trash = self.root / TRASH
        found: dict[str, tuple[str, AppInfo]] = {}
        for d in (trash.iterdir() if trash.is_dir() else ()):
            m = _TRASHED_RE.match(d.name)
            if not m or d.is_symlink() or not (d / "index.html").is_file():
                continue
            meta = self._meta(d)
            info = AppInfo(m.group(1), str(meta.get("title") or m.group(1)), str(meta.get("created", "")),
                           str(meta.get("updated", "")), icon=str(meta.get("icon", "")),
                           description=str(meta.get("description", "")))
            key = m.group(2) + d.name                  # when it was deleted, then the -2 of a second delete
            if m.group(1) not in found or key > found[m.group(1)][0]:
                found[m.group(1)] = (key, info)
        return [info for _, info in sorted(found.values(), key=lambda x: x[0], reverse=True)]

    def restore(self, slug: str) -> AppInfo:
        """Bring the newest deleted copy of ``slug`` back from the trash. KeyError when there is none,
        FileExistsError when a live app has that address now (a new app took the name)."""
        if self._dir(slug) is None:
            raise KeyError(slug)
        trash = self.root / TRASH
        copies = sorted((d for d in (trash.iterdir() if trash.is_dir() else ())
                         if (m := _TRASHED_RE.match(d.name)) and m.group(1) == slug and not d.is_symlink()
                         and (d / "index.html").is_file()), key=lambda d: d.name)
        if not copies:
            raise KeyError(slug)
        if (self.root / slug).exists():
            raise FileExistsError(slug)
        shutil.move(str(copies[-1]), str(self.root / slug))
        info = self.info(slug)
        if info is None:
            raise KeyError(slug)
        return info

    def load_data(self, slug: str) -> Any:
        d = self._app_dir(slug)
        try:
            return json.loads((d / "data.json").read_text())
        except (OSError, ValueError):
            return None

    def save_data(self, slug: str, value: Any) -> int:
        d = self._app_dir(slug)
        raw = json.dumps(value)
        if len(raw.encode()) > MAX_DATA_BYTES:
            raise ValueError("too large")
        tmp = d / "data.tmp"
        tmp.write_text(raw)
        tmp.replace(d / "data.json")
        return len(raw)


# ---- Claude writes the app ------------------------------------------------------------------------

@dataclass
class Made:
    ok: bool
    app: Optional[AppInfo] = None
    reason: str = ""
    usd: float = 0.0
    secs: float = 0.0
    check: Optional[app_check.CheckReport] = None      # the check of the version that was kept
    rounds: int = 0                                    # repair rounds used
    issues: list[str] = field(default_factory=list)    # what is still wrong with the kept version, if anything
    history: list[list[str]] = field(default_factory=list)   # the problems each round found, in order


@dataclass
class Generated:
    text: str                 # the answer's text
    usage: Any                # the API's usage, for the bill
    stop_reason: str
    content: Any = None       # the assistant's final content, appended unchanged for a repair round


# progress(stage, n): stage is "thinking", "writing" (n = characters written so far), "testing", "fixing"
# (n = how many problems are being fixed) or "saving".
Progress = Callable[[str, int], None]
Generate = Callable[[list[dict[str, Any]], Progress], Awaitable[Generated]]
Check = Callable[..., Awaitable[app_check.CheckReport]]


def _no_progress(stage: str, n: int) -> None:
    return None


def claude_generate(model: str, effort: str = "high", client: Any = None) -> Generate:
    """One maker turn: stream ``messages`` to Claude under MAKER_PROMPT, reporting "thinking", then "writing"
    with the characters written so far.

    The system prompt is cached for an hour (``SYSTEM_CACHE_TTL``): it is the same for every build, and a first
    round writes for four minutes or more, so the default five-minute cache had expired by the repair round and
    every repair paid to write the whole prefix again. The request's top-level breakpoint (five minutes) covers
    the conversation, which a quick edit-style repair round reads back in time."""
    from . import openrouter

    # Claude through OpenRouter on the owner's key (openrouter.py, 2026-10-08): "claude-opus-5-5" goes out as
    # "anthropic/claude-opus-5.5"; adaptive thinking, effort and cache_control work there as on Anthropic's API.
    api = client                       # built at the first build, so a Mini App starts before the key is read

    async def generate(messages: list[dict[str, Any]], progress: Progress) -> Generated:
        nonlocal api
        if api is None:
            api = openrouter.anthropic_client()
        parts: list[str] = []
        chars = 0
        progress("thinking", 0)
        async with api.messages.stream(
                model=model, max_tokens=MAKE_MAX_TOKENS, messages=messages,
                system=[{"type": "text", "text": MAKER_PROMPT,
                         "cache_control": {"type": "ephemeral", "ttl": SYSTEM_CACHE_TTL}}],
                thinking={"type": "adaptive"}, output_config={"effort": effort},
                cache_control={"type": "ephemeral"}) as s:
            async for event in s:
                if event.type == "text":
                    parts.append(event.text)
                    chars += len(event.text)
                    progress("writing", chars)
                elif event.type == "content_block_start" and getattr(event.content_block, "type", "") == "thinking":
                    progress("thinking", chars)
            final = await s.get_final_message()
        return Generated("".join(parts), final.usage, str(final.stop_reason or ""), final.content)

    return generate


def build_prompt(request: str, context: str) -> str:
    return f"<owner_context>\n{context}\n</owner_context>\n\nBuild this app: {request.strip()}"


def edit_prompt(app: AppInfo, html: str, data: Any, requests: list[dict[str, str]], change: str,
                context: str, journeys: Optional[list[jev_verify.Journey]] = None) -> str:
    asked = "\n".join(f"- {str(r.get('at', ''))[:16].replace('T', ' ')}: {r.get('text', '')}" for r in requests[-12:])
    saved = ("It has no saved data yet." if data is None else
             "Its saved data right now (what eve.load() returns; long lists and strings shortened). The new "
             f"version must open this as it is, migrating it in code if the shape changes:\n```json\n"
             f"{data_sample(data)}\n```")
    return (f"<owner_context>\n{context}\n</owner_context>\n\n"
            f"You are changing the owner's existing app \"{app.title}\".\n\n"
            f"What the owner has asked of it so far, oldest first:\n{asked or '- (not recorded)'}\n\n{saved}\n\n"
            f"The current file:\n```html\n{html}\n```\n\n"
            + (f"Its journeys now:\n```journeys\n{json.dumps(jev_verify.journeys_json(journeys), ensure_ascii=False)}"
               "\n```\n\n" if journeys else "It has no journeys yet.\n\n")
            + f"The change the owner wants now: {change.strip()}\n\n"
            "Return the whole updated file, every line, in one ```html block, keeping everything they did not "
            "ask to change, then the whole ```journeys block for the app as it now is, with made-up example values "
            "(the saved data above stays on the Mac).")


def repair_prompt(issues: list[str], *, tested: bool = True, editable: bool = True) -> str:
    """The problems, and how to answer. With a file to change (``editable``), edits: a repair of one or two
    problems then costs a few hundred tokens and seconds, not the whole file again (25k tokens, minutes)."""
    listed = "\n".join(f"{i}. {text}" for i, text in enumerate(issues, 1))
    how = ("I opened that file on a phone-sized screen with a stand-in Telegram and eve, filled every field, "
           "tapped every button, pressed the MainButton, and reopened the app with the data it saved. "
           if tested else "")
    if not editable:
        return (f"{how}These problems came up:\n{listed}\n\n"
                "Fix the cause of each one and return the whole fixed file, every line, in one ```html block, then "
                "its ```journeys block.")
    return (f"{how}These problems came up:\n{listed}\n\n"
            "Fix the cause of each one and keep everything that works. Answer with edits to the file you wrote "
            "last, one block per change, in this form:\n\n"
            f"{EDIT_START}\n(lines copied exactly from the file, enough of them to be found only once)\n"
            f"{EDIT_MID}\n(the lines that replace them)\n{EDIT_END}\n\n"
            "The blocks apply in order. If the fixes change most of the file, return the whole fixed file, "
            "every line, in one ```html block instead." + JOURNEY_REPAIR)


# A journey problem is the app's until shown otherwise: the maker may correct a journey, never weaken one.
JOURNEY_REPAIR = ("\n\nWhen a journey problem came from the journey itself (the app does the right thing and the journey "
                  "names a control or a result in other words than the app shows), put the whole corrected ```journeys "
                  "block after your fixes. Never change a journey to get around a problem in the app.")


EDIT_START, EDIT_MID, EDIT_END = "<<<<<<< FIND", "=======", ">>>>>>> REPLACE"
_EDIT = re.compile(r"^<{7} ?FIND[^\n]*\n(.*?)\n={7}[ \t]*\n(.*?)\n?>{7} ?REPLACE[^\n]*$", re.S | re.M)


def apply_edits(html: str, answer: str) -> tuple[Optional[str], str]:
    """``html`` with the FIND/REPLACE blocks of ``answer`` applied in order: (the new file, "") or (None, what to
    tell Claude). (None, "") when the answer holds no edits at all. A FIND must occur exactly once; when it
    differs from the file only in the spaces around its lines, the one place it fits is used."""
    edits = _EDIT.findall(answer)
    if not edits:
        return None, ""
    out = html
    for n, (find, replace) in enumerate(edits, 1):
        where = f"Edit {n} of {len(edits)}"
        if not find.strip():
            return None, f"{where} has nothing between {EDIT_START} and {EDIT_MID}, so it cannot be placed."
        count = out.count(find)
        if count == 1:
            out = out.replace(find, replace, 1)
            continue
        loose = re.compile("\n".join(r"[ \t]*" + re.escape(line.strip()) + r"[ \t]*" for line in find.split("\n")))
        spans = [m.span() for m in loose.finditer(out)]
        if count == 0 and len(spans) == 1:
            out = out[:spans[0][0]] + replace + out[spans[0][1]:]
            continue
        found = count or len(spans)
        return None, (f"{where} could not be applied: its FIND lines occur {found} times in the file, not exactly "
                      "once. Copy them exactly from the file you wrote last, with enough lines to be unique, or "
                      "return the whole fixed file in one ```html block.")
    if extract_html(out) is None:
        return None, ("After your edits the file is no longer one complete HTML document. Return the whole fixed "
                      "file in one ```html block.")
    return out, ""


_DATE_LIKE = re.compile(r"^\d{4}-\d{2}-\d{2}")


def owner_values(data: Any, html: str) -> set[str]:
    """The words in the owner's saved data that are theirs: every string and key with a letter, 3 characters or
    more, that the app's own page does not hold (a default category the app ships is not the owner's). Dates are
    left out: a journey that types today's date is not the owner's record."""
    page = html.casefold()
    out: set[str] = set()

    def walk(v: Any, depth: int = 0) -> None:
        if depth > 12 or len(out) > 2000:
            return
        if isinstance(v, dict):
            for k, x in v.items():
                walk(k, depth + 1)
                walk(x, depth + 1)
        elif isinstance(v, list):
            for x in v:
                walk(x, depth + 1)
        elif isinstance(v, str):
            t = " ".join(v.split())
            if 3 <= len(t) <= 80 and re.search(r"[^\W\d_]", t) and not _DATE_LIKE.match(t) and t.casefold() not in page:
                out.add(t)

    walk(data)
    return out


def without_owner_data(journeys: list[jev_verify.Journey], data: Any, html: str,
                       ) -> tuple[list[jev_verify.Journey], list[str]]:
    """The journeys that carry none of the owner's saved data, and a problem for each that does. Jev runs off
    the Mac (jev_verify: the step's words, the values typed and the expected text go to it), and an edit's
    prompt shows Claude a sample of the saved data, which a journey can copy ("type Priya into Name")."""
    mine = owner_values(data, html)
    if not mine:
        return journeys, []
    keep, problems = [], []
    for j in journeys:
        text = " \n ".join([s.target for s in j.steps] + [s.value for s in j.steps] + list(j.expect))
        if any(re.search(r"(?<!\w)" + re.escape(v) + r"(?!\w)", text, re.I) for v in mine):
            problems.append(f'journey "{j.name}" uses the owner\'s saved data; the verifier runs off the Mac, so '
                            "write it with made-up example values. It was not walked.")
        else:
            keep.append(j)
    return keep, problems


def stopped_reason(e: BaseException) -> str:
    """Why a build stopped on an exception, as the change chat keeps it: the Claude API's own reason where
    there is one (a refused key, a rate limit), else that it stopped."""
    from .miniapp import _error_line

    line = _error_line(e) if isinstance(e, Exception) else ""
    return "the build stopped before it finished" if line in ("", "Something went wrong answering that.") \
        else line.rstrip(".")


class AppMaker:
    """Claude builds or changes one app: generate, check (in code, then in a headless phone), repair, keep the
    best version. Every dollar goes to ``spend``; there is no cap (owner: "no claude limit, just track spend")."""

    def __init__(self, store: AppStore, generate: Generate, *, cost: Callable[[Any], float],
                 spend: Callable[[float], Any], context: Optional[Callable[[], str]] = None,
                 check: Optional[Check] = app_check.check_app, max_repairs: int = 2) -> None:
        self.store, self._generate = store, generate
        self._cost, self._spend = cost, spend
        self._context = context or owner_context
        self._check, self.max_repairs = check, max(0, max_repairs)

    async def make(self, request: str, progress: Progress = _no_progress) -> Made:
        return await self._run(build_prompt(request, self._context()), request, "", None, progress, None)

    async def edit(self, name: str, change: str, progress: Progress = _no_progress) -> Made:
        # A slug is taken as it is (the Mini App and the chat door both pass one): the door locked that app,
        # so the build must change that app and no other that happens to share its name.
        app = self.store.info(name) or self.store.find(name)
        if app is None:
            return Made(False, reason=f"there is no app called {name!r}")
        html = self.store.html(app.slug) or ""
        try:
            data = self.store.load_data(app.slug)
        except KeyError:
            data = None
        journeys = self.store.journeys(app.slug)
        prompt = edit_prompt(app, html, data, self.store.requests(app.slug), change, self._context(), journeys)
        t0 = time.perf_counter()
        tally: dict[str, Any] = {"usd": 0.0, "rounds": 0}
        try:
            made = await self._run(prompt, change, app.slug, data, progress, journeys, tally)
        except Exception as e:
            # Claude could not be reached: the change chat still shows the request, why it did nothing, and what
            # the rounds before the failure already cost
            self._record_failed(app.slug, change, Made(False, reason=stopped_reason(e), usd=tally["usd"],
                                                       secs=time.perf_counter() - t0, rounds=tally["rounds"]))
            raise
        if not made.ok:
            self._record_failed(app.slug, change, made)
        return made

    def _record_failed(self, slug: str, change: str, made: Made) -> None:
        try:
            self.store.record_build(slug, change, build_record(
                ok=False, report=made.check, issues=made.issues, usd=made.usd, secs=made.secs, rounds=made.rounds,
                reason=made.reason))
        except (KeyError, OSError) as e:                 # deleted meanwhile, or the disk: never fails the build
            log.info("apps: the failed build of %s was not recorded (%s)", slug, type(e).__name__)

    async def _test(self, html: str, seed: Any, journeys: Optional[list[jev_verify.Journey]] = None,
                    ) -> tuple[list[str], Optional[app_check.CheckReport]]:
        issues = static_issues(html)
        if self._check is None:
            return issues, None
        try:
            # journeys only when there are some: a check written before them takes (html, seed=) alone
            report = await (self._check(html, seed=seed, journeys=journeys) if journeys else
                            self._check(html, seed=seed))
        except Exception as e:  # noqa: BLE001 — a broken checker never fails a build
            log.warning("apps: the check failed to run (%s)", type(e).__name__)
            return issues, None
        return issues + [i for i in report.issues if i not in issues], report

    async def _run(self, prompt: str, request: str, slug: str, seed: Any, progress: Progress,
                   journeys: Optional[list[jev_verify.Journey]], tally: Optional[dict[str, Any]] = None) -> Made:
        """The build loop. ``journeys`` are the ones in hand (an edit's current ones, else None): an answer with a
        usable ```journeys block replaces them, one without keeps them, and the version kept is saved with the
        journeys it was checked with. A block that is empty or does not parse keeps them too, and stays a
        problem until a usable one comes: otherwise it would switch the verifier off for the app. A repair
        round's block never weakens the set: a journey it leaves out is still walked. On an edit (``seed``, the
        saved data), a journey that carries the owner's data is never walked (without_owner_data).
        ``tally`` gets the running cost and rounds, for a caller whose build raises part way."""
        tally = tally if tally is not None else {}
        t0 = time.perf_counter()
        messages: list[dict[str, Any]] = [{"role": "user", "content": prompt}]
        usd = 0.0
        # (untested, problems, -round, file, problems, report, journeys): a version the phone check really ran
        # on beats one it could not (a checker that did not start), whatever their counts; fewer problems next;
        # the later round on a tie.
        best: Optional[tuple[bool, int, int, str, list[str], Optional[app_check.CheckReport],
                             Optional[list[jev_verify.Journey]]]] = None
        why = "Claude did not return a complete app"
        issues: list[str] = []
        history: list[list[str]] = []
        last: Optional[str] = None          # the file Claude wrote last: what a repair's edits apply to
        stuck: list[str] = []               # an unusable journeys block's problems, until a usable one comes
        for round_no in range(self.max_repairs + 1):
            if round_no:
                n = len(issues)
                progress("fixing", n)
                step: Progress = lambda stage, chars, n=n: progress("fixing", n)  # noqa: E731
            else:
                step = progress
            try:
                gen = await self._generate(messages, step)
            except Exception:
                if best is None:
                    raise
                log.warning("apps: a repair round failed; keeping the best version so far", exc_info=True)
                break
            spent = self._cost(gen.usage) if gen.usage is not None else 0.0
            if gen.usage is not None:
                self._spend(spent)
            usd += spent
            tally["usd"] = usd
            if gen.stop_reason == "refusal":
                why = "Claude declined to build that"
                break
            html, bad_edit = apply_edits(last, gen.text) if last is not None else (None, "")
            if html is None and not bad_edit:
                html = extract_html(gen.text)
            found, journey_problems = jev_verify.extract_journeys(gen.text)
            if found:
                if round_no and journeys:
                    names = {j.name for j in found}
                    found = (found + [j for j in journeys if j.name not in names])[:jev_verify.MAX_JOURNEYS]
                journeys, stuck = found, []
            elif found is not None:
                stuck = journey_problems        # the journeys in hand stay
            elif stuck:
                journey_problems = stuck
            if html is None and not bad_edit and last is not None and found is not None:
                html = last                     # a repair that corrected only the journeys
            if html is None:
                cut = gen.stop_reason == "max_tokens"
                why = "the app was too long to finish" if cut else "Claude did not return a complete app"
                issues = [bad_edit or "Your answer was cut off at the length limit before the file was complete. "
                          "Write it again more compactly (shorter CSS, fewer comments, no repetition) so it fits."
                          if cut else bad_edit or "Your answer did not contain one complete HTML document in a "
                          "```html block."]
                tested = False
            else:
                last = html
                if seed is not None and journeys:
                    kept, leaked = without_owner_data(journeys, seed, html)
                    journeys, journey_problems = kept or None, journey_problems + leaked
                progress("testing", len(html))
                issues, report = await self._test(html, seed, journeys)
                if not journeys and not journey_problems:
                    # not a problem to repair (a round costs more than it proves): the build is checked by the
                    # script alone, as an app from before journeys is
                    log.info("apps: round %d came with no journeys; only the scripted check runs", round_no)
                issues = issues + [f"Journeys block: {p}" for p in journey_problems]
                tested = report is not None and not report.skipped
                untested = self._check is not None and not tested
                if load_issues(html):
                    # never kept: a script from outside would run with the owner's data (load_issues)
                    why = "the app kept loading code from outside cdn.jsdelivr.net/npm at an exact version"
                elif best is None or (untested, len(issues), -round_no) < best[:3]:
                    best = (untested, len(issues), -round_no, html, issues, report, journeys)
            history.append(list(issues))
            if html is not None and not issues:
                break
            log.info("apps: round %d found %d problem(s): %s", round_no, len(issues), "; ".join(issues)[:500])
            if round_no == self.max_repairs:
                break
            messages = messages + [{"role": "assistant", "content": gen.content if gen.content is not None
                                    else gen.text},
                                   {"role": "user", "content": repair_prompt(issues, tested=tested,
                                                                             editable=last is not None)}]
            tally["rounds"] = sum(1 for m in messages if m["role"] == "assistant")
        secs = time.perf_counter() - t0
        rounds = sum(1 for m in messages if m["role"] == "assistant")
        if best is None:
            return Made(False, reason=why, usd=usd, secs=secs, rounds=rounds, history=history)
        _, _, _, html, left, report, kept_journeys = best
        if slug and self.store.info(slug) is None:
            return Made(False, reason="the app was deleted while it was being changed", usd=usd, secs=secs,
                        check=report, rounds=rounds, issues=left, history=history)
        progress("saving", len(html))
        app = self.store.save(html, request, slug, journeys=kept_journeys,
                              build=build_record(ok=True, report=report, issues=left, usd=usd, secs=secs,
                                                 rounds=rounds))
        log.info("apps: %s %s in %.0f s, %d repair round(s), %d problem(s) left, $%.3f",
                 "edited" if slug else "made", app.slug, secs, rounds, len(left), usd)
        return Made(True, app=app, usd=usd, secs=secs, check=report, rounds=rounds, issues=left, history=history)


# ---- window.eve, injected into every app --------------------------------------------------------

# Saves go out one at a time and the newest value wins: three quick taps are at most one request in flight plus
# one waiting (holding the latest value), so a slow earlier save can never land after a newer one. Each is sent
# with keepalive when small enough (the browser's limit is 64 KB), so a save started just before Telegram
# closes the app still arrives.
#
# The page is sandboxed (miniapp.APP_CSP): its requests come from an opaque origin, so they are sent as
# text/plain, which keeps them CORS-simple (no preflight, which keepalive requests cannot always make). They
# carry the app token the page was opened with (``?t=``, miniapp.app_token), read once before the app's own
# code runs; it answers for this app's load and save and nothing else. The app never has the owner's initData.
#
# Telegram's Back button also leads home: on the app's own first view (where the app hides the button) it is
# kept showing and goes back to eve's list of apps, so the owner can reach Change, Undo and the other apps
# without closing the Mini App. On any other view the app's own handlers run, as the app registered them.
EVE_JS = """(() => {
  const slug = location.pathname.split("/").filter(Boolean)[1] || "";
  const token = new URLSearchParams(location.search).get("t") || "";
  const body = (rest) => '{"t":' + JSON.stringify(token) + rest + "}";
  async function post(path, raw) {
    const r = await fetch(path, { method: "POST", headers: { "Content-Type": "text/plain" }, body: raw,
      keepalive: raw.length < 60000 });
    const j = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(j.error || ("HTTP " + r.status));
    return j;
  }
  let busy = false, next = null;       // next: { data, waiting: [[resolve, reject], ...] }, the newest value
  function pump() {
    if (busy || !next) return;
    const job = next; next = null; busy = true;
    post("/api/apps/" + slug + "/save", body(',"data":' + job.data))
      .then(() => job.waiting.forEach((w) => w[0](true)), (e) => job.waiting.forEach((w) => w[1](e)))
      .finally(() => { busy = false; pump(); });
  }
  window.eve = {
    slug,
    load: async () => (await post("/api/apps/" + slug + "/load", body(""))).data ?? null,
    save: (value) => new Promise((resolve, reject) => {
      const data = JSON.stringify(value);  // a snapshot now: later changes to the object are the next save's
      if (data === undefined) { reject(new Error("eve.save needs a JSON value")); return; }
      next = next || { waiting: [] };
      next.data = data; next.waiting.push([resolve, reject]);
      pump();
    }),
  };
  // What this page does on the real phone, sent once when it has settled and again after a new error (at most 3
  // reports): the Telegram it runs in, whether Telegram's native bridge is here (without it the MainButton and
  // BackButton do nothing), the MainButton's state, and the page's errors. The server logs it (phone_report).
  const errors = [];
  let reports = 0, timer = null;
  function report() {
    timer = null;
    if (reports >= 3) return;
    reports++;
    const w = window.Telegram && window.Telegram.WebApp, mb = w && w.MainButton;
    const bridge = window.TelegramWebviewProxy ? "proxy" : (window.external && window.external.notify) ? "external"
      : (window.parent !== window) ? "iframe" : "none";
    const info = { platform: w ? w.platform : "", version: w ? w.version : "", bridge,
      main_button: mb ? (mb.isVisible ? "shown " + JSON.stringify(mb.text || "") : "hidden") : "", errors: errors.slice(-5) };
    post("/api/apps/" + slug + "/report", body("," + JSON.stringify(info).slice(1, -1))).catch(() => {});
  }
  const later = (ms) => { if (!timer) timer = setTimeout(report, ms); };
  window.addEventListener("error", (e) => { errors.push(String(e.message || e.error || "error") + (e.lineno ? " @" + e.lineno : "")); later(1000); });
  window.addEventListener("unhandledrejection", (e) => { errors.push("promise: " + String((e.reason && e.reason.message) || e.reason)); later(1000); });
  window.addEventListener("load", () => later(3000));
  const tg = window.Telegram && window.Telegram.WebApp, bb = tg && tg.BackButton;
  if (bb && tg.isVersionAtLeast && tg.isVersionAtLeast("6.1")) {
    const show = bb.show, on = bb.onClick, own = [];
    let appShows = false;              // the app wants Back (a view below its first): its handlers run
    on.call(bb, () => { if (appShows) own.slice().forEach((f) => f()); else location.href = "/"; });
    show.call(bb);
    bb.show = () => { appShows = true; return bb; };
    bb.hide = () => { appShows = false; return bb; };
    bb.onClick = (f) => { if (typeof f === "function" && !own.includes(f)) own.push(f); return bb; };
    bb.offClick = (f) => { const i = own.indexOf(f); if (i >= 0) own.splice(i, 1); return bb; };
  }
  // The way to change this app: its chat on eve's home page (/?chat=<slug>), where the owner's signed
  // initData lives. The app only navigates there; it is never given the power to change apps or spend.
  // Telegram's Settings item in the Mini App's "..." menu (7.0+) goes there, and so does a small pencil in the
  // page's top right corner, beside the app's title, that scrolls away with the page (the maker is told to
  // keep that corner free, and app_check reports a control under it). The pencil lives in a
  // closed shadow root on <html>, with its host's position set inline and !important, so the app's own CSS
  // (a "button { ... }" rule, a body that is re-rendered) cannot restyle or remove it by accident.
  const toChat = () => { location.href = "/?chat=" + encodeURIComponent(slug); };
  const sb = tg && tg.SettingsButton;
  if (sb && tg.isVersionAtLeast && tg.isVersionAtLeast("7.0")) {
    try { sb.onClick(toChat); sb.show(); } catch (e) {}
  }
  function pencil() {
    if (!slug || document.querySelector("eve-change")) return;
    const host = document.createElement("eve-change");
    host.setAttribute("style", "all: initial !important; position: absolute !important; z-index: 2147483647 !important; "
      + "display: block !important; width: 44px !important; height: 44px !important; "
      + "top: calc(6px + var(--tg-safe-area-inset-top, 0px) + var(--tg-content-safe-area-inset-top, 0px)) !important; "
      + "right: calc(6px + var(--tg-safe-area-inset-right, 0px) + var(--tg-content-safe-area-inset-right, 0px)) !important;");
    const root = host.attachShadow({ mode: "closed" });
    root.innerHTML = '<style>button { all: initial; box-sizing: border-box; width: 44px; height: 44px; display: grid; '
      + 'place-items: center; cursor: pointer; border-radius: 22px; -webkit-tap-highlight-color: transparent; } '
      + 'span { width: 32px; height: 32px; border-radius: 16px; display: grid; place-items: center; '
      + 'font: 17px/1 -apple-system, system-ui, sans-serif; opacity: .8; '
      + 'background: var(--tg-theme-secondary-bg-color, rgba(127, 127, 127, .16)); '
      + 'color: var(--tg-theme-link-color, #2481cc); } '
      + 'button:focus-visible span { outline: 2px solid var(--tg-theme-link-color, #2481cc); outline-offset: 2px; } '
      + '</style><button type="button" aria-label="Change this app" title="Change this app">'
      + '<span aria-hidden="true">\u270e</span></button>';
    const btn = root.querySelector("button");
    btn.addEventListener("click", toChat);
    document.documentElement.appendChild(host);
    // The corner belongs to the app first: while one of its own controls is under the pencil (a header's
    // Settings button, a dialog's close), the pencil is hidden, so a tap there reaches the app. Checked again
    // after every change to the page, a scroll or a resize, at most once a frame.
    const TAP = "button,a[href],input,select,textarea,label,summary,[role=button],[role=tab],[role=link],"
      + "[role=checkbox],[role=switch],[role=radio],[role=menuitem],[role=option],[onclick]";
    let queued = false, hidden = false;
    const under = () => {
      const r = host.getBoundingClientRect();
      if (r.bottom <= 0 || r.top >= innerHeight) return hidden;      // scrolled away: nothing to decide
      const pts = [[r.left + r.width / 2, r.top + r.height / 2], [r.left + 8, r.top + 8], [r.right - 8, r.top + 8],
        [r.left + 8, r.bottom - 8], [r.right - 8, r.bottom - 8]];
      return pts.some(([x, y]) => {
        const el = document.elementsFromPoint(x, y).find((e) => e !== host);
        return !!(el && el !== document.documentElement && el !== document.body && el.closest(TAP));
      });
    };
    const settle = () => {
      queued = false;
      const now = under();
      if (now === hidden) return;
      hidden = now;                    // the host and its button both: the button's "all: initial" is visible
      [host, btn].forEach((e) => e.style.setProperty("visibility", now ? "hidden" : "visible", "important"));
    };
    const later = () => { if (!queued) { queued = true; requestAnimationFrame(settle); } };
    new MutationObserver(later).observe(document.documentElement, { subtree: true, childList: true, attributes: true,
      characterData: true });
    addEventListener("scroll", later, { passive: true, capture: true });
    addEventListener("resize", later);
    settle();
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", pencil); else pencil();
})();
"""


def serve_app_html(html: str) -> str:
    """The app as served: Telegram's script and window.eve put in the head, ahead of the app's own code."""
    inject = ""
    if "telegram-web-app.js" not in html:
        inject += '<script src="https://telegram.org/js/telegram-web-app.js"></script>'
    inject += '<script src="/eve.js"></script>'
    m = re.search(r"<head[^>]*>", html, re.I)
    return html[:m.end()] + inject + html[m.end():] if m else inject + html


# ---- the chat door: "make me a habit tracker" texted to eve ---------------------------------------

def _app_arg(description: str = "Which app, by its name.") -> dict[str, Any]:
    return {"type": "string", "description": description}


MAKER_TOOLS: list[dict[str, Any]] = [
    {
        "type": "function", "name": "make_app", "strict": True,
        "description": "Build a NEW small app (a tracker, a log, a list, a calculator, a planner…) that opens inside "
                       "Telegram and saves its data. Use it whenever the owner asks for an app, tracker or tool "
                       "to be made. Takes a few minutes; the app's picture and an Open button are sent to the chat "
                       "when it is ready.",
        "parameters": {"type": "object", "additionalProperties": False, "required": ["request"],
                       "properties": {"request": {"type": "string", "description":
                                                  "What the app should do, in the owner's words plus any detail "
                                                  "they gave."}}},
    },
    {
        "type": "function", "name": "change_app", "strict": True,
        "description": "Change one of the owner's EXISTING apps: add a feature, fix something, restyle it. Its "
                       "saved data is kept. The new version's picture and Open button are sent when it is ready.",
        "parameters": {"type": "object", "additionalProperties": False, "required": ["app", "change"],
                       "properties": {"app": _app_arg(), "change": {"type": "string", "description": "What to change."}}},
    },
    {
        "type": "function", "name": "undo_app", "strict": True,
        "description": "Put one of the owner's apps back to how it was before its last change (undo, revert, "
                       "go back, \"the last change broke it\"). Its saved data is untouched. When the name fits no "
                       "app or more than one, nothing is undone and the candidates come back: ask which.",
        "parameters": {"type": "object", "additionalProperties": False, "required": ["app"],
                       "properties": {"app": _app_arg()}},
    },
    {
        "type": "function", "name": "rename_app", "strict": True,
        "description": "Give one of the owner's apps a new name. Only the name changes.",
        "parameters": {"type": "object", "additionalProperties": False, "required": ["app", "title"],
                       "properties": {"app": _app_arg("Which app, by its current name."),
                                      "title": {"type": "string", "description": "The new name."}}},
    },
    {
        "type": "function", "name": "delete_app", "strict": True,
        "description": "Delete one of the owner's apps when they ask to delete or remove it. Code asks for "
                       "approval before deletion. It moves to eve's trash and restore_app brings it back. When the name fits no "
                       "app or more than one, nothing is deleted and the candidates come back: ask which.",
        "parameters": {"type": "object", "additionalProperties": False, "required": ["app"],
                       "properties": {"app": _app_arg()}},
    },
    {
        "type": "function", "name": "restore_app", "strict": True,
        "description": "Bring back an app the owner deleted (\"undelete\", \"I deleted it by mistake\"), with its "
                       "saved data.",
        "parameters": {"type": "object", "additionalProperties": False, "required": ["app"],
                       "properties": {"app": _app_arg("Which deleted app, by its name.")}},
    },
    {
        "type": "function", "name": "list_apps", "strict": True,
        "description": "The apps eve has made for the owner, and a button to open each.",
        "parameters": {"type": "object", "additionalProperties": False, "required": [], "properties": {}},
    },
]
MAKER_TOOL_NAMES = frozenset(t["name"] for t in MAKER_TOOLS)

SendButtons = Callable[[int, str, list[tuple[str, str]]], Awaitable[Any]]
SendPhoto = Callable[[int, bytes, str, list[tuple[str, str]]], Awaitable[Any]]


class ChatMaker:
    """The Telegram brain's app tools, over a live Mini App (miniapp.MiniApp).

    A build runs in the background like a Mac task: the tool returns at once, and when it is done the app's
    screenshot (from the check) goes to the chat with an Open button under it (an inline ``web_app`` button,
    so it opens as a Mini App with the owner's signed initData); a change also gets a Change button to the
    app's change chat on the home page, to keep refining it. Undo, rename, delete and restore are instant;
    the first three act only on an app the owner named for sure (AppStore.resolve), never on a guess."""

    def __init__(self, app: Any, spawn: Callable[[Awaitable[Any], str], Any]) -> None:
        # miniapp.MiniApp: .maker, .store, .app_url(slug), .chat_url(slug), .url
        self._app, self._spawn = app, spawn
        self.building: set[str] = set()

    @property
    def live(self) -> bool:
        return bool(getattr(self._app, "url", ""))

    def tools(self) -> list[dict[str, Any]]:
        return list(MAKER_TOOLS) if self.live else []

    def home_url(self) -> str:
        """The Mini App's home screen right now ("" while the tunnel is down)."""
        return f"{self._app.url}/" if self.live else ""

    async def stick_link(self, rotate: bool = False) -> str:
        """The owner's /stick (stick_link.py): Eve Link's pairing link, "" while the tunnel is down."""
        return await self._app.stick_link(rotate) if self.live else ""

    def _missing(self, target: str, candidates: Optional[list[AppInfo]] = None) -> dict[str, Any]:
        """Nothing was done: the name fits no app, or more than one. The brain asks the owner which."""
        if candidates:
            named = [a.title if [c.title for c in candidates].count(a.title) == 1 else f"{a.title} ({a.slug})"
                     for a in candidates[:8]]
            return {"ok": False, "reason": f"{target!r} could mean more than one app; nothing was done. Ask the "
                                           "owner which, then call again with its exact name.",
                    "candidates": named}
        names = ", ".join(a.title for a in self._app.store.list()) or "none yet"
        return {"ok": False, "reason": f"no app called {target!r}; nothing was done. The apps are: {names}"}

    async def handle(self, name: str, args: dict[str, Any], chat_id: int, send: SendButtons,
                     say: Callable[[int, str], Awaitable[Any]], send_photo: Optional[SendPhoto] = None
                     ) -> dict[str, Any]:
        if not self.live:
            return {"ok": False, "reason": "the Mini App is not running right now"}
        store = self._app.store
        if name == "list_apps":
            apps = store.list()
            if not apps:
                return {"ok": True, "apps": [], "note": "no apps yet"}
            await send(chat_id, "Your apps:",
                       [(f"{a.icon} {a.title}".strip(), self._app.app_url(a.slug)) for a in apps[:8]])
            return {"ok": True, "apps": [a.title for a in apps], "sent": "the buttons are in the chat already"}
        target = str(args.get("app") or "").strip()
        if name == "restore_app":
            return await self._restore(target, chat_id, send)
        if name in ("delete_app", "rename_app", "undo_app"):
            # Acted on at once, so never on a guess: "delete the water app" with Water Log and Water Plants,
            # or "undo the water tracker" when only a Habit Tracker is left, does nothing and asks.
            app, candidates = store.resolve(target) if target else (None, [])
            if app is None:
                return self._missing(target, candidates)
            if app.slug in self.building or app.title in self.building:
                return {"ok": False, "reason": f"{app.title} is being changed right now; try again when it is done"}
            if name == "delete_app":
                store.delete(app.slug)
                return {"ok": True, "deleted": app.title,
                        "note": "moved to eve's trash (apps/.trash on the Mac) with its data; restore_app "
                                "brings it back"}
            if name == "rename_app":
                try:
                    renamed = store.rename(app.slug, str(args.get("title") or ""))
                except ValueError as e:
                    return {"ok": False, "reason": str(e)}
                return {"ok": True, "renamed": {"from": app.title, "to": renamed.title}}
            try:
                back = store.revert(app.slug)
            except KeyError:
                return {"ok": False, "reason": f"{app.title} has no earlier version to go back to"}
            url = self._app.app_url(back.slug)
            line = f"{back.title} is back to how it was before its last change."
            if app.undo_warning:
                line += (" Entries saved since that change may not open in this version; changing the app again "
                         "brings them back.")
            if url:
                await send(chat_id, line, [(f"Open {back.title}", url)])
            out = {"ok": True, "undone": back.title, "earlier_versions_left": back.versions,
                   "sent": "an Open button is in the chat already"}
            if app.undo_warning:
                out["note"] = "its saved data has a newer shape than this version knows; said so in the chat"
            return out
        request = str(args.get("request") or args.get("change") or "").strip()
        if name != "change_app":
            target = ""
        if not request:
            return {"ok": False, "reason": "say what the app should do"}
        if target:
            found = store.find(target)
            if found is None:
                return self._missing(target)
            target = found.slug
        key = target or request
        if key in self.building:
            return {"ok": False, "reason": "that one is already being built"}
        self.building.add(key)
        self._spawn(self._build(chat_id, request, target, key, send, say, send_photo), "apps-build")
        return {"ok": True, "sent": "building now (a few minutes, tested on a phone before it is sent); a short "
                                    "line comes when it is being tested, and the app's picture and Open button "
                                    "when it is ready — say only a word or two"}

    async def _restore(self, target: str, chat_id: int, send: SendButtons) -> dict[str, Any]:
        store = self._app.store
        gone = store.trashed()
        app, candidates = AppStore.pick(gone, target)
        if app is None:
            if candidates:
                return self._missing(target, candidates)
            names = ", ".join(a.title for a in gone[:8]) or "none"
            return {"ok": False, "reason": f"no deleted app called {target!r}; the deleted apps are: {names}"}
        try:
            back = store.restore(app.slug)
        except FileExistsError:
            return {"ok": False, "reason": f"a new app now has {app.title}'s address, so it cannot come back "
                                           "under it; rename or delete the new one first"}
        except KeyError:
            return {"ok": False, "reason": f"{app.title} is no longer in the trash"}
        url = self._app.app_url(back.slug)
        if url:
            await send(chat_id, f"{back.icon} {back.title} is back, with its data.".strip(),
                       [(f"Open {back.title}", url)])
        return {"ok": True, "restored": back.title, "sent": "an Open button is in the chat already"}

    def _progress(self, chat_id: int, say: Callable[[int, str], Awaitable[Any]]) -> Progress:
        """A short line when a chat build reaches the phone test and each repair round, never more often than
        CHAT_PROGRESS_SECS: a build runs for minutes, and silence reads as a build that died."""
        state: dict[str, Any] = {"stage": "", "tested": False, "at": -CHAT_PROGRESS_SECS}
        tasks: set[asyncio.Task] = set()

        def settled(task: asyncio.Task) -> None:
            tasks.discard(task)
            if not task.cancelled() and task.exception() is not None:
                log.info("apps: a progress line did not reach the chat (%s)", type(task.exception()).__name__)

        def progress(stage: str, n: int) -> None:
            before, state["stage"] = state["stage"], stage
            if stage == before or stage not in ("testing", "fixing") or (stage == "testing" and state["tested"]):
                return                                     # the first test, then each repair round; no more
            state["tested"] = True
            if time.monotonic() - state["at"] < CHAT_PROGRESS_SECS:
                return
            state["at"] = time.monotonic()
            line = ("Written. Testing it on a phone…" if stage == "testing" else
                    f"The phone test found {n} {'problem' if n == 1 else 'problems'}; fixing "
                    f"{'it' if n == 1 else 'them'}…")
            try:
                task = asyncio.get_running_loop().create_task(say(chat_id, line))
            except RuntimeError:
                return
            tasks.add(task)
            task.add_done_callback(settled)

        return progress

    async def _build(self, chat_id: int, request: str, target: str, key: str, send: SendButtons,
                     say: Callable[[int, str], Awaitable[Any]], send_photo: Optional[SendPhoto]) -> None:
        info = self._app.store.info(target) if target else None
        # a change that fails leaves the app as it was, and the line says so
        failed = f"I couldn't change {info.title if info else target}; it is unchanged" if target \
            else "I couldn't build that app"
        progress = self._progress(chat_id, say)

        async def fail(line: str) -> None:
            # a change that failed leads to the app's change chat too: its thread shows the try, and it is where
            # to ask again in other words
            chat = self._app.chat_url(target) if target and self._app.store.info(target) is not None else ""
            await (send(chat_id, line, [("Change", chat)]) if chat else say(chat_id, line))

        try:
            made = await (self._app.maker.edit(target, request, progress) if target
                          else self._app.maker.make(request, progress))
        except Exception as e:  # noqa: BLE001 — a failed build is a line in the chat
            from .miniapp import _error_line

            log.warning("apps: building from the chat failed (%s)", type(e).__name__)
            await fail(f"{failed}: {_error_line(e)}")
            return
        finally:
            self.building.discard(key)
        if not made.ok or made.app is None:
            await fail(f"{failed}: {made.reason}.")
            return
        app = made.app
        url = self._app.app_url(app.slug)
        if not url:
            await say(chat_id, f"{app.title} is ready. Open it from the Apps tab on the menu button.")
            return
        caption = f"{app.icon} {app.title} is {'updated' if target else 'ready'}.".strip()
        if app.description:
            caption += f"\n{app.description}"
        if made.issues:
            first = owner_issues(made.check, made.issues[:1], 240)[0]
            caption += (f"\nIt may still have a problem: {first} "
                        + ('Say "undo" to go back.' if target else "Tell me what to fix."))
        buttons = [(f"Open {app.title}", url)]
        if target and self._app.chat_url(app.slug):
            buttons.append(("Change", self._app.chat_url(app.slug)))      # the app's change chat, to keep refining
        shot = made.check.screenshot if made.check is not None else None
        if shot and send_photo is not None:
            try:
                # the picture is the phone test's run: its sample entries, not the owner's data
                await send_photo(chat_id, shot, caption + "\n(Picture: a test run with sample entries.)", buttons)
                return
            except Exception as e:  # noqa: BLE001 — no picture is fine; the button still goes
                log.warning("apps: sending the app's picture failed (%s)", type(e).__name__)
        await send(chat_id, caption, buttons)
