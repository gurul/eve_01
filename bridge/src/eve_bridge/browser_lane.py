"""The browser lane: Playwright drives a web task, the plan executor and Jev decide each step.

The fast lane's ceiling on the Mac is the Accessibility tree over web content: nameless buttons, stale
frames, coordinate clicks (Jev's holdout on the AX fixtures: 70.8 %, docs/stackchan/routing.md). A browser
driven through Playwright has none of that. Its page gives every control a role and an accessible name,
a click lands on the element and not on a point, and "did it work" is a DOM question with an exact answer
(the URL, the title, the text that is now on the page). Measured on this Mac, 2026-09-21: Chromium up in
0.19 s, a page in 0.52 s, a snapshot of every control in 40 ms.

This module changes the eyes and hands, not the brain. It presents a page as the same ``Snapshot`` of
``Candidate``s that ax_candidates.py builds from a window, so ``plan_executor.run_plan`` — astra's one
plan, the keyword gate, Jev's typed step answer, the sensitive-label table, "only the human approves" —
runs unchanged on top of it. Nothing about approval is reimplemented here.

    goal ── is it a web task? (WEB_WORDS, a URL, the router's search/open_url) ──▶ BrowserLane.run
                                                                                   │ outline of the page
                                                                                   ▼
                                                  astra plans once (plan_contract.plan_request)
                                                                                   │ Plan
                                                                                   ▼
                              plan_executor.run_plan(senses=PageSenses, effectors=PageEffectors, asker=Jev)
                                                                                   │ per step: candidates from
                                                                                   │ the DOM, gate + Jev pick,
                                                                                   ▼ Playwright clicks, expect
                                                            complete → one sentence · partial → today's loop

Production drives the owner's own Chrome (ATTACH MODE, below). The lane can also run eve's own persistent
Chromium profile under ``~/.config/eve-bridge/browser``; only tools and benches use that.

ATTACH MODE (owner, 2026-09-23: "i want eve to be able to control logged in browser, that's the most
important"). With ``EVE_BROWSER_ATTACH=1`` the lane drives the owner's own running Chrome, signed in
to everything, instead of its own profile. Chrome 136+ ignores ``--remote-debugging-port`` on the default
profile; the supported route (Chrome 144+) is the owner switching on remote debugging once at
``chrome://inspect/#remote-debugging``, after which Chrome writes ``DevToolsActivePort`` (port, then the
browser's WebSocket path) into its user data directory. ``devtools_endpoint`` reads that file exactly as
Google's chrome-devtools-mcp ``--autoConnect`` does, and Playwright's ``connect_over_cdp`` attaches.
EVE'S OWN CHROME (owner, 2026-09-27: "lets give eve its own profile"). With ``EVE_BROWSER_OWN=1`` the
lane drives a second Chrome that belongs to eve: real Google Chrome, started with its own user data directory
(``~/.config/eve-bridge/chrome``, ``EVE_OWN_CHROME_DIR`` moves it) and ``--remote-debugging-port=0``.
Chrome refuses that flag only on the owner's default directory, so this Chrome never asks "Allow remote
debugging?" and nobody presses anything. It stays open between tasks with the owner's sign-ins for eve
(``eve-bridge chrome-profile`` opens it to sign in). It is plain Chrome, not Playwright's launch, so it
carries no automation flags and Google's sign-in accepts it. The trade: any program on the Mac can drive this
Chrome through its port, so it holds only the accounts the owner signs into it, never the owner's own Chrome.
Own mode is attach mode with a different endpoint: the same tab rules and the same gate apply.
STEEL (owner, 2026-10-13: "replace browser with steel for eve", "local steel"). With ``EVE_BROWSER_STEEL=1`` the
lane drives a Chrome run by the self-hosted Steel browser API on this Mac (steel_local.py): eve starts the server
when nothing answers, opens a Steel session and attaches to its websocket. No API key, no window, no Allow; it
turns the lane on by itself and takes precedence over own and attach mode. Its profile keeps eve's sign-ins in
``~/.config/eve-bridge/steel-profile``. Closing the lane releases the session.

In attach mode the lane works in a NEW TAB of its own — never one of the owner's — and closing it closes
that tab and disconnects: the owner's browser, windows and tabs are never closed. The same gate applies as
everywhere: a sensitive control (buy, send, delete, pay…) stops the plan for the owner's yes.

Playwright's sync API must stay on the thread that created it, so the lane owns one worker thread and
everything runs there; the daemon awaits it.
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import hashlib
import json
import logging
import os
import re
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Awaitable, Callable, Iterable, Mapping, Optional
from urllib.parse import urlsplit, urlunsplit

from .ax_candidates import Candidate, Snapshot, is_sensitive, needs_confirmation

log = logging.getLogger(__name__)

DEFAULT_PROFILE = "~/.config/eve-bridge/browser"
DEFAULT_CHROME_DIR = "~/Library/Application Support/Google/Chrome"   # the owner's Chrome user data directory
DEFAULT_OWN_CHROME_DIR = "~/.config/eve-bridge/chrome"    # eve's own Chrome (see EVE'S OWN CHROME)
CHROME_APP = "Google Chrome"
OWN_START_SECS = 20.0                # eve's Chrome: how long a cold start may take to open its debugging port
READ_WAIT_SECS = 6.0                 # page_text: how long a still-growing page may take to settle
READ_POLL_SECS = 0.5
READ_ENOUGH_CHARS = 400              # below this a page is still loading (Gmail's shell is ~190)
SIGN_IN = re.compile(r"(/login|/signin|/sign_in|/ServiceLogin|accounts\.google\.com/.*(signin|ServiceLogin)|/ap/signin)", re.I)
DEFAULT_DEBUG_PORT = 9222            # what chrome://inspect/#remote-debugging shows as "Server running at"
CONSENT_TIMEOUT_MS = 120_000         # Chrome asks the owner "Allow remote debugging?" per connection: wait for the click
DEFAULT_VIEWPORT = (1280, 860)
MAX_CANDIDATES = 120
SNAPSHOT_TIMEOUT_MS = 3000
NAV_READ_POLL_MS = 50
NAV_TIMEOUT_MS = 15000
SETTLE_CAP_SECS = 3.0
ENTER_NAV_SECS = 0.6               # Return in a form: how long to wait for the navigation to start
KEYS = {"return": "Enter", "enter": "Enter", "escape": "Escape", "tab": "Tab", "space": " ", "up": "ArrowUp", "down": "ArrowDown",
        "left": "ArrowLeft", "right": "ArrowRight", "delete": "Backspace"}

# A goal for the web: a URL, a site, or the browser named. Anything else stays on the Mac lane.
WEB_WORDS = re.compile(
    r"(https?://|www\.|\.(com|org|net|io|ai|gov|edu)\b|\b(browser|chrome|safari|website|web ?page|webpage|"
    r"google|youtube|amazon|github|wikipedia|reddit|twitter|linkedin|gmail|news|hacker ?news|bbc|nytimes|"
    r"stack ?overflow|search (the )?web|look up|google it)\b)", re.I)

# What the page's JavaScript is asked for: every visible interactive element, with its accessible name,
# role, state and box. One evaluate, ~1 ms on a busy page; no screenshots, no OCR.
# Controls above or below the viewport are collected too, after every on-screen one (the cap cuts them
# first): a plan names "the Compare all plans button at the bottom", and the effectors scroll a control into
# view before they touch it. Only on-screen controls were collected before, and the plan vocabulary has no
# scroll, so a control below the fold could never be reached (browser_model_eval below_the_fold, 2026-09-24:
# 0 of 15 runs across five models). Off to the side (carousels, off-canvas menus) is still left out.
# Every earlier data-eve-id is cleared first, so a control that is no longer collected can never answer
# to an id that now belongs to another.
# A secret field's contents are never read: a password (by type, or by autocomplete when "show password"
# has made it plain text), a card field (autocomplete cc-*), a one-time code. Its value was copied into the
# snapshot, and became its name when it had no label, so what was typed into it reached the planner and Jev
# on the next step. Password fields are also `secure`, so the lane never types into them; card and code
# fields stay typeable. A field the vault filled (data-eve-filled, _FILL_FIELDS_JS) is secret too, whatever its
# attributes say: a card number in a field with no autocomplete hint, the username of a login.
# roleOf and nameOf are shared with _RECHECK_JS, so the click-time recheck reads a control exactly as it was
# collected.
_IDENTITY_JS = """
  const roleOf = (e) => {
    const r = e.getAttribute('role'); if (r) return r;
    const t = e.tagName.toLowerCase();
    if (t === 'a') return 'link';
    if (t === 'input') { const ty = (e.type || 'text').toLowerCase();
      if (ty === 'submit' || ty === 'button' || ty === 'reset') return 'button';
      if (ty === 'checkbox') return 'checkbox'; if (ty === 'radio') return 'radio';
      if (ty === 'search') return 'searchbox'; if (ty === 'password') return 'password'; return 'textbox'; }
    if (t === 'textarea') return 'textbox'; if (t === 'select') return 'combobox';
    if (t === 'summary') return 'button'; if (e.isContentEditable) return 'textbox';
    return t; };
  const tokens = (e) => (e.getAttribute('autocomplete') || '').toLowerCase().split(/\\s+/);
  const passwordLike = (e) => (e.type || '').toLowerCase() === 'password' ||
    tokens(e).some(a => a === 'current-password' || a === 'new-password');
  const secret = (e) => passwordLike(e) || e.hasAttribute('data-eve-filled') ||
    tokens(e).some(a => a.startsWith('cc-') || a === 'one-time-code');
  const nameOf = (e) => {
    const lab = e.getAttribute('aria-label'); if (lab) return lab;
    const by = e.getAttribute('aria-labelledby');
    if (by) { const t = by.split(/\\s+/).map(id => (document.getElementById(id) || {}).innerText || '').join(' ').trim(); if (t) return t; }
    if (e.labels && e.labels.length) { const t = Array.from(e.labels).map(l => l.innerText).join(' ').trim(); if (t) return t; }
    const own = (e.innerText || (secret(e) ? '' : e.value) || e.placeholder || e.title || e.getAttribute('alt') || '').trim();
    if (own) return own;
    const img = e.querySelector('img[alt],svg[aria-label],[aria-label]');
    return img ? (img.getAttribute('alt') || img.getAttribute('aria-label') || '').trim() : ''; };
  const hrefOf = (e) => e.tagName.toLowerCase() === 'a' && e.hasAttribute('href') ? e.href : '';
"""
_COLLECT_JS = """() => {
  const sel = 'a[href],button,input,textarea,select,summary,[role=button],[role=link],[role=tab],[role=menuitem],' +
              '[role=checkbox],[role=radio],[role=textbox],[role=combobox],[role=option],[role=switch],' +
              '[role=searchbox],[role=menuitemcheckbox],[role=menuitemradio],[contenteditable=true]';""" + _IDENTITY_JS + """
  for (const e of document.querySelectorAll('[data-eve-id]')) e.removeAttribute('data-eve-id');
  const out = []; const H = window.innerHeight, W = window.innerWidth; let i = 0;
  const onscreen = [], offscreen = [];
  for (const e of document.querySelectorAll(sel)) {
    const r = e.getBoundingClientRect();
    if (r.width < 2 || r.height < 2 || r.right < 0 || r.left > W) continue;
    const cs = getComputedStyle(e); if (cs.visibility === 'hidden' || cs.display === 'none' || cs.opacity === '0') continue;
    ((r.bottom < 0 || r.top > H) ? offscreen : onscreen).push([e, r]); }
  for (const [e, r] of onscreen.concat(offscreen)) {
    const role = roleOf(e); const name = nameOf(e).replace(/\\s+/g, ' ').slice(0, 120);
    let value = '';
    if (role === 'checkbox' || role === 'switch' || role === 'menuitemcheckbox') value = (e.checked || e.getAttribute('aria-checked') === 'true') ? 'checked' : 'unchecked';
    else if (role === 'radio' || role === 'menuitemradio' || role === 'tab' || role === 'option') value = (e.checked || e.getAttribute('aria-checked') === 'true' || e.getAttribute('aria-selected') === 'true') ? 'selected' : '';
    else if (e.getAttribute('aria-expanded') !== null) value = e.getAttribute('aria-expanded') === 'true' ? 'expanded' : 'collapsed';
    else if (secret(e)) value = '';
    else if ((role === 'textbox' || role === 'searchbox' || role === 'combobox') && typeof e.value === 'string') value = e.value.slice(0, 40);
    const editable = ['textbox', 'searchbox', 'combobox'].includes(role) && !(e.tagName.toLowerCase() === 'select');
    e.setAttribute('data-eve-id', String(i));
    out.push({id: String(i++), role, name, value, x: Math.round(r.left), y: Math.round(r.top), w: Math.round(r.width),
              h: Math.round(r.height), enabled: !(e.disabled || e.getAttribute('aria-disabled') === 'true'),
              secure: role === 'password' || passwordLike(e), editable, dialog: !!e.closest('dialog,[role=dialog],[role=alertdialog]'),
              focused: document.activeElement === e, offscreen: r.bottom < 0 || r.top > H,
              href: hrefOf(e)});
    if (out.length >= %d) break; }
  const dlg = Array.from(document.querySelectorAll('dialog[open],[role=dialog],[role=alertdialog]')).find(d => {
    const r = d.getBoundingClientRect(); const cs = getComputedStyle(d);
    return r.width >= 2 && r.height >= 2 && cs.visibility !== 'hidden' && cs.display !== 'none'; });
  const focused = document.activeElement;
  return {elements: out, title: document.title, url: location.href,
          dialog: dlg ? (dlg.innerText || '').trim().slice(0, 80) : '',
          focused_id: focused && focused.getAttribute ? focused.getAttribute('data-eve-id') : null,
          document_id: String(performance.timeOrigin), head: (document.body ? document.body.innerText : '').slice(0, 600),
          text_hash: (document.body ? document.body.innerText : '').length + ':' + document.body?.innerText?.slice(0, 4000)}; }
""" % MAX_CANDIDATES

# The click-time recheck (click_candidate): the control under the id the decision named, read as _COLLECT_JS
# read it, and the document it is in. A re-render can put another control under that id, and a navigation or
# a reload can replace the whole document, in the seconds between the snapshot and the click.
_RECHECK_JS = """(want) => {""" + _IDENTITY_JS + """
  const e = document.querySelector('[data-eve-id="' + want.id + '"]');
  const now = {url: location.href, doc: String(performance.timeOrigin), found: !!e};
  if (!e) return now;
  return Object.assign(now, {role: roleOf(e), name: nameOf(e).replace(/\\s+/g, ' ').slice(0, 120), href: hrefOf(e)}); }
"""
STALE_PREFIX = "refused: stale:"     # click_candidate: the page moved under the decision; nothing was clicked
BLOCKED_PREFIX = "refused: blocked:"  # every effector on a bot-check page (browser_blocker)


def click_recheck(recorded: Mapping[str, Any], now: Any) -> list[str]:
    """What differs between the control as the decision saw it and as the page has it now, in words; [] when
    it is the same control in the same document. Anything unreadable is "control": fail closed.
    After LayaBrowse's decision_still_valid (MIT, Copyright (c) 2026 Aryan Bhujade)."""
    if not isinstance(now, Mapping) or not now.get("found"):
        return ["control"]
    changed: list[str] = []
    for key, word in (("url", "page address"), ("doc", "document"), ("role", "role"), ("name", "name"),
                      ("href", "href")):
        if str(recorded.get(key) or "") != str(now.get(key) or ""):
            changed.append(word)
    return changed


# Bot checks: pages that stand between eve and the site until a human proves they are one. They are not the
# task's page and this lane must never try to solve one (no "I'm not a robot" clicks), so the lane stops with
# a line for the owner instead of handing the page on to a planner that would.
# Ported from LayaBrowse goals.browser_blocker (MIT, Copyright (c) 2026 Aryan Bhujade), with Cloudflare's
# interstitial added. Google's /sorry/ counts only on a Google host: "/sorry/" alone is an ordinary path.
CHALLENGE_TITLES = frozenset({"pardon our interruption", "access denied", "robot check", "just a moment",
                              "attention required! | cloudflare"})
CHALLENGE_TEXT = ("checking your browser before you access", "checking your browser before accessing",
                  "needs to review the security of your connection before proceeding")
CHALLENGE_HEAD_CHARS = 500            # the phrases count only at the top of the page, where the check says them


def browser_blocker(url: str, title: str, text: str) -> str:
    """ "browser_challenge" when the page is a known bot check, else "". Pure: the URL, the title, the text."""
    try:
        parts = urlsplit(str(url or ""))
        host, path = (parts.hostname or "").lower(), parts.path
    except ValueError:
        host, path = "", ""
    if path.startswith("/splashui/challenge"):
        return "browser_challenge"
    if path.startswith("/sorry/") and (host == "google.com" or host.endswith(".google.com")):
        return "browser_challenge"
    if str(title or "").casefold().strip().rstrip(".…").strip() in CHALLENGE_TITLES:
        return "browser_challenge"
    head = " ".join(str(text or "")[:CHALLENGE_HEAD_CHARS].split()).casefold()
    if any(phrase in head for phrase in CHALLENGE_TEXT):
        return "browser_challenge"
    return ""


def blocked_line(url: str) -> str:
    """What eve tells the owner when a bot check stops the lane: the site (never the path or query) and what to do."""
    host = observed_site(url).removeprefix("www.") or "that site"
    return (f"{host} is showing a bot check, so I stopped: I can't get past it myself. "
            f"Open {host} in Chrome, finish the check, then ask me again.")


# WebMCP (developer.chrome.com/docs/ai/webmcp): a page can register its own tools on document.modelContext,
# which an agent could call instead of clicking. Measurement only: how many pages eve visits offer any.
# The lane never calls them. getTools() is raced against 300 ms so a page that never answers cannot hold a step.
_WEBMCP_JS = """async () => {
  const mc = document.modelContext;
  if (!mc) return {api: false};
  if (typeof mc.getTools !== 'function') return {api: true, names: null};
  try {
    const tools = await Promise.race([mc.getTools(), new Promise(r => setTimeout(() => r(null), 300))]);
    if (!Array.isArray(tools)) return {api: true, names: null};
    return {api: true, names: tools.slice(0, 50).map(t => String((t && t.name) || ''))};
  } catch (e) { return {api: true, names: null}; }
}"""

# A cookie or consent notice covering the page. Sites put it in a dialog, which the lane (rightly) never
# operates, so every click stopped at dialog_open and no plan dismissed it first (browser_model_eval
# cookie_banner, 2026-09-24: 0 of 15 runs across five models). Code dismisses it before a click or a type:
# only a notice whose text is about cookies or consent, and only with a button that REFUSES or closes —
# never one that agrees. A label the sensitive table knows ("Accept all", "Agree", "OK") is never pressed
# here; a notice that offers nothing else stays up, and the step stops at dialog_open as before.
CONSENT_TEXT = re.compile(r"\b(cookies?|consent|gdpr|tracking technologies|privacy (settings|preferences|choices))\b", re.I)
CONSENT_REFUSE = re.compile(r"\b(reject|refuse|deny|necessary|essential|required only|only required|no,? thanks)\b", re.I)
CONSENT_CLOSE = re.compile(r"^\s*(close|dismiss|not now|×|✕|x)\s*$", re.I)
_CONSENT_JS = r"""(pattern) => {
  const re = new RegExp(pattern, 'i');
  const vis = (e) => { const r = e.getBoundingClientRect(); if (r.width < 2 || r.height < 2) return false;
    const cs = getComputedStyle(e); return cs.visibility !== 'hidden' && cs.display !== 'none' && cs.opacity !== '0'; };
  const boxes = Array.from(document.querySelectorAll('dialog[open],[role=dialog],[role=alertdialog],[aria-modal=true],' +
    '[id*=cookie i],[class*=cookie i],[id*=consent i],[class*=consent i]')).filter(vis);
  for (const b of boxes) {
    const text = (b.innerText || '').trim();
    if (!re.test(text)) continue;
    const btns = Array.from(b.querySelectorAll('button,[role=button],input[type=button],input[type=submit]')).filter(vis);
    for (const e of document.querySelectorAll('[data-eve-consent]')) e.removeAttribute('data-eve-consent');
    btns.forEach((e, i) => e.setAttribute('data-eve-consent', String(i)));
    return {text: text.replace(/\s+/g, ' ').slice(0, 200), buttons: btns.map((e, i) => ({id: String(i),
      name: (e.getAttribute('aria-label') || e.innerText || e.value || e.title || '').trim().replace(/\s+/g, ' ').slice(0, 80)}))};
  }
  return null; }"""


def consent_choice(buttons: list[Mapping[str, Any]]) -> Optional[Mapping[str, Any]]:
    """The button that dismisses a consent notice without agreeing to anything, or None: a refusal first
    ("Reject non-essential cookies", "Necessary only"), else a plain close. A sensitive label never."""
    for rx in (CONSENT_REFUSE, CONSENT_CLOSE):
        for b in buttons:
            name = str(b.get("name") or "")
            if name and rx.search(name) and not is_sensitive(name):
                return b
    return None


ROLE_WORDS = {"link": "link", "button": "button", "checkbox": "checkbox", "radio": "radio button",
              "searchbox": "search field", "textbox": "text field", "password": "secure text field",
              "combobox": "combo box", "tab": "tab", "menuitem": "menu item", "option": "option",
              "switch": "switch", "menuitemcheckbox": "checkbox", "menuitemradio": "radio button"}


@dataclass(frozen=True)
class BrowserLaneConfig:
    profile: Path = Path(DEFAULT_PROFILE).expanduser()
    headless: bool = False
    attach: bool = False                          # drive the owner's running Chrome (see ATTACH MODE)
    chrome_dir: Path = Path(DEFAULT_CHROME_DIR).expanduser()
    debug_port: int = DEFAULT_DEBUG_PORT
    chrome_profile: str = ""                      # attach: the Google account whose Chrome profile to work in
    own: bool = False                             # drive eve's own Chrome, not the owner's (implies attach)
    own_dir: Path = Path(DEFAULT_OWN_CHROME_DIR).expanduser()
    steel: bool = False                           # drive a local Steel session (see STEEL; implies attach)


def configured(environ: Any = None) -> BrowserLaneConfig:
    """``EVE_BROWSER_ATTACH=1`` drives the owner's running Chrome (``EVE_CHROME_DIR`` moves where its
    ``DevToolsActivePort`` is looked for; ``EVE_CHROME_DEBUG_PORT`` is the fallback port;
    ``EVE_CHROME_PROFILE`` names the Google account whose Chrome profile to work in).
    ``EVE_BROWSER_OWN=1`` drives eve's own Chrome instead, and turns the lane on by itself
    (``EVE_OWN_CHROME_DIR`` moves its user data directory). ``EVE_BROWSER_STEEL=1`` drives a local Steel
    session instead of either (steel_local.py), and also turns the lane on by itself."""
    env = os.environ if environ is None else environ
    on = ("1", "true", "yes", "on")
    steel = (env.get("EVE_BROWSER_STEEL") or "0").strip().lower() in on
    own = not steel and (env.get("EVE_BROWSER_OWN") or "0").strip().lower() in on
    attach = steel or own or (env.get("EVE_BROWSER_ATTACH") or "0").strip().lower() in on
    own_dir = Path((env.get("EVE_OWN_CHROME_DIR") or "").strip() or DEFAULT_OWN_CHROME_DIR).expanduser()
    chrome_dir = Path((env.get("EVE_CHROME_DIR") or "").strip() or DEFAULT_CHROME_DIR).expanduser()
    try:
        debug_port = int(str(env.get("EVE_CHROME_DEBUG_PORT") or DEFAULT_DEBUG_PORT))
    except ValueError:
        debug_port = DEFAULT_DEBUG_PORT
    return BrowserLaneConfig(attach=attach, chrome_dir=chrome_dir, debug_port=debug_port,
                             chrome_profile=(env.get("EVE_CHROME_PROFILE") or "").strip().lower(),
                             own=own, own_dir=own_dir, steel=steel)


# Which Chrome profile a context is: the Google accounts signed in there, from Google's own account list,
# fetched with THAT profile's cookies (context.request) — no tab opens. The first listed is the profile's
# primary account.
ACCOUNTS_URL = "https://accounts.google.com/ListAccounts?gpsia=1&source=ChromiumBrowser&json=standard"
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
# The whole lookup, every profile at once, gets this long. Production, 2026-09-23/24: one request after another
# at 8 s each cost 7 s and 19 s before a task began, and identified nothing; the one quick run took 1.4 s.
PROFILE_LOOKUP_BUDGET_SECS = 3.0
CHROME_UA = ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) "
             "Chrome/140.0.0.0 Safari/537.36")


def fetch_accounts(url: str, cookie_header: str, timeout: float) -> str:
    """Google's account list, asked with one profile's cookies. Runs on a lookup thread, never the lane's."""
    import urllib.request

    req = urllib.request.Request(url, headers={"Cookie": cookie_header, "User-Agent": CHROME_UA})
    with urllib.request.urlopen(req, timeout=timeout) as r:     # noqa: S310 — a fixed https URL
        return r.read().decode("utf-8", errors="replace")


def accounts_in(body: str) -> list[str]:
    """The signed-in accounts in a ListAccounts reply, primary first, deduplicated, lower-cased."""
    seen: list[str] = []
    for m in EMAIL.findall(body or ""):
        e = m.lower()
        if e not in seen:
            seen.append(e)
    return seen


def profile_for(request: str, profiles: dict[str, Any], default: str = "") -> str:
    """The profile (primary account) a request names — an email, its part before @, or its domain word
    ("era", "uw", "gmail") — else ``default`` if open, else "" (the first profile)."""
    words = set(re.findall(r"[a-z0-9.@-]+", request.lower()))
    for email in profiles:
        local, _, domain = email.partition("@")
        names = {email, local, domain, domain.split(".")[0]}
        if words & names:
            return email
    return default if default in profiles else ""


def _listening(port: int) -> bool:
    import socket

    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.5):
            return True
    except OSError:
        return False


def devtools_endpoint(chrome_dir: Path, port: int = DEFAULT_DEBUG_PORT) -> Optional[str]:
    """The running Chrome's browser WebSocket, or None when remote debugging is off or Chrome is not running.

    First ``<chrome_dir>/DevToolsActivePort`` (a port line, then a path line), the same reading as
    chrome-devtools-mcp's autoConnect. macOS's app-data protection refuses that read to other apps
    ("Operation not permitted", 2026-09-23), so when the file cannot be read the endpoint is Chrome's
    debugging ``port`` (the one chrome://inspect shows) at ``/devtools/browser`` — Chrome then asks the owner
    "Allow remote debugging?" for the connection, and that click, not the file, is the gate."""
    try:
        lines = [ln.strip() for ln in (Path(chrome_dir) / "DevToolsActivePort").read_text().splitlines() if ln.strip()]
    except PermissionError:
        return f"ws://127.0.0.1:{port}/devtools/browser" if _listening(port) else None
    except OSError:
        return None
    if len(lines) < 2 or not lines[0].isdigit() or not lines[1].startswith("/devtools/browser/"):
        return None
    port = int(lines[0])
    return f"ws://127.0.0.1:{port}{lines[1]}" if 0 < port < 65536 else None


def ensure_chrome_window(run: Callable[..., Any] = None) -> bool:
    """Chrome refuses a remote-debugging connection with 403 when it has NO window — the "Allow?" prompt has
    nowhere to show (live, 2026-09-23: Chrome running, zero windows, 403 at once; one window, connected). On a
    Mac, closing every window leaves Chrome running, so this is common: open a plain new window if there is
    none. True when Chrome has a window afterwards."""
    import subprocess

    run = run or subprocess.run

    def windows() -> int:
        r = run(["osascript", "-e", 'if application "Google Chrome" is running then tell application "Google Chrome" '
                 'to count windows'], capture_output=True, text=True, timeout=10)
        try:
            return int((r.stdout or "0").strip() or 0)
        except ValueError:
            return 0

    if windows() > 0:
        return True
    run(["osascript", "-e", 'tell application "Google Chrome" to make new window'], capture_output=True, text=True,
        timeout=10)
    time.sleep(1.0)
    return windows() > 0


CHROME_BINARY = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"   # the main process, never a Helper


def owner_chrome_running(run: Callable[..., Any] = None) -> bool:
    """True when the owner's own Chrome (the default user data directory) is running. eve's own Chrome is a
    second Chrome process with ``--user-data-dir=<own dir>``; it does not count, because Codex's browser
    extension lives only in the owner's profile."""
    import subprocess

    run = run or subprocess.run
    r = run(["ps", "-axo", "command="], capture_output=True, text=True, timeout=10)
    default = str(Path(DEFAULT_CHROME_DIR).expanduser())
    for line in (r.stdout or "").splitlines():
        if not line.startswith(CHROME_BINARY) or line.startswith(CHROME_BINARY + " Helper"):
            continue
        m = re.search(r"--user-data-dir=(.+?)(?= --|$)", line)   # the default folder has a space in it
        if m is None or str(Path(m.group(1)).expanduser()) == default:
            return True
    return False


def ensure_owner_chrome(run: Callable[..., Any] = None, sleep: Callable[[float], None] = time.sleep,
                        wait_secs: float = 10.0, settle_secs: float = 3.0) -> bool:
    """Start the owner's Chrome when it is not running, so Codex's browser extension has a browser to connect
    from. Live, 2026-10-07: only eve's own Chrome was open, the chrome lane handed a Turnitin login to Codex,
    and Codex answered "the browser-control connection is unavailable" in 8 s. ``open -a`` would only bring
    the running eve Chrome forward, so this is ``open -na``: a new instance on the default profile.
    True when the owner's Chrome is running afterwards."""
    import subprocess

    run = run or subprocess.run
    if owner_chrome_running(run):
        return True
    log.info("browser lane: the owner's Chrome is not running; starting it for Codex's browser extension")
    run(["open", "-na", CHROME_APP], capture_output=True, text=True, timeout=10)
    deadline = time.monotonic() + wait_secs
    while time.monotonic() < deadline:
        sleep(0.5)
        if owner_chrome_running(run):
            sleep(settle_secs)                     # the extension's service worker connects after the window
            return True
    log.warning("browser lane: the owner's Chrome did not start within %.0f s", wait_secs)
    return False


class AttachError(RuntimeError):
    """The owner's Chrome cannot be reached: remote debugging is off, or Chrome is not running."""


def own_chrome_command(own_dir: Path, url: str = "") -> list[str]:
    """Start eve's own Chrome, or hand ``url`` to it when it is already running (Chrome passes a second
    launch on the same user data directory to the running one). Port 0: Chrome picks a free port and writes it
    to ``<own_dir>/DevToolsActivePort``, so it never collides with the owner's Chrome."""
    return ["open", "-na", CHROME_APP, "--args", f"--user-data-dir={Path(own_dir)}", "--remote-debugging-port=0",
            "--no-first-run", "--no-default-browser-check", *([url] if url else [])]


def sign_in_command(own_dir: Path, url: str) -> list[str]:
    """Eve's own Chrome with NO debugging port, for the owner to sign in: Google's sign-in may refuse a
    Chrome that another program can drive (owner, 2026-09-27: "i dont think u can sign into here"). The
    sign-ins stay in ``own_dir``; the next start with the port is already signed in."""
    return ["open", "-na", CHROME_APP, "--args", f"--user-data-dir={Path(own_dir)}", "--no-first-run",
            "--no-default-browser-check", url]


def own_chrome_running(own_dir: Path, run: Callable[..., Any] = None) -> bool:
    """True while any Chrome runs on eve's folder, with the debugging port or without it."""
    import subprocess

    run = run or subprocess.run
    r = run(["pgrep", "-f", "--", f"--user-data-dir={Path(own_dir)}"], capture_output=True, text=True, timeout=10)
    return bool((r.stdout or "").strip())


def quit_own_chrome(own_dir: Path, wait_secs: float = 10.0) -> bool:
    """Quit eve's own Chrome through its own port (Browser.close), never by app name: the owner's Chrome is
    the same app. True when it is not running afterwards."""
    endpoint = _live_endpoint(Path(own_dir))
    if endpoint is not None:
        from playwright.sync_api import sync_playwright

        with sync_playwright() as pw:
            browser = pw.chromium.connect_over_cdp(endpoint, timeout=10000)
            try:
                browser.new_browser_cdp_session().send("Browser.close")
            except Exception:  # noqa: BLE001 — the connection drops as Chrome quits
                pass
    deadline = time.monotonic() + wait_secs
    while own_chrome_running(own_dir):
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.25)
    return True


def _live_endpoint(chrome_dir: Path) -> Optional[str]:
    """``devtools_endpoint``, believed only while something listens on its port: a Chrome that has quit leaves
    its DevToolsActivePort behind."""
    endpoint = devtools_endpoint(chrome_dir)
    if endpoint is None:
        return None
    try:
        port = int(endpoint.split(":")[2].split("/")[0])
    except (IndexError, ValueError):
        return None
    return endpoint if _listening(port) else None


def start_own_chrome(own_dir: Path, *, url: str = "", run: Callable[..., Any] = None,
                     wait_secs: float = OWN_START_SECS, poll_secs: float = 0.25) -> str:
    """Eve's own Chrome's browser WebSocket, starting that Chrome when it is not running (and handing it
    ``url`` when one is given). Raises AttachError when it does not come up within ``wait_secs``."""
    import subprocess

    own_dir = Path(own_dir)
    endpoint = _live_endpoint(own_dir)
    if endpoint is not None and not url:
        return endpoint
    own_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    if endpoint is None:
        try:
            (own_dir / "DevToolsActivePort").unlink()      # stale: the new Chrome writes its own
        except OSError:
            pass
    run = run or subprocess.run
    run(own_chrome_command(own_dir, url), capture_output=True, text=True, timeout=15)
    deadline = time.monotonic() + wait_secs
    while True:
        endpoint = _live_endpoint(own_dir)
        if endpoint is not None:
            log.info("browser lane: eve's own Chrome is up (%s)", own_dir)
            return endpoint
        if time.monotonic() >= deadline:
            if own_chrome_running(own_dir):
                raise AttachError("eve's own Chrome is open for signing in (no port): quit it (⌘Q in that "
                                  "Chrome) and eve can use it")
            raise AttachError(f"eve's own Chrome did not start within {wait_secs:.0f} s ({own_dir})")
        time.sleep(poll_secs)


def is_web_goal(goal: str, route_kind: str = "") -> bool:
    """Code decides which lane sees a goal; the browser gets a URL, a site, or a named browser."""
    return route_kind in ("open_url", "search") or bool(WEB_WORDS.search(goal or ""))


# The Chrome lane drives one browser tab and nothing else, so a web goal that also needs the Mac goes to Codex,
# which has both. "The Mac" is said outright ("on my computer", "the desktop app"), or it is a Mac app named by
# a name no one uses for anything else (Spotify, Photo Booth, Slack) with no browser named for it. Apple's
# everyday-word apps (News, Weather, Maps …) are left out: "the latest news on Google" is a web search.
ON_THE_MAC = re.compile(r"\b(?:on|in|from) (?:my|the) (?:computer|mac|macbook|laptop|desktop)\b|"
                        r"\b(?:desktop|native|mac) (?:app|version)\b", re.I)
IN_A_BROWSER = re.compile(r"(https?://|www\.|\b(?:in|on|using|with|via|through) (?:a |the |my )?(?:web ?)?"
                          r"(?:browser|chrome|google chrome|safari|firefox|arc|brave|edge|web|internet)\b|"
                          r"\bweb (?:version|app|player)\b)", re.I)
BROWSER_APPS = frozenset({"google chrome", "safari", "firefox", "arc", "brave browser", "microsoft edge", "chromium"})
EVERYDAY_WORD_APPS = frozenset({
    "apps", "books", "calendar", "chess", "clock", "console", "contacts", "dictionary", "freeform", "games", "home",
    "journal", "linear", "magnifier", "mail", "maps", "messages", "music", "news", "notes", "passwords", "phone",
    "photos", "podcasts", "preview", "reminders", "screenshot", "shortcuts", "siri", "stocks", "tips", "tuner", "tv",
    "weather"})


def is_browser_only_goal(goal: str, apps: Iterable[str] = ()) -> bool:
    """A web goal (is_web_goal) that the Chrome lane can finish alone: nothing in it is on the Mac."""
    from .task_router import _explicit_search, app_mentions

    if not is_web_goal(goal) or ON_THE_MAC.search(goal or ""):
        return False
    if _explicit_search(goal):
        return True                    # "google how to reset Spotify": the app is what the search is about
    native = [a for a in app_mentions(goal, apps)
              if a.casefold() not in BROWSER_APPS and a.casefold() not in EVERYDAY_WORD_APPS]
    return not native or bool(IN_A_BROWSER.search(goal))


def navigation_destination(value: Any) -> str:
    """A complete observed http(s) origin/path suitable for a planner criterion.

    Query, fragment and credential-bearing links may identify different actions,
    so omitting those components would invent an ambiguous destination.
    """
    if not isinstance(value, str) or not value or any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in value):
        return ""
    if "?" in value or "#" in value:
        return ""
    try:
        parsed = urlsplit(value)
        scheme, host, port = parsed.scheme.lower(), parsed.hostname, parsed.port
        if scheme not in ("http", "https") or not host or parsed.username is not None or parsed.password is not None:
            return ""
        if port is not None and port < 1:
            return ""
        authority = f"[{host}]" if ":" in host else host
        if port is not None and port != (443 if scheme == "https" else 80):
            authority += f":{port}"
        return urlunsplit((scheme, authority, parsed.path or "/", "", ""))
    except ValueError:
        return ""


def observed_site(value: Any) -> str:
    """Host-only page context, including sites whose full URL is ambiguous."""
    if not isinstance(value, str) or any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in value):
        return ""
    try:
        parsed = urlsplit(value)
        if parsed.scheme.lower() not in ("http", "https") or parsed.username is not None or parsed.password is not None:
            return ""
        return parsed.hostname or ""
    except ValueError:
        return ""


# The fields a vault item fills (vault.py; plan_contract's `fill` step), found by code from what a field IS
# (autocomplete, type) and what it is CALLED (label, name, id, placeholder), never from what is in it. Each is
# tagged data-eve-fill=<n> and returned as [n, part, tag]; the lane types the values with Playwright. Jev does
# not pick these fields: a password field is secure and never in its pool (fast_lane.editable_pool), and an
# unlabelled login form, the common case, has nothing for it to read.
#   login  the password field (current-password before new-password), and the username: autocomplete
#          username/email, else the last text field before the password in its form; on a username-only page
#          (a two-step sign-in), the one field that looks like it.
#   card   cc-number, cc-exp (or cc-exp-month and cc-exp-year, selects included), cc-csc, cc-name; a field
#          with no cc-* hint by its label.
#   totp   autocomplete one-time-code, else a field called a code; a row of 4-8 one-character boxes, one digit
#          each; else the focused field, else the page's only text field.
_FILL_FIELDS_JS = r"""(kind) => {
  for (const e of document.querySelectorAll('[data-eve-fill]')) e.removeAttribute('data-eve-fill');
  const vis = (e) => { const r = e.getBoundingClientRect(); const cs = getComputedStyle(e);
    return r.width >= 2 && r.height >= 2 && cs.visibility !== 'hidden' && cs.display !== 'none' && !e.disabled && !e.readOnly; };
  const tokens = (e) => (e.getAttribute('autocomplete') || '').toLowerCase().split(/\s+/);
  const has = (e, t) => tokens(e).includes(t);
  const words = (e) => { const parts = [e.getAttribute('aria-label'), e.getAttribute('name'), e.id, e.getAttribute('placeholder'), e.title];
    if (e.labels) for (const l of e.labels) parts.push(l.innerText);
    return parts.filter(Boolean).join(' ').toLowerCase(); };
  const skip = ['hidden', 'submit', 'button', 'reset', 'checkbox', 'radio', 'file', 'image', 'range', 'color', 'search'];
  const fields = Array.from(document.querySelectorAll('input,select,textarea')).filter(e => !skip.includes((e.type || '').toLowerCase()) && vis(e));
  const textish = (e) => e.tagName === 'INPUT' && ['text', 'email', 'tel', 'number', 'url'].includes((e.type || 'text').toLowerCase());
  const out = [];
  const mark = (e, part) => { if (!e || out.some(o => o[1] === part) || e.hasAttribute('data-eve-fill')) return;
    e.setAttribute('data-eve-fill', String(out.length)); out.push([String(out.length), part, e.tagName.toLowerCase()]); };
  if (kind === 'login') {
    const pwds = fields.filter(e => (e.type || '').toLowerCase() === 'password' || has(e, 'current-password'));
    const pw = pwds.find(e => has(e, 'current-password')) || pwds.find(e => !has(e, 'new-password')) || pwds[0];
    const inScope = (e) => !pw || !pw.form || e.form === pw.form;
    const named = (e) => has(e, 'username') || has(e, 'email') || (e.type || '').toLowerCase() === 'email';
    let user = fields.find(e => e !== pw && textish(e) && inScope(e) && named(e));
    if (!user && pw) { const before = fields.filter(e => e !== pw && textish(e) && inScope(e) && (e.compareDocumentPosition(pw) & Node.DOCUMENT_POSITION_FOLLOWING));
      user = before[before.length - 1]; }
    if (!user && !pw) { const looks = fields.filter(e => textish(e) && (named(e) || /user|e-?mail|login|account|phone/.test(words(e))));
      user = looks.length === 1 ? looks[0] : (fields.filter(textish).length === 1 ? fields.filter(textish)[0] : null); }
    mark(user, 'username'); mark(pw, 'password');
  } else if (kind === 'card') {
    const hinted = (e) => tokens(e).some(a => a.startsWith('cc-'));
    const by = (tok, rx) => fields.find(e => has(e, tok)) || fields.find(e => !hinted(e) && rx.test(words(e)));
    mark(by('cc-number', /card.?num|cc.?num|card no|number on card|\bpan\b/), 'number');
    const month = fields.find(e => has(e, 'cc-exp-month')) || fields.find(e => !hinted(e) && /exp.*(month|\bmm\b)/.test(words(e)));
    const year = fields.find(e => has(e, 'cc-exp-year')) || fields.find(e => !hinted(e) && /exp.*(year|\byy)/.test(words(e)));
    if (month && year && month !== year) { mark(month, 'exp_month'); mark(year, 'exp_year'); }
    else mark(fields.find(e => has(e, 'cc-exp')) || fields.find(e => !hinted(e) && e.tagName === 'INPUT' &&
      /expir|exp.?date|mm.?\/.?yy|valid.?thru/.test(words(e))), 'exp');
    mark(by('cc-csc', /cvc|cvv|csc|security.?code|card.?code|verification.?(code|number)/), 'cvc');
    mark(by('cc-name', /name.?on.?card|card.?holder|cardholder/), 'name');
  } else if (kind === 'totp') {
    const otp = fields.find(e => has(e, 'one-time-code') && !(e.maxLength === 1)) ||
      fields.find(e => textish(e) && e.maxLength !== 1 && /otp|one.?time|2fa|two.?factor|verification|auth.*code|security.?code|passcode|\bcode\b|token/.test(words(e)));
    const singles = fields.filter(e => textish(e) && e.maxLength === 1);
    if (otp) mark(otp, 'code');
    else if (singles.length >= 4 && singles.length <= 8) singles.forEach((e, i) => { e.setAttribute('data-eve-fill', String(out.length)); out.push([String(out.length), 'code#' + i, 'input']); });
    else { const a = document.activeElement; const t = fields.filter(textish);
      mark(a && t.includes(a) ? a : (t.length === 1 ? t[0] : null), 'code'); }
  }
  return out; }"""

# What a screenshot paints over: every secret field (as the collector decides it) and every field the vault
# filled. Playwright's own mask, so the pixels never leave the browser unmasked.
SECRET_FIELDS = ('input[type=password i], [autocomplete*="cc-" i], [autocomplete*="one-time-code" i], '
                 '[autocomplete*="current-password" i], [autocomplete*="new-password" i], [data-eve-filled]')
MASK_COLOR = "#000000"


def snapshot_from_page(raw: Mapping[str, Any], seq: int, secs: float = 0.0) -> Snapshot:
    """The page's collected elements as the Snapshot the gate and Jev already read."""
    elements: list[Candidate] = []
    focused: Optional[Candidate] = None
    destinations: list[str] = []
    for e in raw.get("elements") or []:
        role = ROLE_WORDS.get(str(e.get("role") or ""), str(e.get("role") or "control"))
        c = Candidate(id=str(e["id"]), role=role, label=str(e.get("name") or ""), value=str(e.get("value") or ""),
                      frame=(int(e.get("x", 0)), int(e.get("y", 0)), int(e.get("w", 0)), int(e.get("h", 0))),
                      # AXPress so every role is pressable to the gate; in_content False because on the Mac
                      # lane that flag means "web content the AX walk cannot click reliably" — here the page
                      # is the whole app and every control is a first-class one.
                      actions=("AXPress",), enabled=bool(e.get("enabled", True)), app="browser",
                      in_content=False, in_dialog=bool(e.get("dialog")), secure=bool(e.get("secure")),
                      editable=bool(e.get("editable")))
        elements.append(c)
        destination = navigation_destination(e.get("href"))
        if c.role == "link" and c.label and not e.get("offscreen", False) and destination:
            destinations.append("link destination: " + json.dumps({"id": c.id, "url": destination}))
        if e.get("focused"):
            focused = c
    elements.sort(key=lambda c: (c.frame[1] // 8, c.frame[0]))
    title = str(raw.get("title") or "")
    context = tuple(x for x in (f"title: {title}" if title else "", f"url: {str(raw.get('url') or '')[:120]}",
                               *destinations) if x)
    return Snapshot(seq=seq, app="browser", bundle="eve.browser", title=title, pid=1, elements=tuple(elements),
                    context_lines=context, focused=focused, dialog_text=str(raw.get("dialog") or ""),
                    node_count=len(elements), truncated=len(elements) >= MAX_CANDIDATES, secs=secs)


def outline_lines(snap: Snapshot, limit: int = 80) -> list[str]:
    """Roles, labels, control states and unambiguous observed link destinations."""
    lines: list[str] = []
    destinations: dict[str, str] = {}
    for line in snap.context_lines:
        if line.startswith("link destination: "):
            try:
                item = json.loads(line[len("link destination: "):])
                destination = navigation_destination(item.get("url"))
                if destination:
                    destinations[str(item["id"])] = destination
            except (ValueError, KeyError, TypeError, AttributeError):
                continue
    for c in snap.elements:
        if not c.label or c.secure:
            continue
        state = f", {c.value}" if c.value in ("selected", "checked", "unchecked", "expanded", "collapsed") else ""
        destination = f"; destination: {destinations[c.id]}" if c.role == "link" and c.id in destinations else ""
        lines.append(f"{c.role}: {c.label[:60]}{state}{destination}")
        if len(lines) >= limit:
            break
    return lines


class _Page:
    """Senses and effectors over one Playwright page, on the lane's thread. The methods and their return
    strings are the contract desktop_helpers' adapter keeps, because run_plan and run_delegate read them."""

    def __init__(self, page: Any, clock: Callable[[], float] = time.perf_counter, vault: Any = None,
                 webmcp_probe: bool = False) -> None:
        self.page = page
        self.clock = clock
        self.vault = vault                        # vault.Vault, or None: the fill effector refuses
        self.approve: Optional[str] = None
        self._seq = 0
        self._snapshot: Optional[Snapshot] = None
        self._observed_url = ""
        self._hash_before = ""
        self._hash0 = ""
        self._recorded: dict[str, Any] = {}       # the last snapshot's document and controls (click_recheck)
        self.blocked = ""                         # the owner line while the last snapshot is a bot check, else ""
        self.webmcp_probe = webmcp_probe
        self._webmcp_seen = ""

    # -- senses --
    def _read_dom(self, read: Callable[[], Any]) -> Any:
        """Read the current document, retrying only the navigation context race.

        Chromium can replace the execution context between a click and evaluate.
        Waiting for the next context is observation, never another input or a
        cached answer. Other failures retain their existing error handling.
        """
        deadline = self.clock() + SNAPSHOT_TIMEOUT_MS / 1000
        for attempt in range(SNAPSHOT_TIMEOUT_MS // NAV_READ_POLL_MS + 1):
            try:
                return read()
            except Exception as e:  # noqa: BLE001 — only known navigation errors are retried
                detail = str(e).casefold()
                navigation = ("execution context was destroyed" in detail
                              or "cannot find context with specified id" in detail)
                if (not navigation or self.clock() >= deadline
                        or attempt == SNAPSHOT_TIMEOUT_MS // NAV_READ_POLL_MS):
                    raise
                self.page.wait_for_timeout(NAV_READ_POLL_MS)
        raise RuntimeError("document observation retry limit")  # defensive: loop always returns or raises

    def _collect(self) -> dict[str, Any]:
        raw = self._read_dom(lambda: self.page.evaluate(_COLLECT_JS))
        return raw if isinstance(raw, dict) else {"elements": []}

    def snapshot(self) -> Snapshot:
        self._seq += 1
        self._snapshot = None  # a failed read must not leave focused() using the previous document
        self._observed_url = ""
        self._recorded = {}
        t0 = self.clock()
        raw = self._collect()
        snap = snapshot_from_page(raw, self._seq, self.clock() - t0)
        self._snapshot = snap
        self._observed_url = raw.get("url") if isinstance(raw.get("url"), str) else ""
        # What a decision made on this snapshot is made on: each control as collected, in this document.
        self._recorded = {"url": self._observed_url, "doc": str(raw.get("document_id") or ""),
                          "controls": {str(e.get("id")): (str(e.get("role") or ""), str(e.get("name") or ""),
                                                          str(e.get("href") or ""))
                                       for e in raw.get("elements") or [] if isinstance(e, Mapping)}}
        head = raw.get("head")
        if not isinstance(head, str):
            head = str(raw.get("text_hash") or "").partition(":")[2]
        self.blocked = (blocked_line(self._observed_url)
                        if browser_blocker(self._observed_url, str(raw.get("title") or ""), head) else "")
        # What "changed" means here: the URL, the title, the page text, or any control's state or label.
        # A toggled checkbox changes no text, so the controls are in the hash too.
        controls = [(e.get("role"), e.get("name"), e.get("value")) for e in raw.get("elements") or []]
        digest = hashlib.sha1(json.dumps([raw.get("url"), raw.get("title"), raw.get("text_hash"), controls]).encode())
        self._hash_before, self._hash0 = self._hash0, digest.hexdigest()
        if self.webmcp_probe:
            self._probe_webmcp(self._observed_url)
        return snap

    def _probe_webmcp(self, url: str) -> None:
        """Log, once per page, whether it offers WebMCP tools. Only the host and the tool names are logged,
        never the path or query. A failed probe is silent: it must never cost a step."""
        parts = urlsplit(url)
        if parts.scheme not in ("http", "https") or not parts.hostname:
            return
        page_key = f"{parts.scheme}://{parts.netloc}{parts.path}"
        if page_key == self._webmcp_seen:
            return
        self._webmcp_seen = page_key
        try:
            found = self.page.evaluate(_WEBMCP_JS)
        except Exception:  # noqa: BLE001 — measurement only
            return
        if not isinstance(found, dict):
            return
        names = found.get("names")
        if isinstance(names, list):
            names = [re.sub(r"[^A-Za-z0-9_.-]", "", str(n))[:64] for n in names]
        log.info("browser lane: webmcp probe host=%s api=%s tools=%s names=%s", parts.hostname,
                 bool(found.get("api")), len(names) if isinstance(names, list) else "-",
                 ",".join(names) if names else "-")

    def current_url(self) -> str:
        """Full URL from the last successful snapshot; never perform another read."""
        return self._observed_url

    def text_visible(self, s: str) -> bool:
        needle = " ".join(str(s).split()).casefold()
        if not needle:
            return False
        try:
            # The page text, the title, and what is typed into its fields (innerText has no field values).
            def read() -> bool:
                body = self.page.evaluate("() => (document.body ? document.body.innerText : '') + ' ' + "
                                          "Array.from(document.querySelectorAll('input,textarea')).map(e => e.value || '').join(' ')") or ""
                if needle in " ".join(str(body).split()).casefold():
                    return True
                return needle in " ".join(str(self.page.title()).split()).casefold()

            return bool(self._read_dom(read))
        except Exception:  # noqa: BLE001 — a page mid-navigation has no text yet
            return False

    def focused(self) -> Optional[Candidate]:
        snap = self._snapshot if self._snapshot is not None else self.snapshot()
        return snap.focused

    def frontmost_pid(self) -> int:
        return 1                                           # one page; the executor's focus check never fires

    def screen_changed(self) -> Optional[bool]:
        if not self._hash_before or not self._hash0:
            return None
        return self._hash_before != self._hash0

    # -- effectors --
    def _locator(self, c: Candidate) -> Any:
        return self.page.locator(f'[data-eve-id="{c.id}"]').first

    def _blocked(self) -> str:
        """The effectors' refusal on a bot-check page (browser_blocker), "" otherwise."""
        return f"{BLOCKED_PREFIX} {self.blocked}" if self.blocked else ""

    def _recheck(self, c: Candidate) -> str:
        """"" when the control under ``c.id`` is still the one the last snapshot recorded, in the same document
        (click_recheck); else the STALE_PREFIX refusal, saying what changed."""
        before = self._recorded.get("controls", {}).get(c.id)
        if before is None:
            return f"{STALE_PREFIX} {c.label!r} was not in the last look at the page; look again"
        recorded = {"url": self._recorded.get("url", ""), "doc": self._recorded.get("doc", ""),
                    "role": before[0], "name": before[1], "href": before[2]}
        try:
            now = self.page.evaluate(_RECHECK_JS, {"id": c.id})
        except Exception as e:  # noqa: BLE001 — a document being replaced cannot vouch for the control
            return f"{STALE_PREFIX} could not re-read {c.label!r} ({type(e).__name__}); look again"
        changed = click_recheck(recorded, now)
        if changed:
            return f"{STALE_PREFIX} {c.label!r} changed since the decision ({', '.join(changed)}); look again"
        return ""

    def click_candidate(self, c: Candidate) -> str:
        if self.blocked:
            return self._blocked()
        norm = " ".join(c.label.split()).casefold()
        # The raw href (query and all: HN's vote?id=…) from the last look; an upvote or follow link asks too.
        href = (self._recorded.get("controls", {}).get(c.id) or ("", "", ""))[2]
        if needs_confirmation(c, href) and not (self.approve and self.approve.casefold() == norm):  # "" is no yes
            return f"refused: {c.label!r} is a sensitive control without approval"
        loc = self._locator(c)
        try:
            if loc.count() == 0:
                return f"refused: {c.label!r} is no longer on the page"
            loc.scroll_into_view_if_needed(timeout=SNAPSHOT_TIMEOUT_MS)
            # Read again what the decision was made on, after the scroll (which can lazy-load and re-render)
            # and as the last thing before the press. A separate evaluate, not a click inside it: Playwright's
            # click is a real pointer event and waits for the control to be visible, stable and uncovered.
            stale = self._recheck(c)
            if stale:
                log.info("browser lane: %s", stale)
                return stale
            loc.click(timeout=SNAPSHOT_TIMEOUT_MS)
        except Exception as e:  # noqa: BLE001 — covered, detached, navigated away: nothing was clicked
            return f"refused: could not click {c.label!r} ({type(e).__name__})"
        x, y, w, h = c.frame
        return f"clicked {c.role} {c.label!r} at ({x + w // 2},{y + h // 2})"

    def focus_and_type(self, c: Candidate, text: str) -> str:
        if self.blocked:
            return self._blocked()
        if c.secure:
            return f"refused: {c.label!r} is a secure field"
        loc = self._locator(c)
        try:
            if loc.count() == 0:
                return f"refused: {c.label!r} is no longer on the page"
            loc.scroll_into_view_if_needed(timeout=SNAPSHOT_TIMEOUT_MS)      # a field below the fold
            loc.click(timeout=SNAPSHOT_TIMEOUT_MS)
            focused = self.page.evaluate("() => document.activeElement && document.activeElement.getAttribute('data-eve-id')")
            if str(focused) != c.id:
                return f"refused: focus is not on the {c.role} {c.label!r}"
            self.page.keyboard.type(text, delay=5)
        except Exception as e:  # noqa: BLE001
            return f"refused: could not type into {c.label!r} ({type(e).__name__})"
        return f"typed {len(text)} characters into {c.role} {c.label!r}"

    def fill_from_vault(self, handle: str) -> str:
        """Fill the page's form from a vault item (vault.py): code finds the fields (_FILL_FIELDS_JS) and types
        the values; the result names the parts filled, never a value. A login or 2FA item fills only on its own
        site (vault.Vault.fill_values). "refused: ..." means nothing was typed."""
        from .vault import VaultError
        if self.blocked:
            return self._blocked()
        if self.vault is None:
            return "refused: there is no vault"
        try:
            item, values = self.vault.fill_values(handle, self.page.url)
        except VaultError as e:
            return f"refused: {e}"
        try:
            found = self._read_dom(lambda: self.page.evaluate(_FILL_FIELDS_JS, item.kind))
        except Exception as e:  # noqa: BLE001
            return f"refused: could not read the form ({type(e).__name__})"
        todo = [(str(fid), str(part), str(tag)) for fid, part, tag in found or []
                if str(part).partition("#")[0] in values]
        if not todo:
            return f"refused: no field on this page for the {item.kind} {item.label!r}"
        filled: list[str] = []
        try:
            for fid, part, tag in todo:
                base, _, index = part.partition("#")
                value = values[base] if not index else values[base][int(index):int(index) + 1]
                loc = self.page.locator(f'[data-eve-fill="{fid}"]').first
                if tag == "select":
                    choices = list(dict.fromkeys((value, value.lstrip("0"), value[-2:])))
                    if not any(self._select(loc, c) for c in choices):
                        raise ValueError(f"no {base} option")
                else:
                    loc.fill(value, timeout=SNAPSHOT_TIMEOUT_MS)
                loc.evaluate("e => e.setAttribute('data-eve-filled', '')")
                if base not in filled:
                    filled.append(base)
        except Exception as e:  # noqa: BLE001 — a field that would not take the value
            what = f"could not fill the {part.partition('#')[0]} ({type(e).__name__})"
            return (f"filled {', '.join(filled)} from {item.label!r} but {what}" if filled
                    else f"refused: {what}")
        finally:
            values.clear()
        empty = self.page.evaluate("ids => ids.filter(i => { const e = document.querySelector(`[data-eve-fill=\"${i}\"]`);"
                                   " return !e || !e.value; }).length", [fid for fid, _, _ in todo])
        if empty:
            return f"filled {', '.join(filled)} from {item.label!r} but {empty} field(s) stayed empty"
        return f"filled {', '.join(filled)} from {item.label!r}"

    @staticmethod
    def _select(loc: Any, choice: str) -> bool:
        for how in ({"value": choice}, {"label": choice}):
            try:
                loc.select_option(**how, timeout=1000)
                return True
            except Exception:  # noqa: BLE001 — not this spelling
                continue
        return False

    def clear_consent(self) -> str:
        """Dismiss a cookie/consent notice (CONSENT_TEXT) with a refusing or closing button. The line says what
        was pressed, "" when there was no such notice or no safe button. Never raises."""
        try:
            found = self.page.evaluate(_CONSENT_JS, CONSENT_TEXT.pattern)
        except Exception:  # noqa: BLE001 — a page mid-navigation: nothing to dismiss yet
            return ""
        if not isinstance(found, dict):
            return ""
        choice = consent_choice(list(found.get("buttons") or []))
        if choice is None:
            return ""
        try:
            loc = self.page.locator(f'[data-eve-consent="{choice["id"]}"]').first
            loc.click(timeout=SNAPSHOT_TIMEOUT_MS)
        except Exception as e:  # noqa: BLE001
            return f"refused: could not dismiss the cookie notice ({type(e).__name__})"
        self.settle(0.3)
        return f"dismissed the cookie notice with {choice['name']!r}"

    def press(self, key: str) -> str:
        name = KEYS.get(str(key).lower())
        if name is None:
            raise ValueError(f"not a key: {key!r}")
        if self.blocked:
            return self._blocked()
        before = self.page.url
        self.page.keyboard.press(name)
        if name == "Enter":
            # A form submitted with Return navigates a beat after the key, and until the new page commits every
            # load-state wait answers for the OLD page: the executor moved on while the results were still on
            # their way (live probe of the eval's docs search, 2026-09-24). Wait for the URL to change, briefly.
            t0 = self.clock()
            while self.clock() - t0 < ENTER_NAV_SECS:
                self.page.wait_for_timeout(50)
                if self.page.url != before:
                    try:
                        self.page.wait_for_load_state("domcontentloaded", timeout=NAV_TIMEOUT_MS)
                    except Exception:  # noqa: BLE001 — a slow page is still the new page
                        pass
                    break
        return f"pressed {key}"

    def settle(self, secs: float) -> float:
        t0 = self.clock()
        try:
            self.page.wait_for_load_state("domcontentloaded", timeout=int(min(secs, SETTLE_CAP_SECS) * 1000))
            self.page.wait_for_load_state("networkidle", timeout=int(min(secs, SETTLE_CAP_SECS) * 1000))
        except Exception:  # noqa: BLE001 — a page that never goes idle is still settled enough
            pass
        return self.clock() - t0

    def open_url(self, url: str) -> str:
        if not re.match(r"^(https?|file)://", url, re.I):
            url = "https://" + url
        self.page.goto(url, wait_until="domcontentloaded", timeout=NAV_TIMEOUT_MS)
        return f"opened {url}"


class BrowserLane:
    """Owns the browser and its thread. Every public coroutine hands one job to that thread."""

    def __init__(self, config: BrowserLaneConfig, *, step_asker: Any = None,
                 launcher: Optional[Callable[[], Any]] = None, vault: Any = None) -> None:
        from .vault import Vault
        self.config = config
        self.vault = vault if vault is not None else Vault()   # what the `fill` step fills from (vault.py)
        self._step_asker = step_asker
        self._launcher = launcher
        self._pool = concurrent.futures.ThreadPoolExecutor(max_workers=1, thread_name_prefix="browser-lane")
        self._pw: Any = None
        self._context: Any = None
        self._page: Optional[_Page] = None
        self._plain_http: dict[str, str] = {}          # tests only: an https URL the plan contract insists on → a loopback http one
        self._browser: Any = None                         # attach mode: the owner's Chrome, connected over CDP
        self._profile_map: Optional[dict[str, Any]] = None  # attach mode: {account: context}, per connection
        self._fetch_accounts: Callable[[str, str, float], str] = fetch_accounts   # tests swap the HTTP call
        self.plans_started = 0                            # chrome_lane.py: a plan executing here is progress

    # -- the thread --
    async def _run(self, fn: Callable[..., Any], *args: Any) -> Any:
        return await asyncio.get_running_loop().run_in_executor(self._pool, fn, *args)

    def _ensure(self) -> _Page:
        if self._page is not None:
            try:
                self._page.page.title()               # alive?
                return self._page
            except Exception:  # noqa: BLE001 — the owner closed the window: open a fresh one
                self._page = None
        if self._context is None and self.config.attach and self._launcher is None:
            if self.config.steel:
                from . import steel_local

                try:
                    endpoint = steel_local.start_session(steel_local.configured())   # no key, no dialog, no window
                except steel_local.SteelError as e:
                    raise AttachError(str(e)) from None
            elif self.config.own:
                endpoint = start_own_chrome(self.config.own_dir)   # eve's Chrome: no dialog, no window needed
            else:
                endpoint = devtools_endpoint(self.config.chrome_dir, self.config.debug_port)
                if endpoint is None:
                    raise AttachError("Chrome's remote debugging is off (or Chrome is not running): open "
                                      "chrome://inspect/#remote-debugging in Chrome and switch it on")
                ensure_chrome_window()                     # no window: Chrome answers 403 instead of asking
            from playwright.sync_api import sync_playwright

            self._pw = sync_playwright().start()
            try:
                self._browser = self._pw.chromium.connect_over_cdp(endpoint, timeout=CONSENT_TIMEOUT_MS)
            except Exception as e:
                self._pw.stop()
                self._pw = None
                if "403" in str(e):
                    raise AttachError("Chrome refused the connection (403): it was declined, or Chrome had no "
                                      "window to ask in") from None
                raise
            self._context = self._pick_profile(self.config.chrome_profile)   # the owner's profile: their logins
            page = self._context.new_page()                   # eve's own tab; the owner's tabs are never touched
            page.bring_to_front()
            self._page = _Page(page, vault=self.vault, webmcp_probe=True)
            return self._page
        if self._context is None:
            if self._launcher is not None:
                self._context = self._launcher()
            else:
                from playwright.sync_api import sync_playwright

                self._pw = sync_playwright().start()
                self.config.profile.mkdir(parents=True, exist_ok=True)
                w, h = DEFAULT_VIEWPORT
                self._context = self._pw.chromium.launch_persistent_context(
                    str(self.config.profile), headless=self.config.headless, viewport={"width": w, "height": h},
                    args=["--disable-blink-features=AutomationControlled"])
        if self._browser is not None:
            # Attached, and eve's tab is gone (the owner closed it, or Chrome dropped it): a NEW tab of its
            # own. Never pages[0] — in the owner's Chrome that is one of THEIR tabs.
            page = self._context.new_page()
        else:
            pages = list(self._context.pages)
            page = pages[0] if pages else self._context.new_page()
        page.bring_to_front()
        self._page = _Page(page, vault=self.vault, webmcp_probe=True)
        return self._page

    def _profiles(self) -> dict[str, Any]:
        """{primary account email: its Chrome context} for every profile with an open window. Nothing is opened
        on screen. Cached for the connection's life (``_close`` forgets it).

        Every profile is asked at once, within PROFILE_LOOKUP_BUDGET_SECS in total. Playwright's sync objects
        belong to the lane's thread, so each context's cookies for Google's account list are read here (a local
        CDP call), and the requests themselves run on short-lived threads with plain HTTP. A profile that has
        not answered within the budget is simply not addressable by name."""
        if self._profile_map is not None:
            return self._profile_map
        t0 = time.perf_counter()
        contexts = list(self._browser.contexts)
        jobs: list[tuple[Any, str]] = []
        for ctx in contexts:
            try:
                cookies = ctx.cookies([ACCOUNTS_URL])
            except Exception:  # noqa: BLE001 — a profile that cannot say is simply not addressable by name
                continue
            header = "; ".join(f"{c['name']}={c['value']}" for c in cookies or [] if c.get("name"))
            if header:
                jobs.append((ctx, header))
        fetch = self._fetch_accounts
        replies: dict[int, str] = {}
        if jobs:
            pool = concurrent.futures.ThreadPoolExecutor(max_workers=len(jobs), thread_name_prefix="chrome-profiles")
            futures = {pool.submit(fetch, ACCOUNTS_URL, header, PROFILE_LOOKUP_BUDGET_SECS): i
                       for i, (_, header) in enumerate(jobs)}
            done, _ = concurrent.futures.wait(futures, timeout=PROFILE_LOOKUP_BUDGET_SECS)
            pool.shutdown(wait=False, cancel_futures=True)     # a straggler finishes on its own and is dropped
            for f in done:
                try:
                    replies[futures[f]] = f.result()
                except Exception:  # noqa: BLE001
                    pass
        found: dict[str, Any] = {}
        for i, (ctx, _) in enumerate(jobs):
            accounts = accounts_in(replies.get(i, ""))
            if accounts and accounts[0] not in found:
                found[accounts[0]] = ctx
        self._profile_map = found
        log.info("browser lane: Chrome profiles open: %s (%d contexts, %d with Google cookies, %d answered, %.1f s)",
                 ", ".join(found) or "none identified", len(contexts), len(jobs), len(replies),
                 time.perf_counter() - t0)
        return found

    def _pick_profile(self, wanted: str) -> Any:
        if self.config.own or self.config.steel:
            return self._browser.contexts[0]               # eve's Chrome and Steel's have one profile: their own
        profiles = self._profiles() if wanted else {}
        if wanted and wanted not in profiles:
            raise AttachError(f"no open Chrome window for {wanted}: open one in that profile"
                              + (f" (open now: {', '.join(profiles)})" if profiles else ""))
        return profiles.get(wanted) or self._browser.contexts[0]

    def _use_profile(self, request: str) -> str:
        """Point the lane at the profile a request names (or the configured default). Returns the account."""
        p = self._ensure()
        if self._browser is None:
            return ""
        contexts = list(self._browser.contexts)
        if len(contexts) <= 1:
            return ""                                      # one profile: there is nothing to choose, nothing to ask
        profiles = self._profiles()
        email = profile_for(request, profiles, self.config.chrome_profile)
        target = profiles.get(email) or contexts[0]
        if target is not self._context:
            try:
                p.page.close()
            except Exception:  # noqa: BLE001
                pass
            self._context = target
            page = target.new_page()
            page.bring_to_front()
            self._page = _Page(page, vault=self.vault, webmcp_probe=True)
        return email

    def _alive(self) -> bool:
        try:
            return bool(self._browser.is_connected())
        except Exception:  # noqa: BLE001
            return False

    @property
    def connected(self) -> bool:
        return self._context is not None

    async def connect(self, answer_prompt: Optional[Callable[[], Awaitable[Any]]] = None) -> None:
        """Connect now, if not already. In attach mode Chrome asks "Allow remote debugging?" for this
        connection; ``answer_prompt`` (chrome_consent.ConsentBroker.answer_own_connection) runs alongside,
        so the dialog raised by THIS connection is answered with the owner's own yes or no."""
        if self._context is not None:
            # On the lane's own thread: Playwright's sync objects belong to the thread that made them.
            alive = self._browser is None or await self._run(self._alive)
            if alive:
                return
            log.info("browser lane: the Chrome connection is gone (Chrome restarted?); reconnecting")
            await self._run(self._close)                 # then a fresh connection, and a fresh Allow
        if self.config.own or self.config.steel:
            answer_prompt = None                       # eve's own Chrome and Steel never ask: there is nothing to press
        connecting = asyncio.ensure_future(self._run(self._ensure))
        answering = asyncio.ensure_future(answer_prompt()) if answer_prompt is not None else None
        try:
            await connecting
        finally:
            if answering is not None and not answering.done():
                answering.cancel()
                await asyncio.gather(answering, return_exceptions=True)

    async def use_profile(self, request: str) -> str:
        return await self._run(self._use_profile, request)

    def _close(self) -> None:
        if self._browser is not None:
            # Attach mode: close eve's own tab and disconnect. Never the context or the browser — they are
            # the owner's Chrome, with every window and tab they have open.
            try:
                if self._page is not None:
                    self._page.page.close()
            except Exception:  # noqa: BLE001 — the owner may have closed it already
                pass
            try:
                if self._pw is not None:
                    self._pw.stop()
            except Exception:  # noqa: BLE001
                pass
            self._browser = self._context = self._pw = None
            self._page = None
            self._profile_map = None
            if self.config.steel:
                from . import steel_local

                steel_local.release(steel_local.configured())   # Chrome closes and writes eve's sign-ins
            return
        for obj in (self._context, self._pw):
            try:
                if obj is not None:
                    obj.close() if obj is self._context else obj.stop()
            except Exception:  # noqa: BLE001
                pass
        self._context = self._pw = None
        self._page = None

    async def close(self) -> None:
        await self._run(self._close)
        self._pool.shutdown(wait=False)

    # -- what the agent calls --
    def _outline(self) -> dict[str, Any]:
        p = self._ensure()
        cleared = p.clear_consent()                  # the planner reads the page, not the notice over it
        if cleared:
            log.info("browser lane: %s", cleared)
        snap = p.snapshot()
        if p.blocked:
            log.info("browser lane: a bot check is up at %s", observed_site(p.current_url()) or "-")
        return {"app": "browser", "lines": outline_lines(snap), "title": snap.title,
                "url": navigation_destination(p.current_url()), "site": observed_site(p.current_url()),
                "vault": self.vault.lines() if self.vault is not None else [],
                "blocked": p.blocked}              # a bot check: the owner line; the caller says it and stops

    async def outline(self) -> dict[str, Any]:
        return await self._run(self._outline)

    def _run_plan(self, plan_dict: dict[str, Any], goal: str, start: int, approved: Mapping[str, str],
                  success_rejected: bool = False) -> dict[str, Any]:
        from . import plan_contract as pc
        from .plan_executor import run_plan

        p = self._ensure()
        plan = pc.parse_plan(plan_dict, goal, source=str(plan_dict.get("source") or "astra"))
        ok = {int(k): v for k, v in (approved or {}).items()}
        p.approve = None                          # the executor grants each approved click its label (_grant)

        def open_app(name: str) -> str:
            if name.casefold() in ("browser", "chrome", "chromium", "google chrome", "safari"):
                return f"opened {name}"                    # the browser is already the whole world here
            raise ValueError(f"{name} is not the browser: this plan belongs on the Mac lane")

        first = plan.steps[start] if start < len(plan.steps) else None
        if first is not None and first.kind != "open_url":
            p.snapshot()                          # the page the plan starts on: a bot check stops it before step 1
            if p.blocked:
                return self._blocked_result(p, start)
        result = run_plan(plan, senses=p, effectors=p, asker=self._step_asker, open_app=open_app,
                          open_url=lambda url: p.open_url(self._plain_http.get(url, url)), frontmost_app=lambda: "browser", start=start, approved=ok,
                          decide="jev" if self._step_asker is not None else "keyword", planned_for="browser",
                          success_rejected=success_rejected)
        if p.blocked and result.status != "complete":
            # A step landed on a bot check (the effectors refused everything after it): not a step that failed,
            # a page nobody here may pass. Blocked, with the owner's line, never handed on to be solved.
            return {**self._blocked_result(p, result.next_index), "ms": round(result.ms, 1),
                    "ledger": [e.to_dict() for e in result.ledger]}
        return result.to_dict()

    @staticmethod
    def _blocked_result(p: _Page, index: int) -> dict[str, Any]:
        """A plan result for a bot-check page: status "blocked", and the sentence is what eve tells the owner."""
        log.info("browser lane: plan blocked by a bot check at %s", observed_site(p.current_url()) or "-")
        return {"status": "blocked", "next_index": index, "reason": "browser_challenge", "confirm": "",
                "sentence": p.blocked, "ms": 0.0, "ledger": []}

    async def run_plan(self, plan_dict: dict[str, Any], goal: str, start: int = 0,
                       approved: Optional[Mapping[str, str]] = None, success_rejected: bool = False) -> dict[str, Any]:
        self.plans_started += 1
        return await self._run(self._run_plan, plan_dict, goal, start, dict(approved or {}), bool(success_rejected))

    def _page_text(self, limit: int = 12000) -> dict[str, Any]:
        """Read once the text stops growing: a web app (Gmail) says "Loading…" for seconds after the load event
        (live, 2026-09-23: 190 characters read too early). Polls every READ_POLL_SECS, up to READ_WAIT_SECS."""
        p = self._ensure()

        def body() -> str:
            try:
                return " ".join(p.page.inner_text("body", timeout=5000).split())
            except Exception:  # noqa: BLE001 — a page with no body text reads as empty, never as a crash
                return ""

        def challenge(text: str) -> str:
            url = str(p.page.url or "")
            return blocked_line(url) if browser_blocker(url, str(p.page.title() or ""), text) else ""

        text, deadline = body(), time.perf_counter() + READ_WAIT_SECS
        blocked = challenge(text)
        if blocked:                                         # a bot check never grows into the answer: no wait
            return {"title": p.page.title(), "url": p.page.url, "text": text[:limit], "signed_out": False,
                    "blocked": blocked}
        while time.perf_counter() < deadline:
            time.sleep(READ_POLL_SECS)
            again = body()
            if len(again) <= len(text) and len(again) >= READ_ENOUGH_CHARS:
                break                                       # stopped growing, and there is something to read
            text = again if len(again) >= len(text) else text
        return {"title": p.page.title(), "url": p.page.url, "text": text[:limit],
                "signed_out": bool(SIGN_IN.search(p.page.url)), "blocked": challenge(text)}

    async def page_text(self, limit: int = 12000) -> dict[str, Any]:
        """The page's title, URL and visible text (whitespace-collapsed, capped): what a question is answered from."""
        return await self._run(self._page_text, limit)

    async def open_url(self, url: str) -> str:
        return await self._run(lambda: self._ensure().open_url(url))

    def _screenshot(self) -> Optional[bytes]:
        """The page itself (the browser viewport, full size), not the desktop. None when not connected: a
        picture never opens a new connection (which would ask the owner for Chrome access)."""
        if self._context is None:
            return None
        try:
            page = self._ensure().page
            return page.screenshot(type="jpeg", quality=85, mask=[page.locator(SECRET_FIELDS)], mask_color=MASK_COLOR)
        except Exception:  # noqa: BLE001
            return None

    async def screenshot(self) -> Optional[bytes]:
        return await self._run(self._screenshot)


def make_step_asker(environ: Any = None) -> Any:
    """The same Jev step asker the desktop worker builds, or None (keyword gate alone)."""
    env = os.environ if environ is None else environ
    if (env.get("EVE_JEV_STEP") or "0").strip().lower() not in ("1", "true", "yes", "on"):
        return None
    from . import jev
    from .typed_ask import make_step_asker as typed_step_asker

    try:
        url, key, model = jev.route_config(env)
    except jev.JevError as e:
        log.info("browser lane: jev is off (%s); the keyword gate decides alone", e)
        return None
    seconds = jev.timeout_from_env(env)
    return typed_step_asker(jev.make_predict(url, key, model, timeout_s=seconds), time.perf_counter, env)
