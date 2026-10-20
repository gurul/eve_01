//! A port of `apps_maker.py`, lines 1-1126: the maker's prompt, the skeleton app, the owner's context, the app store.
//!
//! Constants and prompts are verbatim (generated from the Python values).

pub mod maker;
pub use maker::*;

#[allow(unused_imports)]
use crate::prelude::*;



/// py: apps_maker.py:50 `MAX_DATA_BYTES`
pub const MAX_DATA_BYTES: usize = 1000000;

/// py: apps_maker.py:51 `MAX_APP_BYTES`
pub const MAX_APP_BYTES: usize = 600000;

/// py: apps_maker.py:52 `MAKE_MAX_TOKENS`
/// thinking counts toward it too; a whole app is ~10-25k tokens of text
pub const MAKE_MAX_TOKENS: usize = 96000;

/// py: apps_maker.py:53 `SYSTEM_CACHE_TTL`
pub const SYSTEM_CACHE_TTL: &str = "1h";

/// py: apps_maker.py:54 `MAX_VERSIONS`
pub const MAX_VERSIONS: usize = 10;

/// py: apps_maker.py:55 `MAX_REQUESTS`
/// the newest requests kept in meta.json
pub const MAX_REQUESTS: usize = 30;

/// py: apps_maker.py:56 `MAX_BUILDS`
pub const MAX_BUILDS: usize = 30;

/// py: apps_maker.py:57 `DATA_SAMPLE_CHARS`
pub const DATA_SAMPLE_CHARS: usize = 6000;

/// py: apps_maker.py:58 `CHAT_PROGRESS_SECS`
pub const CHAT_PROGRESS_SECS: f64 = 20.0;

/// py: apps_maker.py:59 `SLUG_RE`
/// Python flags 32.
pub static SLUG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[a-z0-9][a-z0-9-]{0,47}"#).expect("SLUG_RE"));

/// py: apps_maker.py:60 `ALLOWED_HOSTS`
pub const ALLOWED_HOSTS: &[&str] = &["cdn.jsdelivr.net", "telegram.org"];

/// py: apps_maker.py:64 `PINNED_LIB_RE`
/// Python flags 34. Written in Python's terms (see "Python's regex classes" below): `\w` is `[\p{L}\p{N}_]`,
/// and under IGNORECASE `[a-z]` also takes "İ" and "ı".
pub static PINNED_LIB_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?i)/npm/(?:@[a-z0-9İı][\p{L}\p{N}_.-]*/)?[a-z0-9İı][\p{L}\p{N}_.-]*@\d+\.\d+\.\d+(?:-[\p{L}\p{N}_.]+)?"#,
        r#"(?:/|(?:\n?\z))"#
    ))
    .expect("PINNED_LIB_RE")
});

/// py: apps_maker.py:65 `_JSDELIVR_URL`
/// Python flags 34. Python's `\s` (with the separators \x1c-\x1f), and its IGNORECASE "i" (also "İ", "ı").
pub(crate) static JSDELIVR_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:https?:)?//cdn\.jsdel[iİı]vr\.net(/[^\s\x1c-\x1f"'`)<>\\]*)?"#).expect("JSDELIVR_URL")
});

/// py: apps_maker.py:66 `_FILLER`
/// A Python set: sorted here; membership is what matters.
pub(crate) const FILLER: &[&str] = &["a", "an", "app", "apps", "my", "one", "that", "the", "this"];

/// py: apps_maker.py:67 `TRASH`
pub const TRASH: &str = ".trash";

/// py: apps_maker.py:68 `UNDO_REQUEST`
/// the request an undo leaves in meta.json
pub const UNDO_REQUEST: &str = "(undo)";

/// py: apps_maker.py:69 `_TRASHED_RE`
/// Python flags 32.
pub(crate) static TRASHED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^([a-z0-9][a-z0-9-]{0,47})-(\d{8}-\d{6})(?:-\d+)?(?:\n?\z)"#).expect("TRASHED_RE"));

/// py: apps_maker.py:70 `_DATA_V_RE`
/// Python flags 32 (a bytes pattern).
pub(crate) static DATA_V_RE: LazyLock<regex::bytes::Regex> =
    LazyLock::new(|| regex::bytes::Regex::new(r#"(?-u)^\s*\{\s*"v"\s*:\s*(\d{1,9})\b"#).expect("DATA_V_RE"));

/// py: apps_maker.py:73 `SKELETON_JOURNEYS`
pub const SKELETON_JOURNEYS: &str = "[
  {\"name\": \"Log two glasses\", \"steps\": [{\"do\": \"tap\", \"target\": \"Add a glass\"}, {\"do\": \"tap\", \"target\": \"Add a glass\"}],
   \"expect\": [\"2 glasses\", \"6 glasses to go of 8\"]},
  {\"name\": \"Lower the goal\", \"steps\": [{\"do\": \"type\", \"target\": \"Daily goal, in glasses\", \"value\": \"5\"},
   {\"do\": \"tap\", \"target\": \"Add a glass\"}], \"expect\": [\"4 glasses to go of 5\"]},
  {\"name\": \"See the week\", \"steps\": [{\"do\": \"tap\", \"target\": \"Add a glass\"},
   {\"do\": \"tap\", \"target\": \"See the last 7 days ›\"}], \"expect\": [\"Last 7 days\", \"1 glass\"]}
]";

/// py: apps_maker.py:85 `SKELETON`
pub const SKELETON: &str = "<!doctype html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">
<title>Water Log</title>
<meta name=\"eve-icon\" content=\"💧\">
<meta name=\"description\" content=\"Count glasses of water and see the week.\">
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
  font: 16px/1.4 -apple-system, BlinkMacSystemFont, \"Segoe UI\", Roboto, sans-serif;
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
<section class=\"card\" id=\"unreadable\" role=\"alert\" hidden>Your saved data couldn't be read, so nothing is saved over it.
  <button class=\"plain\" id=\"retry\">Try again</button></section>
<main id=\"home\">
  <h1>Water Log</h1>
  <section class=\"card\"><div class=\"big\"><span id=\"count\">0</span> <span class=\"unit\">glasses</span></div>
    <div class=\"hint\" id=\"goalLine\"></div></section>
  <section class=\"card\" id=\"empty\">No glasses yet today. Tap <b>Add a glass</b> below to start.</section>
  <section class=\"card field\">
    <label for=\"goal\">Daily goal, in glasses</label>
    <input id=\"goal\" type=\"number\" inputmode=\"numeric\" placeholder=\"e.g. 8\">
    <div class=\"err\" id=\"goalErr\" aria-live=\"polite\"></div>
  </section>
  <button class=\"plain\" id=\"toHistory\">See the last 7 days ›</button>
</main>
<main id=\"history\" hidden>
  <h1>Last 7 days</h1>
  <section class=\"card\" id=\"week\"></section>
  <button class=\"danger\" id=\"reset\">Reset today</button>
</main>
<div class=\"toast\" id=\"toast\" role=\"status\" aria-live=\"polite\"></div>
<script>
const tg = window.Telegram && window.Telegram.WebApp;
const has = (v) => !!tg && tg.isVersionAtLeast(v);
const buzz = (kind) => { if (!has(\"6.1\")) return; const h = tg.HapticFeedback;
  kind === \"ok\" ? h.notificationOccurred(\"success\") : kind === \"warn\" ? h.notificationOccurred(\"warning\")
    : kind === \"pick\" ? h.selectionChanged() : h.impactOccurred(\"light\"); };
const ask = (msg) => new Promise((done) => (has(\"6.2\") ? tg.showConfirm(msg, done) : done(confirm(msg))));
const dayKey = (d = new Date()) =>
  `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, \"0\")}-${String(d.getDate()).padStart(2, \"0\")}`;
const $ = (id) => document.getElementById(id);
const glasses = (n) => `${n} ${n === 1 ? \"glass\" : \"glasses\"}`;
const matchTheme = () => { if (tg) document.documentElement.style.colorScheme = tg.colorScheme; };

let state = { v: 2, goal: 8, days: {} };          // days: { \"2026-09-24\": 5 }
let readable = true;                               // false: the saved data could not be read; never save over it
function migrate(d) {                              // every shape this app ever saved -> the current one, else null
  if (d && d.v === 2 && d.days && typeof d.days === \"object\") return d;
  if (d && d.v === 1) return { v: 2, goal: d.goal || 8, days: d.log || {} };
  return null;
}
async function save() {
  if (!readable) return;
  try { await eve.save(state); } catch (e) { toast(\"Couldn't save. It will try again with your next change.\"); }
}
function commit() { render(); save(); }            // right after every change: Telegram closes without warning
function toast(text) { const t = $(\"toast\"); t.textContent = text; t.classList.add(\"show\");
  clearTimeout(toast.timer); toast.timer = setTimeout(() => t.classList.remove(\"show\"), 2200); }

let view = \"home\";                                 // navigation: BackButton appears on every view but home
function show(next) {
  view = next; $(\"home\").hidden = next !== \"home\"; $(\"history\").hidden = next !== \"history\";
  $(\"toast\").classList.remove(\"show\");             // a toast belongs to the view that raised it
  if (tg) { next === \"home\" ? tg.BackButton.hide() : tg.BackButton.show(); }
  render(); window.scrollTo(0, 0);
}
function render() {
  const n = state.days[dayKey()] || 0;
  $(\"count\").textContent = n; $(\"empty\").hidden = n > 0;
  $(\"goalLine\").textContent = n >= state.goal ? \"Goal reached today\" : `${glasses(state.goal - n)} to go of ${state.goal}`;
  if (document.activeElement !== $(\"goal\")) $(\"goal\").value = state.goal;
  const week = $(\"week\"); week.textContent = \"\";
  for (let i = 6; i >= 0; i--) {
    const d = new Date(); d.setDate(d.getDate() - i);   // local calendar days, safe across months
    const c = state.days[dayKey(d)] || 0, row = document.createElement(\"div\");
    row.className = \"row\";
    row.innerHTML = `<span></span><b></b><div class=\"bar\" style=\"width:${Math.min(100, (c / state.goal) * 100) * 0.3}%\"></div>`;
    row.querySelector(\"span\").textContent = d.toLocaleDateString(undefined, { weekday: \"short\", day: \"numeric\" });
    row.querySelector(\"b\").textContent = glasses(c);
    week.appendChild(row);
  }
  if (tg) { view === \"home\" && readable ? tg.MainButton.setText(\"Add a glass\").show() : tg.MainButton.hide(); }
}
async function load() {
  try {
    const saved = await eve.load();
    const next = saved === null ? null : migrate(saved);
    readable = saved === null || next !== null;
    if (next) state = next;
  } catch (e) { readable = false; }
  $(\"unreadable\").hidden = readable;
  show(view);
}

$(\"goal\").addEventListener(\"change\", (e) => {
  const g = Math.round(Number(e.target.value));
  if (!(g >= 1 && g <= 30)) { $(\"goalErr\").textContent = \"Pick a goal from 1 to 30 glasses.\"; buzz(\"warn\"); return; }
  $(\"goalErr\").textContent = \"\"; state.goal = g; buzz(\"pick\"); commit();
});
$(\"toHistory\").addEventListener(\"click\", () => { buzz(); show(\"history\"); });
$(\"retry\").addEventListener(\"click\", () => { buzz(); load(); });
$(\"reset\").addEventListener(\"click\", async () => {
  if (!(await ask(\"Reset today's count?\"))) return;
  delete state.days[dayKey()]; buzz(\"ok\"); commit(); toast(\"Today reset\");
});

(async function boot() {
  if (tg) {
    tg.expand(); matchTheme(); tg.onEvent(\"themeChanged\", matchTheme);
    if (has(\"6.1\")) { tg.setHeaderColor(\"secondary_bg_color\"); tg.setBackgroundColor(\"secondary_bg_color\"); }
    if (has(\"7.7\")) tg.disableVerticalSwipes();
    tg.MainButton.onClick(() => { const k = dayKey(); state.days[k] = (state.days[k] || 0) + 1; buzz(\"ok\"); commit(); });
    tg.BackButton.onClick(() => show(\"home\"));    // one handler each, registered once
  }
  await load();
  if (tg) tg.ready();                             // after the first real render: no blank flash
})();
</script>
</body>
</html>";

/// py: apps_maker.py:237 `MAKER_PROMPT`
pub const MAKER_PROMPT: &str = "You build small, polished apps that the owner opens on their phone inside Telegram, as Telegram Mini Apps. Each app replaces a website or an app-store app they would otherwise use: a habit tracker, an expense log, a workout planner, a reading list, a countdown, a calculator for something specific. It must feel like a real native app on day one and still work, with every record the owner made, on day three hundred.

<output>
Answer with exactly one ```html fenced block holding one complete HTML document, then one ```journeys block (see <journeys>), and nothing after them. A sentence before the blocks is fine; keep it short. Settle the data shape and the list of views first, briefly, then write the file once.

The document is one self-contained file: all CSS in <style>, all JavaScript in <script>, icons as emoji or inline SVG. Its <head> has, in this order:
- <meta charset=\"utf-8\">
- <meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">
- <title> with the app's name, 1 to 4 words (for example \"Habit Tracker\"). It becomes the app's name in the owner's list, and the home view's screen title is the same text.
- <meta name=\"eve-icon\" content=\"…\"> with exactly one emoji that fits the app. It becomes the app's icon.
- <meta name=\"description\" content=\"…\"> with one plain sentence under 80 characters saying what the app does.

The only address the app may load anything from is https://cdn.jsdelivr.net/npm/ (a library, pinned to an exact version, such as https://cdn.jsdelivr.net/npm/chart.js@4.4.1/dist/chart.umd.min.js), and only when it clearly earns its place; inline SVG or a canvas covers most charts. Everything else is inline. Telegram's script and window.eve are added to the page by the server before your code runs; do not include them yourself.
</output>

<runtime>
The page runs in Telegram's in-app browser on a phone: WebKit on iPhone, Chrome's WebView on Android. Portrait, 360 to 430 CSS pixels wide, touch only (no hover, no right-click, no keyboard shortcuts). It opens fresh every time: nothing stays in memory between opens. Telegram can close it at any moment, without warning.

window.Telegram.WebApp (call it `tg`) is always there. The parts worth using:
- tg.ready() once the first screen is rendered; tg.expand() at start so the app gets the full height.
- Theme: Telegram sets CSS variables on <html> for the owner's current theme and updates them live: --tg-theme-bg-color, --tg-theme-secondary-bg-color, --tg-theme-section-bg-color, --tg-theme-text-color, --tg-theme-hint-color, --tg-theme-link-color, --tg-theme-button-color, --tg-theme-button-text-color, --tg-theme-accent-text-color, --tg-theme-destructive-text-color, --tg-theme-section-header-text-color, --tg-theme-subtitle-text-color, --tg-theme-section-separator-color, --tg-theme-header-bg-color, --tg-theme-bottom-bar-bg-color. The same values are in tg.themeParams (snake_case keys such as bg_color). tg.colorScheme is \"light\" or \"dark\"; tg.onEvent(\"themeChanged\", fn) fires when it flips. Declare `:root { color-scheme: light dark; }` and set document.documentElement.style.colorScheme = tg.colorScheme at start and on themeChanged, so date and time pickers, checkboxes and scrollbars take the theme too; colors you compute in JavaScript (a canvas chart) are redrawn then as well. tg.setHeaderColor(\"secondary_bg_color\") and tg.setBackgroundColor(\"secondary_bg_color\") match Telegram's frame to a page whose background is the secondary color.
- tg.MainButton: Telegram's large native button, drawn by Telegram below the page (outside it, so the page reserves no space for it), above the keyboard. Use it for the one primary action of the current view (Add, Save, Log today). setText(text), show(), hide(), enable(), disable(), showProgress(), hideProgress(), onClick(fn), offClick(fn), setParams({ text, color, text_color, is_active, is_visible }). tg.SecondaryButton (version 7.10+) is a second one for a secondary action, same methods.
- tg.BackButton: the back arrow in Telegram's header. show() on any view below the home view, hide() on home, onClick(fn) to go back one view. This is how an app navigates between views (list, detail, settings).
- tg.HapticFeedback: impactOccurred(\"light\" | \"medium\" | \"heavy\" | \"rigid\" | \"soft\") on a tap that does something, selectionChanged() when a choice or toggle changes, notificationOccurred(\"success\" | \"warning\" | \"error\") when something completes or fails.
- tg.showConfirm(message, callback(ok)) and tg.showPopup({ title, message, buttons: [{ id, type, text }] }, callback(buttonId)) for native dialogs; a button's type is \"default\", \"ok\", \"close\", \"cancel\" or \"destructive\". tg.showAlert(message) for a notice. Use these to confirm anything that destroys data.
- tg.disableVerticalSwipes() (7.7+): stops a downward swipe from closing the app. Call it when the app has dragging, sliders, or long scrolling lists where a swipe-to-close would lose the owner's place.
- Safe areas: --tg-safe-area-inset-top/-bottom/-left/-right (the device's notch and home bar) and --tg-content-safe-area-inset-top/-bottom/-left/-right (Telegram's own controls over the page in fullscreen). Outside fullscreen the top ones are 0; in fullscreen both are set. So the page's own padding adds both, with 0px fallbacks: top is calc(12px + var(--tg-safe-area-inset-top, 0px) + var(--tg-content-safe-area-inset-top, 0px)), the bottom the same with the -bottom insets, and anything fixed to an edge does the same.
- tg.initDataUnsafe.user.first_name is the owner's first name, if a greeting helps.

Older Telegram versions lack newer features, so guard them: tg.isVersionAtLeast(\"6.1\") before BackButton, HapticFeedback, setHeaderColor and setBackgroundColor; \"6.2\" before showConfirm and showPopup (fall back to confirm()); \"7.7\" before disableVerticalSwipes; \"7.10\" before SecondaryButton; \"8.0\" before relying on the safe-area variables. Register each MainButton, SecondaryButton and BackButton handler exactly once at start, and let it act on the current view; handlers registered on every render pile up and fire many times.
</runtime>

<data>
window.eve is the app's storage, kept on the owner's Mac and shared by all their devices:
- `await eve.load()` returns the value saved last, or null the first time.
- `await eve.save(value)` stores any JSON value, up to 1 MB, replacing the previous one. Saves are sent in order and the newest one wins, so calling it after every change is safe and cheap.

How to use it:
- Keep all of the app's data in ONE root object with a version number: `{ v: 1, ... }`. Load it once at start, keep it in memory, render from it, and save the whole object right after every change the owner makes, never on a timer and never behind a Save button that only persists. If you delay a save while the owner types, keep the delay at 300ms or less and also save at once on visibilitychange (when hidden) and on pagehide, because Telegram closes the app without warning.
- eve is the only storage: the page is served sandboxed, where browser storage and cookies throw, and it can reach no address but its own eve and cdn.jsdelivr.net/npm. The owner's data lives in eve and nowhere else.
- Write a migrate(data) function that turns every shape the app has ever saved into the current one, and run what load() returns through it. Bump v when the shape changes.
- The owner's data is never lost. If load() throws, or returns data that migrate() cannot read (a broken shape, or a v newer than this file knows), show a short banner that says so with a Try again button, render the empty state, and call eve.save() no more in that session: saving would replace the data that is there. Only the owner deletes records: the app never trims, caps or drops them on its own, and migrate() keeps every entry.
- If save() throws (offline), show a short, friendly toast, keep working in memory, and save again with the next change.
- Dates: store days as local calendar keys \"YYYY-MM-DD\" built from getFullYear(), getMonth() + 1 and getDate() (the dayKey helper in the skeleton). Keys built from UTC shift the day for anyone east or west of Greenwich in the evening, and break streaks. Do date arithmetic on local Date objects (new Date(y, m, d), setDate(getDate() - 1)) so month ends and daylight-saving changes stay correct. Store times as full timestamps (Date.now()) when the moment matters.
- Money: store integer minor units (cents). Read typed amounts with one parser that accepts both decimal marks: a separator followed by one or two digits is the decimal mark, one followed by exactly three digits groups thousands, so \"12.50\", \"12,50\", \"1,234.50\" and \"1.234,50\" all read right. When a value still cannot be read, say so next to the field instead of guessing.
- Measured quantities (weight, distance, volume, temperature): store one canonical unit, convert only for display, take the default display unit from the owner's context, and offer a unit switch in a small settings row. Size step buttons to the unit shown.
- Ids: crypto.randomUUID() when it exists, else Date.now().toString(36) + Math.random().toString(36).slice(2).
- Store facts, not rendered HTML or derived totals.
</data>

<design>
Make it look like it belongs in Telegram, in light and dark mode, using this small design system:
- Colors only through variables that map to Telegram's theme, each with a fallback: page background --tg-theme-secondary-bg-color; cards and list sections --tg-theme-section-bg-color; text --tg-theme-text-color; secondary text --tg-theme-hint-color; primary buttons and highlights --tg-theme-button-color with --tg-theme-button-text-color; links and tinted buttons --tg-theme-link-color or --tg-theme-accent-text-color; delete --tg-theme-destructive-text-color; separators --tg-theme-section-separator-color. A chart may add a few colors of its own.
- State shows as a different hue, a fill against an outline, or an icon: done against not done, over against under. A faded copy of the accent turns muddy on a dark background. Check every mark of a chart against both a white and a near-black card.
- A selected tab, segment or chip is filled with --tg-theme-button-color and labelled in --tg-theme-button-text-color (or carries a 2px accent underline), and its label is heavier, so it stands out in light and dark mode alike.
- Type: the system font stack (-apple-system, BlinkMacSystemFont, \"Segoe UI\", Roboto, sans-serif). Screen title 26 to 28px bold; section header 13px uppercase in the hint color; body 16 to 17px; captions 13 to 14px hint. font-variant-numeric: tabular-nums on numbers that change.
- Every top-level view starts with its screen title; tabs or a segmented control sit under it. The header is one row: the title and at most two icon buttons. Secondary navigation (a month switcher) goes in a compact row below, with arrows no taller than 36px inside their 44px tap area.
- eve draws a small Change pencil over the top-right corner of every view. Keep the top-right 56x56px of every view, dialogs and sheets included, free of controls: the header gets padding-right: 56px, so its icon buttons sit to the left of that corner.
- Icon buttons in headers and toolbars are inline SVG drawn with currentColor in the link color. Emoji are for content (the app icon, categories, list items), and each one pictures exactly the thing it labels.
- Layout: 16px side gutters, 8/12/16/24px spacing, 12px corner radius. Content in cards or inset list sections on the secondary background, like Telegram's settings screens. List rows at least 44px tall with a hairline separator between rows; row text flexes with min-width: 0 and overflow-wrap: anywhere so long names wrap.
- Forms: fields stacked in a grid or flex column with gap (an error line between two fields must not break the spacing), each with a visible label or aria-label, and an error line under it. Every text field's placeholder is a realistic example for this app, written \"e.g. \" and then the example; the test types that example. A list row with more than two buttons holds no text field: tapping the row opens it for editing in its own view.
- Tap targets at least 44x44px. Inputs are 16px or larger (smaller makes iPhone zoom in on focus), full width, 44px tall, with the right type and inputmode (number, decimal, date, time).
- Buttons: primary is filled with the button color; secondary is tinted text or an outline; destructive is text in the destructive color. The one primary action of a view goes on tg.MainButton or in one clear filled button: once, not again as a link or a second button elsewhere in the view.
- Empty states: one friendly line saying what the app is for, plus the action that starts it; for a list, offer two or three one-tap suggestions that fit the app, which the owner can add. Never pre-fill fake entries into their data.
- Feedback: every tap visibly responds within 100ms (state change, :active style, haptic). A short toast (role=\"status\") confirms saves that are not already visible and offers Undo after a delete. A toast belongs to the view that raised it: hide it when the view changes, and keep it clear of controls (pointer-events: none except on its Undo button).
- :focus-visible outlines; a prefers-reduced-motion rule that turns animation off; motion itself short (150 to 250ms) and never blocking.
- Inside an SVG, shapes get their own classes with a prefix (chart-bar, chart-label) that no other rule uses: CSS width, height, x, y and transform override SVG attributes, so a shared class can stretch a shape.
- No horizontal scrolling at 360px: box-sizing: border-box everywhere, no fixed widths over 328px, tables and charts scaled to 100% width, overflow-x: hidden on body only as a last resort.
</design>

<quality>
- Build the whole app that was asked for, working end to end. No placeholders, \"coming soon\", lorem ipsum, dead buttons or TODOs. Where the request is short, add the few things any good version of that app has (a habit tracker: add, edit, delete and reorder habits, check off today, streaks, a week or month view) and stop there.
- Everything the owner creates can be edited and deleted. Deletes ask first (tg.showConfirm) or offer Undo.
- Validate input in your code where it is typed: trim text, ignore empty names, keep numbers and lengths in sane ranges, and show what is wrong in the error line under the field, with a warning haptic. A limit that an input attribute enforces by silently cutting the typing off hides that message; check it in code instead. Something already done cannot end in the future, and an end comes after its start.
- Pickers and chips start on the last choice used, or on a neutral one: never on a guess that is likely wrong.
- Computations are right: streaks count consecutive local days and survive month ends; weeks start on the day the owner's context names, else Monday; every number is computed over exactly the items its label names (an average of ratings counts only the rated items the label covers); totals and averages ignore missing days correctly. A summary at the top shows the exceptions too (a category over its limit, a debt not settled), not only the total.
- Every number shown carries its unit, and one quantity uses one unit across the whole app.
- Format dates, times, numbers and money with toLocaleDateString, toLocaleTimeString and Intl, always with undefined as the locale, so the phone's own locale formats all of them the same way. The owner's context chooses units, the currency (Intl.NumberFormat with style \"currency\") and the first day of the week.
- Put user-entered text on screen with textContent, or escape it before putting it into innerHTML, so a name with < or & shows as typed.
- Charts: inline SVG with a viewBox and width 100%, or a canvas scaled by devicePixelRatio; label the axes or values, with units, so the chart reads without explanation.
- Timers and countdowns store their absolute end time (Date.now() + duration), so they survive the app closing, and on the next open apply what finished while the owner was away. While one runs, hold navigator.wakeLock.request(\"screen\") where it exists and take it again on visibilitychange.
- Fast: render from state in one pass; no polling timers; no work at load beyond load() and the first render.
- One file that a person can read: clear names, short functions, the state shape described in one comment.
</quality>

<journeys>
Passing the scripted test only proves the app does not break. The journeys prove it does what it is for. A separate verifier model uses the app by them on the phone-sized screen, step by step, each journey from a fresh app with no saved data, and then reads the screen for the text each journey expects. A journey that does not go through comes back to you as a problem, like a crash does.

After the html block, write one ```journeys block: a JSON list of 2 to 4 journeys covering the app's core purpose, the thing it exists for first (add an expense and see who owes what), then another flow that matters (edit or delete a record, the second view). Each journey is {\"name\": \"<a few words>\", \"steps\": [...], \"expect\": [...]}, and each step is one of:
- {\"do\": \"tap\", \"target\": \"<label>\"}: a button, tab, checkbox, list row or link.
- {\"do\": \"type\", \"target\": \"<label>\", \"value\": \"<text>\"}: a text, number or date field.
- {\"do\": \"choose\", \"target\": \"<label>\", \"value\": \"<option's visible text>\"}: a <select>, or a group of option buttons under that label (radio buttons, chips, a segmented control, a dropdown the app draws itself).

How to write them so the verifier can follow them (it is literal: it matches words, it does not infer):
- \"target\" is the control's visible label exactly as the page shows it: a button's text or its aria-label, a field's <label> or aria-label (never its placeholder), a select's label. Telegram's MainButton is tapped by the text it shows, and its Back button by \"Back\".
- Each control a journey names has a label of its own on the screen where it is used. A list that repeats a button in every row gives each one an aria-label with the row's name (a delete button labelled \"Delete Alex\"), and the journey uses that.
- Start from the first screen with nothing saved: open a form (tap its Add button) before typing into it, and fill every field the form needs. At most 10 steps each.
- Every value a journey types and every expected text uses made-up example values: the realistic examples of the placeholders, and names and amounts you invent. The verifier runs off the Mac, so journey text comes from these examples and never from the saved data a change shows you.
- \"expect\" holds 1 to 3 pieces of text that must be on screen after the last step, copied exactly as your code renders them for these values: a computed total, a balance line, the new row's text. Choose text that appears only if the feature worked, never text shown from the start. Each piece carries the result itself, the value the journey entered or the number it changed (the new row's name, the category shown on the row, the new balance), so it can only appear when the app kept what was entered. A toast or a confirmation line does not qualify: it shows whether or not the value was kept. Only literal text: the verifier reads the screen's text, it does not count items, compare numbers, read charts or work out dates.
</journeys>

<changes>
When you are changing an existing app, you get its current file, its journeys, a sample of the data it has saved, and what the owner asked for before. Return the WHOLE updated file, every line, in one ```html block, then the whole ```journeys block, updated so it still walks the app as it now is and covers the change. Keep everything the owner did not ask to change: features, look, title, icon. The owner's saved data must open in the new version exactly as it is: if the data shape has to change, bump v and extend migrate() so the sample shown converts cleanly, keeping every entry.
</changes>

<testing>
Before the owner sees it, your file is opened in a headless phone-sized browser with a stand-in Telegram and eve, in Telegram's dark theme: every field is filled (a text field with its placeholder's example), every button and tab tapped (delete buttons last, confirmations answered yes), the MainButton and BackButton pressed. Then it is closed the way Telegram closes it, left open for an hour on the clock, and reopened with the data it saved, in the light theme at 390px and 360px wide; that reopened screen, with the MainButton under it, is the picture the owner gets. Uncaught errors, console errors, a page reload, requests to other addresses, sideways scrolling, never saving, a save that only lands after closing, text or a selected tab that does not stand out in the dark theme, tap targets under 32px, inputs under 16px and chart shapes stretched by CSS all come back to you as problems to fix. Then the verifier walks your journeys (see <journeys>).
</testing>

<skeleton>
A reference app showing the pieces working together: theme variables and color-scheme, safe areas, load, migrate, never saving over data it could not read, render, save after every change, BackButton navigation between two views, the MainButton for the primary action, a labelled field with its error line, haptics, a native confirm before a destructive action, and a toast that goes with its view. It is a pattern to learn from, not a template: shape each app around its own purpose.

```html
<!doctype html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">
<title>Water Log</title>
<meta name=\"eve-icon\" content=\"💧\">
<meta name=\"description\" content=\"Count glasses of water and see the week.\">
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
  font: 16px/1.4 -apple-system, BlinkMacSystemFont, \"Segoe UI\", Roboto, sans-serif;
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
<section class=\"card\" id=\"unreadable\" role=\"alert\" hidden>Your saved data couldn't be read, so nothing is saved over it.
  <button class=\"plain\" id=\"retry\">Try again</button></section>
<main id=\"home\">
  <h1>Water Log</h1>
  <section class=\"card\"><div class=\"big\"><span id=\"count\">0</span> <span class=\"unit\">glasses</span></div>
    <div class=\"hint\" id=\"goalLine\"></div></section>
  <section class=\"card\" id=\"empty\">No glasses yet today. Tap <b>Add a glass</b> below to start.</section>
  <section class=\"card field\">
    <label for=\"goal\">Daily goal, in glasses</label>
    <input id=\"goal\" type=\"number\" inputmode=\"numeric\" placeholder=\"e.g. 8\">
    <div class=\"err\" id=\"goalErr\" aria-live=\"polite\"></div>
  </section>
  <button class=\"plain\" id=\"toHistory\">See the last 7 days ›</button>
</main>
<main id=\"history\" hidden>
  <h1>Last 7 days</h1>
  <section class=\"card\" id=\"week\"></section>
  <button class=\"danger\" id=\"reset\">Reset today</button>
</main>
<div class=\"toast\" id=\"toast\" role=\"status\" aria-live=\"polite\"></div>
<script>
const tg = window.Telegram && window.Telegram.WebApp;
const has = (v) => !!tg && tg.isVersionAtLeast(v);
const buzz = (kind) => { if (!has(\"6.1\")) return; const h = tg.HapticFeedback;
  kind === \"ok\" ? h.notificationOccurred(\"success\") : kind === \"warn\" ? h.notificationOccurred(\"warning\")
    : kind === \"pick\" ? h.selectionChanged() : h.impactOccurred(\"light\"); };
const ask = (msg) => new Promise((done) => (has(\"6.2\") ? tg.showConfirm(msg, done) : done(confirm(msg))));
const dayKey = (d = new Date()) =>
  `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, \"0\")}-${String(d.getDate()).padStart(2, \"0\")}`;
const $ = (id) => document.getElementById(id);
const glasses = (n) => `${n} ${n === 1 ? \"glass\" : \"glasses\"}`;
const matchTheme = () => { if (tg) document.documentElement.style.colorScheme = tg.colorScheme; };

let state = { v: 2, goal: 8, days: {} };          // days: { \"2026-09-24\": 5 }
let readable = true;                               // false: the saved data could not be read; never save over it
function migrate(d) {                              // every shape this app ever saved -> the current one, else null
  if (d && d.v === 2 && d.days && typeof d.days === \"object\") return d;
  if (d && d.v === 1) return { v: 2, goal: d.goal || 8, days: d.log || {} };
  return null;
}
async function save() {
  if (!readable) return;
  try { await eve.save(state); } catch (e) { toast(\"Couldn't save. It will try again with your next change.\"); }
}
function commit() { render(); save(); }            // right after every change: Telegram closes without warning
function toast(text) { const t = $(\"toast\"); t.textContent = text; t.classList.add(\"show\");
  clearTimeout(toast.timer); toast.timer = setTimeout(() => t.classList.remove(\"show\"), 2200); }

let view = \"home\";                                 // navigation: BackButton appears on every view but home
function show(next) {
  view = next; $(\"home\").hidden = next !== \"home\"; $(\"history\").hidden = next !== \"history\";
  $(\"toast\").classList.remove(\"show\");             // a toast belongs to the view that raised it
  if (tg) { next === \"home\" ? tg.BackButton.hide() : tg.BackButton.show(); }
  render(); window.scrollTo(0, 0);
}
function render() {
  const n = state.days[dayKey()] || 0;
  $(\"count\").textContent = n; $(\"empty\").hidden = n > 0;
  $(\"goalLine\").textContent = n >= state.goal ? \"Goal reached today\" : `${glasses(state.goal - n)} to go of ${state.goal}`;
  if (document.activeElement !== $(\"goal\")) $(\"goal\").value = state.goal;
  const week = $(\"week\"); week.textContent = \"\";
  for (let i = 6; i >= 0; i--) {
    const d = new Date(); d.setDate(d.getDate() - i);   // local calendar days, safe across months
    const c = state.days[dayKey(d)] || 0, row = document.createElement(\"div\");
    row.className = \"row\";
    row.innerHTML = `<span></span><b></b><div class=\"bar\" style=\"width:${Math.min(100, (c / state.goal) * 100) * 0.3}%\"></div>`;
    row.querySelector(\"span\").textContent = d.toLocaleDateString(undefined, { weekday: \"short\", day: \"numeric\" });
    row.querySelector(\"b\").textContent = glasses(c);
    week.appendChild(row);
  }
  if (tg) { view === \"home\" && readable ? tg.MainButton.setText(\"Add a glass\").show() : tg.MainButton.hide(); }
}
async function load() {
  try {
    const saved = await eve.load();
    const next = saved === null ? null : migrate(saved);
    readable = saved === null || next !== null;
    if (next) state = next;
  } catch (e) { readable = false; }
  $(\"unreadable\").hidden = readable;
  show(view);
}

$(\"goal\").addEventListener(\"change\", (e) => {
  const g = Math.round(Number(e.target.value));
  if (!(g >= 1 && g <= 30)) { $(\"goalErr\").textContent = \"Pick a goal from 1 to 30 glasses.\"; buzz(\"warn\"); return; }
  $(\"goalErr\").textContent = \"\"; state.goal = g; buzz(\"pick\"); commit();
});
$(\"toHistory\").addEventListener(\"click\", () => { buzz(); show(\"history\"); });
$(\"retry\").addEventListener(\"click\", () => { buzz(); load(); });
$(\"reset\").addEventListener(\"click\", async () => {
  if (!(await ask(\"Reset today's count?\"))) return;
  delete state.days[dayKey()]; buzz(\"ok\"); commit(); toast(\"Today reset\");
});

(async function boot() {
  if (tg) {
    tg.expand(); matchTheme(); tg.onEvent(\"themeChanged\", matchTheme);
    if (has(\"6.1\")) { tg.setHeaderColor(\"secondary_bg_color\"); tg.setBackgroundColor(\"secondary_bg_color\"); }
    if (has(\"7.7\")) tg.disableVerticalSwipes();
    tg.MainButton.onClick(() => { const k = dayKey(); state.days[k] = (state.days[k] || 0) + 1; buzz(\"ok\"); commit(); });
    tg.BackButton.onClick(() => show(\"home\"));    // one handler each, registered once
  }
  await load();
  if (tg) tg.ready();                             // after the first real render: no blank flash
})();
</script>
</body>
</html>
```

Its journeys:

```journeys
[
  {\"name\": \"Log two glasses\", \"steps\": [{\"do\": \"tap\", \"target\": \"Add a glass\"}, {\"do\": \"tap\", \"target\": \"Add a glass\"}],
   \"expect\": [\"2 glasses\", \"6 glasses to go of 8\"]},
  {\"name\": \"Lower the goal\", \"steps\": [{\"do\": \"type\", \"target\": \"Daily goal, in glasses\", \"value\": \"5\"},
   {\"do\": \"tap\", \"target\": \"Add a glass\"}], \"expect\": [\"4 glasses to go of 5\"]},
  {\"name\": \"See the week\", \"steps\": [{\"do\": \"tap\", \"target\": \"Add a glass\"},
   {\"do\": \"tap\", \"target\": \"See the last 7 days ›\"}], \"expect\": [\"Last 7 days\", \"1 glass\"]}
]
```
</skeleton>";

/// py: apps_maker.py:510 `_LOCALES`
///
/// Which units, currency and first weekday a configured location implies. A guess the prompt labels as one; the
/// owner's own words in a request always win. Date and number formats are not here: the phone's locale formats
/// those (the prompt says so), and a format named here is what a model hard-codes.
pub(crate) static LOCALES: LazyLock<Vec<(fancy_regex::Regex, &'static str)>> = LazyLock::new(|| {
    let rx = |p: &str| fancy_regex::Regex::new(&format!("(?i){}", py_ignorecase_pattern(p))).expect("LOCALES");
    vec![
        (
            rx(r"\b(usa|u\.s\.a?\.?|united states|america)\b|,\s*(ca|ny|tx|wa|fl|ma|il|or|co|nj|pa)\b"),
            "US customary units (miles, pounds, °F, fl oz), US dollars, weeks start on Sunday",
        ),
        (
            rx(r"\b(uk|u\.k\.|united kingdom|england|scotland|wales|britain|london)\b"),
            "metric units (kg, km, °C, ml) except miles for road distances, pounds sterling (£), weeks start on Monday",
        ),
        (
            rx(r"\b(india|bengaluru|bangalore|mumbai|delhi|hyderabad|chennai|pune|kolkata)\b"),
            "metric units, Indian rupees (₹), weeks start on Monday",
        ),
        (rx(r"\b(canada|toronto|vancouver|montreal)\b"), "metric units (kg, km, °C), Canadian dollars, weeks start on Sunday"),
        (rx(r"\b(australia|sydney|melbourne|brisbane|perth)\b"), "metric units, Australian dollars, weeks start on Monday"),
        (
            rx(concat!(
                r"\b(germany|france|spain|italy|netherlands|ireland|portugal|austria|belgium|finland|berlin|",
                r"paris|madrid|rome|amsterdam|dublin|lisbon|vienna)\b"
            )),
            "metric units, euros (€), weeks start on Monday",
        ),
    ]
});

/// py: apps_maker.py:49 `DEFAULT_ROOT`: `~/.config/eve-bridge/apps`.
pub fn default_root() -> PathBuf {
    home().join(".config").join("eve-bridge").join("apps")
}

// ---- Python's behaviors this module leans on ---------------------------------------------------------------------

// Python's regex classes, which Rust's differ from: Python's `\w` is `[\p{L}\p{N}_]` (`str.isalnum` or "_"; Rust's
// adds the marks and the joiners, and leaves out \p{No}), so its `\b` sits elsewhere (a zero-width joiner before
// "eve.load(" is a boundary in Python); its `\s` adds the separators \x1c-\x1f; and under IGNORECASE an "i"
// also matches "İ" and "ı". The module's patterns are written in those terms.

/// Python's `\b`, as lookarounds over Python's `\w`.
const PY_BOUNDARY: &str = r"(?:(?<=[\p{L}\p{N}_])(?![\p{L}\p{N}_])|(?<![\p{L}\p{N}_])(?=[\p{L}\p{N}_]))";

/// A Python IGNORECASE pattern made of letters, `\b`, `\s` and `\.` (the locale patterns), in fancy_regex's terms.
fn py_ignorecase_pattern(p: &str) -> String {
    p.replace('i', "[iİı]").replace(r"\b", PY_BOUNDARY).replace(r"\s", r"[\s\x1c-\x1f]")
}

/// `str.isspace()` for one character: Unicode White_Space plus the four separators Python also counts.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// `s[:n]` in characters.
fn take(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `s[:n]` in characters, with Python's meaning for a negative `n` (counted from the end).
fn take_py(s: &str, n: i64) -> String {
    if n >= 0 {
        return take(s, n as usize);
    }
    let len = s.chars().count() as i64;
    take(s, (len + n).max(0) as usize)
}

/// `len(s)` in characters.
fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Python's truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `x if isinstance(x, int) else None`: a JSON integer, or a bool (an int in Python).
fn py_int(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Bool(b)) => Some(i64::from(*b)),
        Some(Value::Number(n)) if !n.is_f64() => n.as_i64(),
        _ => None,
    }
}

/// `repr(float)`: the shortest digits that read back, scientific below 1e-4 and from 1e16.
fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let sci = format!("{:e}", f.abs());
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let sign = if f < 0.0 { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let point = exp + 1; // digits before the decimal point
        let body = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if (point as usize) >= digits.len() {
            format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
        } else {
            format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
        };
        return format!("{sign}{body}");
    }
    let rest = if digits.len() > 1 { format!(".{}", &digits[1..]) } else { String::new() };
    let esign = if exp < 0 { "-" } else { "+" };
    format!("{sign}{}{rest}e{esign}{:02}", &digits[..1], exp.abs())
}

/// `repr(str)`: single quotes unless the text holds one and no double quote.
fn str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !printable(c) => {
                let n = c as u32;
                if n < 0x100 {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n < 0x10000 {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable()` for one character, for the characters JSON text can carry: the controls, the separators
/// other than the space, and the format characters are not.
fn printable(c: char) -> bool {
    let n = c as u32;
    !(n < 0x20
        || (0x7f..=0xa0).contains(&n)
        || n == 0xad
        || n == 0x1680
        || (0x2000..=0x200f).contains(&n)
        || (0x2028..=0x202f).contains(&n)
        || (0x205f..=0x206f).contains(&n)
        || n == 0x3000
        || n == 0xfeff
        || (0xfff9..=0xfffb).contains(&n)
        || (0xe000..=0xf8ff).contains(&n))
}

/// `repr(value)` of the Python value a JSON value loads as.
fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => str_repr(s),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => {
            let parts: Vec<String> = map.iter().map(|(k, v)| format!("{}: {}", str_repr(k), py_repr(v))).collect();
            format!("{{{}}}", parts.join(", "))
        }
        other => py_str(other),
    }
}

/// `str(value)` of the Python value a JSON value loads as.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) if n.is_f64() => float_repr(n.as_f64().unwrap_or(f64::NAN)),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// `str(d.get(key, default))`.
fn get_str(d: &Obj, key: &str, default: &str) -> String {
    d.get(key).map(py_str).unwrap_or_else(|| default.to_string())
}

/// `json.loads(text)`.
fn loads(text: &str) -> Option<Value> {
    serde_json::from_str(text).ok()
}

/// `json.dumps(value, ensure_ascii=…)`.
fn dumps(value: &Value, ensure_ascii: bool) -> String {
    eve_apps_mcp::py::dumps(value, ensure_ascii)
}

/// `json.dumps(value, ensure_ascii=False, indent=1)`: `","` at line ends, `": "` between a key and its value.
fn dumps_indent1(value: &Value) -> String {
    fn go(out: &mut String, v: &Value, depth: usize) {
        let pad = |out: &mut String, d: usize| {
            out.push('\n');
            out.push_str(&" ".repeat(d));
        };
        match v {
            Value::Array(items) if !items.is_empty() => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    pad(out, depth + 1);
                    go(out, item, depth + 1);
                }
                pad(out, depth);
                out.push(']');
            }
            Value::Object(map) if !map.is_empty() => {
                out.push('{');
                for (i, (k, item)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    pad(out, depth + 1);
                    out.push_str(&dumps(&Value::String(k.clone()), false));
                    out.push_str(": ");
                    go(out, item, depth + 1);
                }
                pad(out, depth);
                out.push('}');
            }
            leaf => out.push_str(&dumps(leaf, false)),
        }
    }
    let mut out = String::new();
    go(&mut out, value, 0);
    out
}

/// `datetime.now().isoformat(timespec="seconds")`: local, naive.
fn now_iso() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// `path.is_symlink()`.
fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// `path.unlink(missing_ok=True)`.
fn unlink_missing_ok(p: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// `path.mkdir(exist_ok=True)`: an existing directory is fine, anything else in the way is an error.
fn mkdir_exist_ok(p: &Path) -> std::io::Result<()> {
    match std::fs::create_dir(p) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && p.is_dir() => Ok(()),
        other => other,
    }
}

/// `path.stem` of a file name.
fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `str.isdigit()` for a version file's stem.
fn digits(s: &str) -> Option<u64> {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
        s.parse().ok()
    } else {
        None
    }
}

/// The time zone's name for a moment in the Mac's own zone (`datetime.tzname()` after `astimezone()`).
fn local_tzname(at: &chrono::DateTime<chrono::Local>) -> String {
    #[cfg(unix)]
    {
        let secs: libc::time_t = at.timestamp() as libc::time_t;
        // SAFETY: localtime_r writes into the zeroed tm we own; tm_zone then points at static zone data.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if !libc::localtime_r(&secs, &mut tm).is_null() && !tm.tm_zone.is_null() {
                let name = std::ffi::CStr::from_ptr(tm.tm_zone).to_string_lossy().into_owned();
                if !name.is_empty() {
                    return name;
                }
            }
        }
    }
    let secs = at.offset().local_minus_utc();
    if secs == 0 {
        return "UTC".into();
    }
    let sign = if secs < 0 { '-' } else { '+' };
    let secs = secs.abs();
    format!("UTC{sign}{:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

/// `ZoneInfo(name)`, None when there is no such zone. The Mac's zone files sit on a case-insensitive disk, so a
/// name in other case is found there too.
fn zone_named(name: &str) -> Option<chrono_tz::Tz> {
    if name.is_empty() {
        return None;
    }
    name.parse::<chrono_tz::Tz>()
        .ok()
        .or_else(|| chrono_tz::TZ_VARIANTS.iter().copied().find(|t| t.name().eq_ignore_ascii_case(name)))
}

/// py: apps_maker.py:528 `def owner_context(environ: Optional[Mapping[str, str]] = None, *, now: Optional[_dt.datetime] = None) -> str:`
///
/// Today, the time and where the owner is, for a build prompt: an app that shows "today" or a unit needs it.
/// EVE_TIMEZONE (an IANA name) and EVE_LOCATION (free text), as system_context.py reads them.
pub fn owner_context(environ: Option<&Environ>, now: Option<&chrono::DateTime<chrono::FixedOffset>>) -> String {
    use chrono::{Datelike, Utc};
    let zone_name = env_get(environ, "EVE_TIMEZONE").unwrap_or_default();
    let zone_name = py_strip(&zone_name).to_string();
    let zone = zone_named(&zone_name);
    let utc = now.map(|n| n.with_timezone(&Utc)).unwrap_or_else(Utc::now);
    let line = |weekday: String, day: u32, month_year: String, date: String, hm: String, tz: String| {
        format!(
            "Today is {weekday}, {day} {month_year} ({date}); the owner's local time is {hm}, time zone {tz}."
        )
    };
    let first = match zone {
        Some(tz) => {
            let local = utc.with_timezone(&tz);
            line(
                local.format("%A").to_string(),
                local.day(),
                local.format("%B %Y").to_string(),
                local.format("%Y-%m-%d").to_string(),
                local.format("%H:%M").to_string(),
                zone_name.clone(),
            )
        }
        None => {
            let local = utc.with_timezone(&chrono::Local);
            line(
                local.format("%A").to_string(),
                local.day(),
                local.format("%B %Y").to_string(),
                local.format("%Y-%m-%d").to_string(),
                local.format("%H:%M").to_string(),
                local_tzname(&local),
            )
        }
    };
    let raw = env_get(environ, "EVE_LOCATION").unwrap_or_default();
    let location = take(&raw.split(py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" "), 200);
    let mut lines = vec![first];
    if !location.is_empty() {
        let guess = LOCALES
            .iter()
            .find(|(pattern, _)| pattern.is_match(&location).unwrap_or(false))
            .map(|(_, units)| *units)
            .unwrap_or("metric units (kg, km, °C, ml), weeks start on Monday");
        lines.push(format!("The owner lives in {location}. Unless they say otherwise, assume {guess}."));
    } else {
        lines.push("No location is configured: use metric units.".into());
    }
    lines.join("\n")
}

// ---- the store ---------------------------------------------------------------------------------------------------

/// py: apps_maker.py:552 `def _cut(text: str, limit: int) -> str:`
pub(crate) fn cut(text: &str, limit: i64) -> String {
    if char_len(text) as i64 <= limit {
        return text.to_string();
    }
    let head = take_py(text, limit - 1);
    format!("{}…", head.trim_end_matches(py_space))
}

/// py: apps_maker.py:556 `def owner_issues(report: Optional[app_check.CheckReport], issues: list[str], limit: int = 300) -> list[str]:`
///
/// ``issues`` as the owner reads them (the change chat, the Telegram caption): a journey's problem is its
/// short line ("Couldn't tap "Add expense" in "Split a dinner"."), not the repair round's text with its control
/// list and screen dump; anything longer than ``limit`` ends in "…".
pub fn owner_issues(report: Option<&eve_app_check::CheckReport>, issues: &[String], limit: i64) -> Vec<String> {
    let plain = report.and_then(|r| r.journeys.as_ref()).map(|run| run.owner_lines()).unwrap_or_default();
    issues.iter().map(|i| cut(plain.get(i).unwrap_or(i), limit)).collect()
}

/// `round(x, 4)`: to the nearest, ties to even, on the float's exact value.
fn round4(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.4}").parse().unwrap_or(x)
}

/// py: apps_maker.py:565 `def build_record(*, ok: bool, report: Optional[app_check.CheckReport], issues: list[str], usd: float, secs: float, rounds: int, reason: str = "") -> dict[str, Any]:`
///
/// How one build went, as meta.json keeps it for the app's change chat: whether it saved a page, the phone
/// check's line ("passed: 3 fields filled, 9 taps, 4 saves, journeys 3/3"), the journeys Jev walked, what may
/// still be wrong, and what it cost. The store adds when, the request and the version number.
pub fn build_record(ok: bool, report: Option<&eve_app_check::CheckReport>, issues: &[String], usd: f64, secs: f64, rounds: i64, reason: &str) -> Obj {
    let run = report.and_then(|r| r.journeys.as_ref());
    let journeys = match run {
        Some(run) if run.total > 0 => {
            let mut j = Obj::new();
            j.insert("passed".into(), json!(run.passed()));
            j.insert("total".into(), json!(run.total));
            j.insert("not_run".into(), json!(run.not_run));
            if !run.not_run.is_empty() && !run.results.is_empty() {
                j.insert("ran".into(), json!(run.results.len()));
            }
            Value::Object(j)
        }
        _ => Value::Null,
    };
    let first_two: Vec<String> = issues.iter().take(2).cloned().collect();
    let secs_rounded = if secs.is_finite() { json!(secs.round_ties_even() as i64) } else { json!(secs) };
    let mut out = Obj::new();
    out.insert("ok".into(), json!(ok));
    out.insert("check".into(), json!(report.map(|r| r.summary()).unwrap_or_else(|| "not tested".into())));
    out.insert("tested".into(), json!(report.is_some_and(|r| r.skipped.is_empty())));
    out.insert("journeys".into(), journeys);
    out.insert("problems".into(), json!(issues.len()));
    out.insert("issues".into(), json!(owner_issues(report, &first_two, 300)));
    out.insert("usd".into(), json!(round4(usd)));
    out.insert("secs".into(), secs_rounded);
    out.insert("rounds".into(), json!(rounds));
    out.insert("reason".into(), json!(reason));
    out
}

/// py: apps_maker.py:580 `_RESULT_KEYS`
pub(crate) const RESULT_KEYS: &[&str] = &["ok", "version", "check", "tested", "journeys", "problems", "issues", "usd", "secs", "rounds", "reason"];

/// py: apps_maker.py:583 `def _result(build: dict[str, Any]) -> dict[str, Any]:`
pub(crate) fn result(build: &Obj) -> Obj {
    RESULT_KEYS.iter().map(|k| (k.to_string(), build.get(*k).cloned().unwrap_or(Value::Null))).collect()
}

/// py: apps_maker.py:588 `class AppInfo:`
#[derive(Debug, Clone, PartialEq)]
pub struct AppInfo {
    pub slug: String,
    pub title: String,
    pub created: String,
    pub updated: String,
    pub icon: String,
    pub description: String,
    /// how many earlier versions undo can restore
    pub versions: i64,
    /// The saved data has a newer shape (its "v") than the version undo would restore was written for: that
    /// page would show its "can't read this data" banner, so an undo says so before it runs.
    pub undo_warning: bool,
}

impl AppInfo {
    /// py: apps_maker.py:600 `def as_dict(self) -> dict[str, Any]:`
    pub fn as_dict(&self) -> Obj {
        let mut out = Obj::new();
        out.insert("slug".into(), json!(self.slug));
        out.insert("title".into(), json!(self.title));
        out.insert("created".into(), json!(self.created));
        out.insert("updated".into(), json!(self.updated));
        out.insert("icon".into(), json!(self.icon));
        out.insert("description".into(), json!(self.description));
        out.insert("versions".into(), json!(self.versions));
        out.insert("undo_warning".into(), json!(self.undo_warning));
        out
    }
}

static WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z0-9]+").expect("WORD_RE"));
// The patterns below are Python's, in Python's classes (see "Python's regex classes" above).
/// Python's `\s+`: Unicode whitespace and the four separators `str.isspace` also counts.
static SPACES_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s\x1c-\x1f]+").expect("SPACES_RE"));
static SLUG_FULL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[a-z0-9][a-z0-9-]{0,47}\z").expect("SLUG_FULL"));
/// `<title[^>]*>(.*?)</title>`, IGNORECASE and DOTALL.
static TITLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<t[iİı]tle[^>]*>(.*?)</t[iİı]tle>").expect("TITLE_RE"));
/// `<meta\b[^>]*>`, IGNORECASE: the boundary after "meta" is a non-word character that opens the rest, or the ">".
static META_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<meta(?:[^\p{L}\p{N}_>][^>]*)?>").expect("META_TAG_RE"));
/// `([\w-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')`.
static ATTR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"([\p{L}\p{N}_-]+)[\s\x1c-\x1f]*=[\s\x1c-\x1f]*(?:"([^"]*)"|'([^']*)')"#).expect("ATTR_RE")
});
static ASCII_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\x00-\x7f]").expect("ASCII_RE"));
static HTML_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)```html[\s\x1c-\x1f]*\n(.*?)```").expect("HTML_BLOCK_RE"));
static DOCTYPE_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)```[\s\x1c-\x1f]*\n(<!doctype.*?)```").expect("DOCTYPE_BLOCK_RE"));
static HAS_TITLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<t[iİı]tle[^>]*>[\s\x1c-\x1f]*[^\s\x1c-\x1f]").expect("HAS_TITLE_RE"));
/// `\beve\s*\.\s*load\s*\(` (only ever asked whether it matches): the boundary as the start or a non-word character.
static EVE_LOAD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\A|[^\p{L}\p{N}_])eve[\s\x1c-\x1f]*\.[\s\x1c-\x1f]*load[\s\x1c-\x1f]*\(").expect("EVE_LOAD_RE")
});
static EVE_SAVE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\A|[^\p{L}\p{N}_])eve[\s\x1c-\x1f]*\.[\s\x1c-\x1f]*save[\s\x1c-\x1f]*\(").expect("EVE_SAVE_RE")
});

/// py: apps_maker.py:606 `def slugify(text: str) -> str:`
pub fn slugify(text: &str) -> String {
    let low = text.to_lowercase();
    let words: Vec<&str> = WORD_RE.find_iter(&low).take(6).map(|m| m.as_str()).collect();
    let joined = words.join("-");
    let joined = if joined.is_empty() { "app".to_string() } else { joined };
    let cut = take(&joined, 40);
    let cut = cut.trim_matches('-');
    if cut.is_empty() { "app".into() } else { cut.to_string() }
}

/// py: apps_maker.py:611 `def _clean(text: str, limit: int) -> str:`
pub(crate) fn clean(text: &str, limit: i64) -> String {
    // `html.unescape`: the full HTML5 table, entities without their ";" ("&amp"), and Python's numeric
    // reference rules (&#128; is "€", &#0; is U+FFFD), which html_escape's decoder does not follow.
    let unescaped = eve_canvas::html::unescape(text);
    let spaced = SPACES_RE.replace_all(&unescaped, " ");
    take_py(py_strip(&spaced), limit)
}

/// py: apps_maker.py:615 `def title_of(html: str, fallback: str = "App") -> str:`
pub fn title_of(html: &str, fallback: &str) -> String {
    let raw = TITLE_RE.captures(html).and_then(|c| c.get(1)).map(|m| m.as_str()).unwrap_or("");
    let title = clean(raw, 60);
    if title.is_empty() { take(fallback, 60) } else { title }
}

/// py: apps_maker.py:620 `def _meta(html: str, name: str) -> str:`
///
/// The content of ``<meta name="<name>" content="…">``, whatever the attribute order.
pub(crate) fn meta(html: &str, name: &str) -> String {
    for tag in META_TAG_RE.find_iter(html) {
        let mut attrs: HashMap<String, String> = HashMap::new();
        for c in ATTR_RE.captures_iter(tag.as_str()) {
            let v1 = c.get(2).map(|m| m.as_str()).unwrap_or("");
            let v2 = c.get(3).map(|m| m.as_str()).unwrap_or("");
            attrs.insert(c[1].to_lowercase(), if v1.is_empty() { v2.to_string() } else { v1.to_string() });
        }
        if attrs.get("name").map(|n| n.to_lowercase()).unwrap_or_default() == name {
            return attrs.get("content").cloned().unwrap_or_default();
        }
    }
    String::new()
}

/// py: apps_maker.py:629 `def icon_of(html: str) -> str:`
///
/// The app's emoji (``<meta name="eve-icon">``): a short run of non-ASCII symbols, else nothing.
pub fn icon_of(html: &str) -> String {
    let icon = clean(&meta(html, "eve-icon"), 16).replace(' ', "");
    if !icon.is_empty() && char_len(&icon) <= 8 && !ASCII_RE.is_match(&icon.replace('\u{200d}', "")) {
        icon
    } else {
        String::new()
    }
}

/// py: apps_maker.py:635 `def description_of(html: str) -> str:`
pub fn description_of(html: &str) -> String {
    clean(&meta(html, "description"), 120)
}

/// py: apps_maker.py:639 `def extract_html(text: str) -> Option<String>:`
///
/// The HTML document in Claude's answer: the ```html block, else the whole answer if it is a document.
pub fn extract_html(text: &str) -> Option<String> {
    let m = HTML_BLOCK_RE.captures(text).or_else(|| DOCTYPE_BLOCK_RE.captures(text));
    let html = py_strip(m.as_ref().and_then(|c| c.get(1)).map(|g| g.as_str()).unwrap_or(text)).to_string();
    let low = html.to_lowercase();
    if (low.contains("<!doctype html") || low.contains("<html")) && low.contains("</html>") && html.len() <= MAX_APP_BYTES
    {
        return Some(html);
    }
    None
}

/// py: apps_maker.py:651 `_LOADS`
///
/// Where a page actually loads something from: a src/href on a loading tag, CSS url()/@import, fetch/import/
/// WebSocket/EventSource/sendBeacon. Plain links and namespace strings (the SVG xmlns) are not loads.
///
/// Python's patterns in Python's classes (see "Python's regex classes" above). Each `\b` is spelled out: after a
/// tag name and before "src", it is a non-word character (the two can be one); before "fetch" or "from", it is
/// the start or a non-word character the match takes in. A host is a greedy run of letters, so the character a
/// match takes in is never the end of the match before it.
pub(crate) static LOADS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        concat!(
            r#"(?i)<(?:scr[iİı]pt|l[iİı]nk|[iİı]mg|[iİı]frame|aud[iİı]o|v[iİı]deo|source|embed|object|track)"#,
            r#"[^\p{L}\p{N}_>](?:[^>]*?[^\p{L}\p{N}_>])??(?:src|href|data)[\s\x1c-\x1f]*=[\s\x1c-\x1f]*"#,
            r#"["']?[\s\x1c-\x1f]*(?:https?:)?//([a-z0-9İı.-]+)"#
        ),
        r#"(?i)(?:url\(|@[iİı]mport)[\s\x1c-\x1f]*["']?[\s\x1c-\x1f]*(?:https?:)?//([a-z0-9İı.-]+)"#,
        concat!(
            r#"(?i)(?:\A|[^\p{L}\p{N}_])(?:fetch|[iİı]mport|WebSocket|EventSource|sendBeacon)[\s\x1c-\x1f]*\("#,
            r#"[\s\x1c-\x1f]*["'`](?:(?:https?|wss?):)?//([a-z0-9İı.-]+)"#
        ),
        concat!(
            r#"(?i)\.open[\s\x1c-\x1f]*\([\s\x1c-\x1f]*["'](?:GET|POST|PUT|PATCH|DELETE)["'][\s\x1c-\x1f]*,"#,
            r#"[\s\x1c-\x1f]*["'`](?:https?:)?//([a-z0-9İı.-]+)"#
        ),
        r#"(?i)(?:\A|[^\p{L}\p{N}_])from[\s\x1c-\x1f]*["'](?:https?:)?//([a-z0-9İı.-]+)"#,
    ]
    .iter()
    .map(|p| Regex::new(p).expect("LOADS"))
    .collect()
});

/// py: apps_maker.py:662 `def load_issues(html: str) -> list[str]:`
///
/// Code or data the file would load from anywhere but a pinned npm package on jsdelivr. Not a repair hint
/// but a gate: a version with one of these is never saved (AppMaker._run), because a script from outside runs
/// with the app's access to the owner's data.
pub fn load_issues(html: &str) -> Vec<String> {
    let mut issues = Vec::new();
    let hosts: std::collections::BTreeSet<String> =
        LOADS.iter().flat_map(|rx| rx.captures_iter(html).map(|c| c[1].to_lowercase()).collect::<Vec<_>>()).collect();
    for host in hosts.iter().filter(|h| !ALLOWED_HOSTS.iter().any(|a| *h == a || h.ends_with(&format!(".{a}")))) {
        issues.push(format!(
            "The app loads from {host}; the only address it may load from is cdn.jsdelivr.net (everything else inline)."
        ));
    }
    let paths: std::collections::BTreeSet<String> = JSDELIVR_URL
        .captures_iter(html)
        .map(|c| c.get(1).map(|m| m.as_str()).filter(|s| !s.is_empty()).unwrap_or("/").to_string())
        .collect();
    for path in paths {
        // `PINNED_LIB_RE.match`: anchored at the start; the leftmost match starts there when one can
        let pinned = PINNED_LIB_RE.find(&path).is_some_and(|m| m.start() == 0);
        if !pinned {
            issues.push(format!(
                "The app loads https://cdn.jsdelivr.net{}, which is not an npm package at an exact version, so the \
                 code behind it can change after the app was tested. Load \
                 https://cdn.jsdelivr.net/npm/<package>@<exact version>/<file>, or do it inline.",
                take(&path, 80)
            ));
        }
    }
    issues
}

/// py: apps_maker.py:679 `def static_issues(html: str) -> list[str]:`
///
/// What is wrong with a file before it is even run: the storage contract, its name, where it loads from.
pub fn static_issues(html: &str) -> Vec<String> {
    let mut issues = Vec::new();
    if !HAS_TITLE_RE.is_match(html) {
        issues.push("The file has no <title>, so the app has no name in the owner's list.".to_string());
    }
    if !EVE_LOAD_RE.is_match(html) {
        issues.push("The app never calls eve.load(), so it cannot show anything it saved before.".to_string());
    }
    if !EVE_SAVE_RE.is_match(html) {
        issues.push("The app never calls eve.save(), so nothing the owner enters survives closing it.".to_string());
    }
    issues.extend(load_issues(html));
    issues
}

/// py: apps_maker.py:691 `def _sample(value: Any, depth: int = 0) -> Any:`
///
/// ``value`` with long lists and strings shortened, keeping its shape readable for a migration.
pub(crate) fn sample(value: &Value, depth: i64) -> Value {
    if depth > 8 {
        return json!("…");
    }
    match value {
        Value::Array(items) => {
            if items.len() > 8 {
                let mut out: Vec<Value> = items[..4].iter().map(|v| sample(v, depth + 1)).collect();
                out.push(json!(format!("… {} more items …", items.len() - 6)));
                out.extend(items[items.len() - 2..].iter().map(|v| sample(v, depth + 1)));
                return Value::Array(out);
            }
            Value::Array(items.iter().map(|v| sample(v, depth + 1)).collect())
        }
        Value::Object(map) => {
            let mut out: Obj = map.iter().take(40).map(|(k, v)| (k.clone(), sample(v, depth + 1))).collect();
            if map.len() > 40 {
                let last: Vec<String> = map.keys().skip(map.len() - 3).map(|k| str_repr(k)).collect();
                out.insert("…".into(), json!(format!("{} more keys, e.g. {}", map.len() - 40, last.join(", "))));
            }
            Value::Object(out)
        }
        Value::String(s) if char_len(s) > 200 => json!(format!("{}…", take(s, 200))),
        other => other.clone(),
    }
}

/// py: apps_maker.py:711 `def data_sample(value: Any, limit: int = DATA_SAMPLE_CHARS) -> str:`
pub fn data_sample(value: &Value, limit: i64) -> String {
    let raw = dumps(value, false);
    if char_len(&raw) as i64 <= limit {
        return raw;
    }
    let text = dumps(&sample(value, 0), false);
    if char_len(&text) as i64 <= limit { text } else { format!("{} …(cut)", take_py(&text, limit)) }
}

/// What the store raises, by the Python exception its callers catch.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// `KeyError`: no such app (or nothing to go back to, or nothing in the trash). Shown as Python shows the
    /// key.
    #[error("'{0}'")]
    Missing(String),
    /// `ValueError`: a bad name or too much data.
    #[error("{0}")]
    Invalid(String),
    /// `FileExistsError`: a live app has the address a restore needs.
    #[error("{0}")]
    Exists(String),
    /// `OSError`: the disk.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The store's result.
pub type StoreResult<T> = Result<T, StoreError>;

/// How `AppStore::save` writes the new page (``index.tmp``) and moves it over the live one: the disk, or a
/// test's disk that fills up (Python's tests patch ``Path.write_text`` and ``Path.replace``).
pub trait PageDisk {
    fn write(&self, path: &Path, text: &str) -> std::io::Result<()> {
        std::fs::write(path, text)
    }
    fn replace(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::rename(from, to)
    }
}

/// The disk itself.
pub struct RealDisk;

impl PageDisk for RealDisk {}

/// py: apps_maker.py:719 `class AppStore:`
///
/// The owner's apps on disk: ``<root>/<slug>/index.html``, ``meta.json``, ``data.json``, ``journeys.json`` and
/// ``versions/``. A page's journeys (jev_verify.py) are versioned with it: ``versions/<n>.html`` keeps its
/// ``versions/<n>.journeys.json`` beside it, so an undo restores both. An app built before journeys has none.
///
/// Every path is built from a slug that matches SLUG_RE in full and is not a symlink, so nothing a request or
/// a URL carries can reach outside ``root``.
pub struct AppStore {
    pub root: PathBuf,
}

impl Default for AppStore {
    fn default() -> Self {
        Self::new(&default_root())
    }
}

impl AppStore {
    /// py: apps_maker.py:727 `def __init__(self, root: Path = DEFAULT_ROOT) -> None:`
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf() }
    }

    /// py: apps_maker.py:730 `def _dir(self, slug: str) -> Optional[Path]:`
    pub(crate) fn dir(&self, slug: &str) -> Option<PathBuf> {
        if !SLUG_FULL.is_match(slug) {
            return None;
        }
        let d = self.root.join(slug);
        if is_symlink(&d) { None } else { Some(d) }
    }

    /// py: apps_maker.py:736 `def _app_dir(self, slug: str) -> Path:`
    ///
    /// The folder of an app that exists, else KeyError.
    pub(crate) fn app_dir(&self, slug: &str) -> StoreResult<PathBuf> {
        match self.dir(slug) {
            Some(d) if d.join("index.html").is_file() => Ok(d),
            _ => Err(StoreError::Missing(slug.to_string())),
        }
    }

    /// py: apps_maker.py:743 `def _meta(self, d: Path) -> dict[str, Any]:`
    pub(crate) fn meta(&self, d: &Path) -> Obj {
        let Some(Value::Object(mut m)) = std::fs::read_to_string(d.join("meta.json")).ok().and_then(|t| loads(&t)) else {
            return Obj::new();
        };
        // meta written before requests
        if !m.contains_key("requests") && truthy(m.get("request")) {
            let at = m.get("created").cloned().unwrap_or_else(|| json!(""));
            let text = py_str(&m["request"]);
            m.insert("requests".into(), json!([{"at": at, "text": text}]));
        }
        m
    }

    /// py: apps_maker.py:752 `def _write_meta(self, d: Path, meta: dict[str, Any]) -> None:`
    pub(crate) fn write_meta(&self, d: &Path, meta: &mut Obj) -> std::io::Result<()> {
        meta.shift_remove("request");
        let tmp = d.join("meta.tmp");
        std::fs::write(&tmp, dumps(&Value::Object(meta.clone()), false))?;
        std::fs::rename(&tmp, d.join("meta.json"))
    }

    /// The `*.html` in `versions/` whose stem is a number, oldest first.
    fn numbered(vdir: &Path) -> Vec<(u64, PathBuf)> {
        let mut found: Vec<(u64, PathBuf)> = std::fs::read_dir(vdir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "html"))
                    .filter_map(|p| digits(&stem(&p)).map(|n| (n, p)))
                    .collect()
            })
            .unwrap_or_default();
        found.sort_by_key(|(n, _)| *n);
        found
    }

    /// py: apps_maker.py:758 `def _versions(self, d: Path) -> list[Path]:`
    ///
    /// Earlier versions, oldest first. The single ``index.prev.html`` of the first builds becomes version 1.
    pub(crate) fn versions(&self, d: &Path) -> std::io::Result<Vec<PathBuf>> {
        let vdir = d.join("versions");
        let prev = d.join("index.prev.html");
        if prev.is_file() {
            mkdir_exist_ok(&vdir)?;
            let n = Self::numbered(&vdir).last().map(|(n, _)| *n).unwrap_or(0);
            if n == 0 {
                std::fs::rename(&prev, vdir.join("1.html"))?;
            } else {
                std::fs::remove_file(&prev)?;
            }
        }
        Ok(Self::numbered(&vdir).into_iter().map(|(_, p)| p).collect())
    }

    /// py: apps_maker.py:771 `def list(self) -> list[AppInfo]:`
    pub fn list(&self) -> Vec<AppInfo> {
        let mut apps: Vec<AppInfo> = std::fs::read_dir(&self.root)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().join("meta.json").exists())
                    .filter_map(|e| e.file_name().to_str().map(str::to_string))
                    .filter_map(|name| self.info(&name))
                    .collect()
            })
            .unwrap_or_default();
        apps.sort_by(|a, b| b.updated.cmp(&a.updated));
        apps
    }

    /// py: apps_maker.py:780 `def _data_v(d: Path) -> Optional[int]:`
    ///
    /// The ``v`` of the app's saved data, read from the start of data.json only (every app keeps one root
    /// object that starts with it, as the prompt asks): cheap enough for every listing. None when unknown.
    pub(crate) fn data_v(d: &Path) -> Option<i64> {
        use std::io::Read;
        let f = std::fs::File::open(d.join("data.json")).ok()?;
        let mut head = Vec::with_capacity(256);
        f.take(256).read_to_end(&mut head).ok()?;
        DATA_V_RE.captures(&head).and_then(|c| std::str::from_utf8(&c[1]).ok()?.parse().ok())
    }

    /// py: apps_maker.py:790 `def info(self, slug: str) -> Option<AppInfo>:`
    pub fn info(&self, slug: &str) -> Option<AppInfo> {
        let d = self.dir(slug)?;
        if !d.join("index.html").is_file() {
            return None;
        }
        let m = self.meta(&d);
        let versions = match self.versions(&d) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("apps: {} versions unreadable: {}", slug, e);
                return None;
            }
        };
        let mut warn = false;
        if let Some(last) = versions.last() {
            let then = match m.get("data_v") {
                Some(Value::Object(seen)) => py_int(seen.get(&stem(last))),
                _ => None,
            };
            if let Some(then) = then {
                warn = Self::data_v(&d).is_some_and(|now| now > then);
            }
        }
        let (Some(title), Some(created), Some(updated)) = (m.get("title"), m.get("created"), m.get("updated")) else {
            return None;
        };
        Some(AppInfo {
            slug: slug.to_string(),
            title: py_str(title),
            created: py_str(created),
            updated: py_str(updated),
            icon: get_str(&m, "icon", ""),
            description: get_str(&m, "description", ""),
            versions: versions.len() as i64,
            undo_warning: warn,
        })
    }

    /// py: apps_maker.py:808 `def html(self, slug: str) -> Option<String>:`
    pub fn html(&self, slug: &str) -> Option<String> {
        std::fs::read_to_string(self.dir(slug)?.join("index.html")).ok()
    }

    /// py: apps_maker.py:815 `def journeys(self, slug: str) -> Optional[list[jev_verify.Journey]]:`
    ///
    /// The journeys of the app's current page, or None when it has none (an app built before them).
    pub fn journeys(&self, slug: &str) -> Option<Vec<eve_jev_verify::Journey>> {
        let d = self.dir(slug)?;
        let raw = loads(&std::fs::read_to_string(d.join("journeys.json")).ok()?)?;
        if raw.is_null() {
            return None;
        }
        let (found, _) = eve_jev_verify::parse_journeys(&raw);
        if found.is_empty() { None } else { Some(found) }
    }

    /// py: apps_maker.py:828 `def _journeys_of(version: Path) -> Path:`
    pub(crate) fn journeys_of(version: &Path) -> PathBuf {
        version.with_file_name(format!("{}.journeys.json", stem(version)))
    }

    /// py: apps_maker.py:831 `def requests(self, slug: str) -> list[dict[str, str]]:`
    ///
    /// What the owner asked of this app, oldest first. Each request's fields as text (`str()` of what
    /// meta.json holds).
    pub fn requests(&self, slug: &str) -> Vec<HashMap<String, String>> {
        let Some(d) = self.dir(slug) else {
            return Vec::new();
        };
        match self.meta(&d).get("requests") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|r| r.as_object())
                .map(|r| r.iter().map(|(k, v)| (k.clone(), py_str(v))).collect())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// py: apps_maker.py:837 `def _exact(apps: list[AppInfo], name: str) -> list[AppInfo]:`
    ///
    /// The apps ``name`` names exactly. A slug is checked across every app before any title: two builds of
    /// "Notes" are ``notes`` and ``notes-2``, both titled Notes, and "notes" is the first one.
    pub(crate) fn exact(apps: &[AppInfo], name: &str) -> Vec<AppInfo> {
        let wanted = py_strip(name).to_lowercase();
        let s = slugify(name);
        let tests: [&dyn Fn(&AppInfo) -> bool; 3] =
            [&|a| a.slug == wanted, &|a| a.title.to_lowercase() == wanted, &|a| a.slug == s];
        for test in tests {
            let hits: Vec<AppInfo> = apps.iter().filter(|a| test(a)).cloned().collect();
            if !hits.is_empty() {
                return hits;
            }
        }
        Vec::new()
    }

    /// py: apps_maker.py:849 `def _words(text: str) -> set[str]:`
    ///
    /// The words that tell apps apart ("my habit trackers" -> {habit, tracker}).
    pub(crate) fn words(text: &str) -> HashSet<String> {
        let low = text.to_lowercase();
        WORD_RE
            .find_iter(&low)
            .map(|m| {
                let w = m.as_str();
                if w.len() > 3 && w.ends_with('s') { w[..w.len() - 1].to_string() } else { w.to_string() }
            })
            .filter(|w| !FILLER.contains(&w.as_str()))
            .collect()
    }

    fn words_of(a: &AppInfo) -> HashSet<String> {
        Self::words(&format!("{} {}", a.title, a.slug))
    }

    /// py: apps_maker.py:854 `def find(self, name: str) -> Option<AppInfo>:`
    ///
    /// The app a request names ("the habit tracker", "habit-tracker"): exact slug, then exact title, else the
    /// best word overlap (a tie goes to the newest). For a change, which keeps the old page as a version; a
    /// delete or an undo goes through ``resolve``.
    pub fn find(&self, name: &str) -> Option<AppInfo> {
        let apps = self.list();
        if let Some(first) = Self::exact(&apps, name).into_iter().next() {
            return Some(first);
        }
        let words = Self::words(name);
        let mut best: Option<(usize, &AppInfo)> = None;
        for a in &apps {
            let score = words.intersection(&Self::words_of(a)).count();
            // max() keeps the first of equals: the list is newest first
            if score > 0 && best.is_none_or(|(b, _)| score > b) {
                best = Some((score, a));
            }
        }
        best.map(|(_, a)| a.clone())
    }

    /// py: apps_maker.py:867 `def resolve(self, name: str) -> tuple[Optional[AppInfo], list[AppInfo]]:`
    ///
    /// The one app ``name`` surely means, for a delete, undo or rename: an exact slug or title, or the only
    /// app whose name holds every word asked for. Else (None, the apps it might mean), so the owner is asked
    /// which; nothing is picked by a guess.
    pub fn resolve(&self, name: &str) -> (Option<AppInfo>, Vec<AppInfo>) {
        Self::pick(&self.list(), name)
    }

    /// py: apps_maker.py:874 `def pick(cls, apps: list[AppInfo], name: str) -> tuple[Optional[AppInfo], list[AppInfo]]:`
    ///
    /// ``resolve`` over any list of apps (the trash's, for a restore).
    pub fn pick(apps: &[AppInfo], name: &str) -> (Option<AppInfo>, Vec<AppInfo>) {
        let mut exact = Self::exact(apps, name);
        if exact.len() == 1 {
            return (exact.pop(), Vec::new());
        }
        if !exact.is_empty() {
            return (None, exact);
        }
        let words = Self::words(name);
        if words.is_empty() {
            return (None, Vec::new());
        }
        let mut full: Vec<AppInfo> = apps.iter().filter(|a| words.is_subset(&Self::words_of(a))).cloned().collect();
        if full.len() == 1 {
            return (full.pop(), Vec::new());
        }
        if !full.is_empty() {
            return (None, full);
        }
        (None, apps.iter().filter(|a| !words.is_disjoint(&Self::words_of(a))).cloned().collect())
    }

    /// py: apps_maker.py:889 `def save(self, html: str, request: str, slug: str = "", journeys: Optional[list[jev_verify.Journey]] = None, build: Optional[dict[str, Any]] = None) -> AppInfo:`
    ///
    /// Keep ``html`` as the app ``slug`` (a new app when empty), with its ``journeys`` (None: this page has
    /// none). The page it replaces becomes the newest version with its journeys (at most MAX_VERSIONS are
    /// kept); the title follows the page's <title> unless the owner renamed the app. Each save is the app's
    /// next version number (``meta["version"]``); ``build`` is how the build that made the page went
    /// (``build_record``), kept with that number for the app's change chat (``thread``).
    pub fn save(&self, html: &str, request: &str, slug: &str, journeys: Option<&[eve_jev_verify::Journey]>, build: Option<&Obj>) -> StoreResult<AppInfo> {
        self.save_with(html, request, slug, journeys, build, &RealDisk)
    }

    /// `save`, with the new page's write and its move into place lent (a test's full disk).
    pub fn save_with(
        &self,
        html: &str,
        request: &str,
        slug: &str,
        journeys: Option<&[eve_jev_verify::Journey]>,
        build: Option<&Obj>,
        disk: &dyn PageDisk,
    ) -> StoreResult<AppInfo> {
        let now = now_iso();
        let mut slug = slug.to_string();
        if slug.is_empty() {
            let short = take(request, 40);
            let base = slugify(&title_of(html, if short.is_empty() { "App" } else { &short }));
            slug = base.clone();
            let mut n = 2;
            while self.root.join(&slug).exists() {
                slug = take(&format!("{base}-{n}"), 48);
                n += 1;
            }
        }
        let d = self.dir(&slug).ok_or_else(|| StoreError::Invalid("bad app name".into()))?;
        std::fs::create_dir_all(&d)?;
        let mut meta = self.meta(&d);
        // The new page is written in full before the live one moves: a disk that fills up, or any other failed
        // write, leaves the app as it was instead of an app with no page (which would vanish from the list).
        let tmp = d.join("index.tmp");
        let live = d.join("index.html");
        if let Err(e) = disk.write(&tmp, html) {
            let _ = unlink_missing_ok(&tmp);
            return Err(e.into());
        }
        let mut kept: Option<PathBuf> = None;
        if live.is_file() {
            let versions = self.versions(&d)?;
            mkdir_exist_ok(&d.join("versions"))?;
            let n = versions.last().and_then(|v| digits(&stem(v))).map(|n| n + 1).unwrap_or(1);
            let k = d.join("versions").join(format!("{n}.html"));
            std::fs::rename(&live, &k)?;
            kept = Some(k.clone());
            if d.join("journeys.json").is_file() {
                std::fs::rename(d.join("journeys.json"), Self::journeys_of(&k))?;
            }
            // which shape of data that page was written for: an undo to it can then warn (AppInfo.undo_warning)
            let mut seen = match meta.get("data_v") {
                Some(Value::Object(s)) => s.clone(),
                _ => Obj::new(),
            };
            seen.insert(n.to_string(), Self::data_v(&d).map_or(Value::Null, |v| json!(v)));
            meta.insert("data_v".into(), Value::Object(seen));
        }
        if let Err(e) = disk.replace(&tmp, &live) {
            if let Some(k) = &kept {
                // the page that was there comes back
                let _ = std::fs::rename(k, &live);
                if Self::journeys_of(k).is_file() {
                    let _ = std::fs::rename(Self::journeys_of(k), d.join("journeys.json"));
                }
            }
            let _ = unlink_missing_ok(&tmp);
            return Err(e.into());
        }
        match journeys {
            Some(j) if !j.is_empty() => {
                let jtmp = d.join("journeys.tmp");
                std::fs::write(&jtmp, dumps_indent1(&eve_jev_verify::journeys_json(j)))?;
                std::fs::rename(&jtmp, d.join("journeys.json"))?;
            }
            _ => unlink_missing_ok(&d.join("journeys.json"))?,
        }
        let versions = self.versions(&d)?;
        if versions.len() > MAX_VERSIONS {
            for old in &versions[..versions.len() - MAX_VERSIONS] {
                std::fs::remove_file(old)?;
                unlink_missing_ok(&Self::journeys_of(old))?;
            }
        }
        if let Some(Value::Object(dv)) = meta.get("data_v") {
            let names: HashSet<String> = self.versions(&d)?.iter().map(|p| stem(p)).collect();
            let kept_v: Obj = dv
                .iter()
                .filter(|(k, v)| names.contains(k.as_str()) && py_int(Some(v)).is_some())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            meta.insert("data_v".into(), Value::Object(kept_v));
        }
        let before: Vec<Value> = match meta.get("requests") {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        // An app from before version numbers counts its earlier builds (every request but an undo) as versions.
        let version = py_int(meta.get("version")).unwrap_or_else(|| {
            before.iter().filter(|r| r.as_object().is_some_and(|r| r.get("text") != Some(&json!(UNDO_REQUEST)))).count()
                as i64
        }) + 1;
        let n = Self::next(&mut meta);
        let mut requests = before;
        requests.push(json!({"at": now, "text": take(request, 500), "n": n}));
        let created = meta.get("created").cloned().unwrap_or_else(|| json!(now));
        meta.insert("created".into(), created);
        meta.insert("updated".into(), json!(now));
        let skip = requests.len().saturating_sub(MAX_REQUESTS);
        meta.insert("requests".into(), Value::Array(requests.split_off(skip)));
        meta.insert("version".into(), json!(version));
        if let Some(build) = build {
            let mut record = build.clone();
            record.insert("at".into(), json!(now));
            record.insert("request".into(), json!(take(request, 500)));
            record.insert("version".into(), json!(version));
            record.insert("n".into(), json!(n));
            Self::add_build(&mut meta, &record);
        }
        self.describe(&mut meta, html, request);
        self.write_meta(&d, &mut meta)?;
        self.info(&slug).ok_or(StoreError::Missing(slug))
    }

    /// py: apps_maker.py:966 `def _next(meta: dict[str, Any]) -> int:`
    ///
    /// The next number in the app's own order of events (``meta["seq"]``): two builds in the same second (a
    /// change that failed at once, then its retry) still show in the order they happened.
    pub(crate) fn next(meta: &mut Obj) -> i64 {
        let seq = py_int(meta.get("seq")).unwrap_or(0);
        meta.insert("seq".into(), json!(seq + 1));
        seq + 1
    }

    /// py: apps_maker.py:974 `def _add_build(meta: dict[str, Any], record: dict[str, Any]) -> None:`
    pub(crate) fn add_build(meta: &mut Obj, record: &Obj) {
        let mut builds: Vec<Value> = match meta.get("builds") {
            Some(Value::Array(a)) => a.iter().filter(|b| b.is_object()).cloned().collect(),
            _ => Vec::new(),
        };
        builds.push(Value::Object(record.clone()));
        let skip = builds.len().saturating_sub(MAX_BUILDS);
        meta.insert("builds".into(), Value::Array(builds.split_off(skip)));
    }

    /// py: apps_maker.py:978 `def record_build(self, slug: str, request: str, build: dict[str, Any]) -> None:`
    ///
    /// A build of an existing app that did not save a page (Claude failed, or nothing it wrote was usable):
    /// kept for the app's change chat, so a change that cost money and did nothing still shows there. It is
    /// not added to the requests, which tell the next build what the app was asked to do. KeyError when the
    /// app is gone.
    pub fn record_build(&self, slug: &str, request: &str, build: &Obj) -> StoreResult<()> {
        let d = self.app_dir(slug)?;
        let mut meta = self.meta(&d);
        let mut record = build.clone();
        record.insert("at".into(), json!(now_iso()));
        record.insert("request".into(), json!(take(request, 500)));
        let n = Self::next(&mut meta);
        record.insert("n".into(), json!(n));
        Self::add_build(&mut meta, &record);
        self.write_meta(&d, &mut meta)?;
        Ok(())
    }

    /// py: apps_maker.py:989 `def thread(self, slug: str) -> list[dict[str, Any]]:`
    ///
    /// The app's change chat, oldest first: every request (``kind`` "request", or "undo"), each with how its
    /// build went (``result``: build_record's fields plus ``version``) when that was recorded, and the builds
    /// that saved nothing. A request from before results were recorded has ``result`` None. KeyError when the
    /// app is gone.
    pub fn thread(&self, slug: &str) -> StoreResult<Vec<Obj>> {
        let meta = self.meta(&self.app_dir(slug)?);
        let builds: Vec<&Obj> = match meta.get("builds") {
            Some(Value::Array(a)) => a.iter().filter_map(|b| b.as_object()).collect(),
            _ => Vec::new(),
        };
        // 0: from before "n"
        let order = |x: &Obj| py_int(x.get("n")).unwrap_or(0);
        // a saved build and its request carry the same number (AppStore.save writes both at once)
        let mut saved: HashMap<i64, &Obj> = HashMap::new();
        for b in &builds {
            if truthy(b.get("ok")) && order(b) != 0 {
                saved.insert(order(b), b);
            }
        }
        let mut items: Vec<((String, i64), Obj)> = Vec::new();
        if let Some(Value::Array(requests)) = meta.get("requests") {
            for r in requests.iter().filter_map(|r| r.as_object()) {
                let at = get_str(r, "at", "");
                let text = get_str(r, "text", "");
                let b = if order(r) != 0 { saved.remove(&order(r)) } else { None };
                let mut item = Obj::new();
                item.insert("at".into(), json!(at));
                item.insert("text".into(), json!(text));
                item.insert("kind".into(), json!(if text == UNDO_REQUEST { "undo" } else { "request" }));
                item.insert("result".into(), b.map_or(Value::Null, |b| Value::Object(result(b))));
                items.push(((at, order(r)), item));
            }
        }
        for b in &builds {
            // a failed build, or a saved one whose request was trimmed off the (shorter-lived) request list
            if !truthy(b.get("ok")) || saved.contains_key(&order(b)) {
                let at = get_str(b, "at", "");
                let mut item = Obj::new();
                item.insert("at".into(), json!(at));
                item.insert("text".into(), json!(get_str(b, "request", "")));
                item.insert("kind".into(), json!("request"));
                item.insert("result".into(), Value::Object(result(b)));
                items.push(((at, order(b)), item));
            }
        }
        items.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(items.into_iter().map(|(_, item)| item).collect())
    }

    /// py: apps_maker.py:1016 `def _describe(self, meta: dict[str, Any], html: str, request: str = "") -> None:`
    pub(crate) fn describe(&self, meta: &mut Obj, html: &str, request: &str) {
        if !truthy(meta.get("renamed")) {
            let fallback = match meta.get("title") {
                t @ Some(v) if truthy(t) => py_str(v),
                _ => {
                    let short = take(request, 40);
                    if short.is_empty() { "App".to_string() } else { short }
                }
            };
            meta.insert("title".into(), json!(title_of(html, &fallback)));
        }
        let icon = icon_of(html);
        let icon = if icon.is_empty() { meta.get("icon").cloned().unwrap_or_else(|| json!("")) } else { json!(icon) };
        meta.insert("icon".into(), icon);
        let description = description_of(html);
        let description = if description.is_empty() {
            meta.get("description").cloned().unwrap_or_else(|| json!(""))
        } else {
            json!(description)
        };
        meta.insert("description".into(), description);
    }

    /// py: apps_maker.py:1022 `def rename(self, slug: str, title: str) -> AppInfo:`
    ///
    /// The owner's name for the app: kept through later changes. The slug (its address) stays.
    pub fn rename(&self, slug: &str, title: &str) -> StoreResult<AppInfo> {
        let d = self.app_dir(slug)?;
        let title = clean(title, 60);
        if title.is_empty() {
            return Err(StoreError::Invalid("an app needs a name".into()));
        }
        let mut meta = self.meta(&d);
        meta.insert("title".into(), json!(title));
        meta.insert("renamed".into(), json!(true));
        self.write_meta(&d, &mut meta)?;
        self.info(slug).ok_or_else(|| StoreError::Missing(slug.to_string()))
    }

    /// py: apps_maker.py:1035 `def revert(self, slug: str) -> AppInfo:`
    ///
    /// Undo the last change: the newest earlier version becomes the app again, and leaves the versions. The
    /// page it replaces is not kept (undo is a pop). KeyError when there is nothing to go back to.
    pub fn revert(&self, slug: &str) -> StoreResult<AppInfo> {
        let d = self.app_dir(slug)?;
        let versions = self.versions(&d)?;
        let Some(last) = versions.last() else {
            return Err(StoreError::Missing(format!("{slug} has no earlier version")));
        };
        std::fs::rename(last, d.join("index.html"))?;
        if Self::journeys_of(last).is_file() {
            std::fs::rename(Self::journeys_of(last), d.join("journeys.json"))?;
        } else {
            // that page was built before journeys
            unlink_missing_ok(&d.join("journeys.json"))?;
        }
        let html = std::fs::read_to_string(d.join("index.html"))?;
        let mut meta = self.meta(&d);
        if let Some(Value::Object(dv)) = meta.get_mut("data_v") {
            dv.shift_remove(&stem(last));
        }
        let updated = now_iso();
        meta.insert("updated".into(), json!(updated));
        meta.insert("icon".into(), json!(""));
        meta.insert("description".into(), json!(""));
        self.describe(&mut meta, &html, "");
        let mut requests: Vec<Value> = match meta.get("requests") {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        let n = Self::next(&mut meta);
        requests.push(json!({"at": updated, "text": UNDO_REQUEST, "n": n}));
        meta.insert("requests".into(), Value::Array(requests));
        self.write_meta(&d, &mut meta)?;
        self.info(slug).ok_or_else(|| StoreError::Missing(slug.to_string()))
    }

    /// py: apps_maker.py:1061 `def delete(self, slug: str) -> None:`
    ///
    /// Move the whole app (page, data, versions) to ``<root>/.trash/<slug>-<stamp>/``: out of the list, never
    /// erased, so a regretted delete can be undone (``restore``).
    pub fn delete(&self, slug: &str) -> StoreResult<()> {
        let d = self.app_dir(slug)?;
        let trash = self.root.join(TRASH);
        std::fs::create_dir_all(&trash)?;
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let mut target = trash.join(format!("{slug}-{stamp}"));
        let mut n = 2;
        while target.exists() {
            target = trash.join(format!("{slug}-{stamp}-{n}"));
            n += 1;
        }
        std::fs::rename(&d, &target)?;
        Ok(())
    }

    /// The trash's copies whose folder name is `<slug>-<stamp>` with a page in them: (folder, slug, stamp).
    fn trash_copies(&self) -> Vec<(PathBuf, String, String)> {
        let trash = self.root.join(TRASH);
        if !trash.is_dir() {
            return Vec::new();
        }
        std::fs::read_dir(&trash)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let name = e.file_name().to_str()?.to_string();
                        let c = TRASHED_RE.captures(&name)?;
                        let (slug, stamp) = (c[1].to_string(), c[2].to_string());
                        let d = e.path();
                        if is_symlink(&d) || !d.join("index.html").is_file() {
                            return None;
                        }
                        Some((d, slug, stamp))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// py: apps_maker.py:1073 `def trashed(self) -> list[AppInfo]:`
    ///
    /// Deleted apps that can come back, the most recently deleted first; one per slug (its newest delete).
    pub fn trashed(&self) -> Vec<AppInfo> {
        let mut found: HashMap<String, (String, AppInfo)> = HashMap::new();
        for (d, slug, stamp) in self.trash_copies() {
            let meta = self.meta(&d);
            let title = match meta.get("title") {
                t @ Some(v) if truthy(t) => py_str(v),
                _ => slug.clone(),
            };
            let info = AppInfo {
                slug: slug.clone(),
                title,
                created: get_str(&meta, "created", ""),
                updated: get_str(&meta, "updated", ""),
                icon: get_str(&meta, "icon", ""),
                description: get_str(&meta, "description", ""),
                versions: 0,
                undo_warning: false,
            };
            // when it was deleted, then the -2 of a second delete
            let key = format!("{stamp}{}", d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            if found.get(&slug).is_none_or(|(k, _)| key > *k) {
                found.insert(slug, (key, info));
            }
        }
        let mut out: Vec<(String, AppInfo)> = found.into_values().collect();
        out.sort_by(|a, b| b.0.cmp(&a.0));
        out.into_iter().map(|(_, info)| info).collect()
    }

    /// py: apps_maker.py:1090 `def restore(self, slug: str) -> AppInfo:`
    ///
    /// Bring the newest deleted copy of ``slug`` back from the trash. KeyError when there is none,
    /// FileExistsError when a live app has that address now (a new app took the name).
    pub fn restore(&self, slug: &str) -> StoreResult<AppInfo> {
        if self.dir(slug).is_none() {
            return Err(StoreError::Missing(slug.to_string()));
        }
        let mut copies: Vec<PathBuf> =
            self.trash_copies().into_iter().filter(|(_, s, _)| s == slug).map(|(d, _, _)| d).collect();
        copies.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        let Some(newest) = copies.pop() else {
            return Err(StoreError::Missing(slug.to_string()));
        };
        let home = self.root.join(slug);
        if home.exists() {
            return Err(StoreError::Exists(slug.to_string()));
        }
        std::fs::rename(&newest, &home)?;
        self.info(slug).ok_or_else(|| StoreError::Missing(slug.to_string()))
    }

    /// py: apps_maker.py:1109 `def load_data(self, slug: str) -> Any:`
    pub fn load_data(&self, slug: &str) -> StoreResult<Value> {
        let d = self.app_dir(slug)?;
        Ok(std::fs::read_to_string(d.join("data.json")).ok().and_then(|t| loads(&t)).unwrap_or(Value::Null))
    }

    /// py: apps_maker.py:1116 `def save_data(self, slug: str, value: Any) -> int:`
    pub fn save_data(&self, slug: &str, value: &Value) -> StoreResult<i64> {
        let d = self.app_dir(slug)?;
        let raw = dumps(value, true);
        if raw.len() > MAX_DATA_BYTES {
            return Err(StoreError::Invalid("too large".into()));
        }
        let tmp = d.join("data.tmp");
        std::fs::write(&tmp, &raw)?;
        std::fs::rename(&tmp, d.join("data.json"))?;
        Ok(char_len(&raw) as i64)
    }
}

/// An app's `updated` (a local ISO time, as meta.json keeps it) in seconds since the epoch; 0 when unreadable.
fn epoch_of(updated: &str) -> f64 {
    use chrono::TimeZone;
    chrono::DateTime::parse_from_rfc3339(updated)
        .map(|t| t.timestamp() as f64)
        .ok()
        .or_else(|| {
            let naive = chrono::NaiveDateTime::parse_from_str(updated, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
            chrono::Local.from_local_datetime(&naive).earliest().map(|t| t.timestamp() as f64)
        })
        .unwrap_or(0.0)
}

fn link_info(a: AppInfo) -> eve_link::seams::AppInfo {
    eve_link::seams::AppInfo { updated: epoch_of(&a.updated), slug: a.slug, title: a.title }
}

/// The store as Eve Link's app list reads it (link.py: `server.store.list()`, `.info(slug)`).
impl eve_link::seams::AppStoreView for AppStore {
    fn list(&self) -> Vec<eve_link::seams::AppInfo> {
        AppStore::list(self).into_iter().map(link_info).collect()
    }
    fn info(&self, slug: &str) -> Option<eve_link::seams::AppInfo> {
        AppStore::info(self, slug).map(link_info)
    }
}
