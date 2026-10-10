"""Test a freshly built app in a phone-sized headless browser before the owner ever sees it.

apps_maker.py hands every app Claude writes to ``check_app``. It runs the page in Chromium (Playwright) at an
iPhone's size with touch, with a stand-in Telegram (``window.Telegram.WebApp``: theme and its CSS variables,
MainButton, BackButton, haptics, popups that answer yes) and a stand-in ``window.eve`` whose storage lives
here in Python, so a page reload or a crash cannot lose it. Then it uses the app the way a person would in the
first minute:

1. Opens it (empty, or with ``seed``: the owner's real data when an existing app is being changed) in Telegram's
   DARK theme, the one a fixed color breaks, and waits for it to settle.
2. View by view: fills every visible field with a value that fits it (the example in its placeholder, a number
   inside its range, what a prefilled field already holds; a select keeps a real choice), taps every button,
   checkbox and tab it can reach, reading the page again whenever a tap re-renders it (at most
   ``MAX_PER_GROUP`` of a row of siblings such as an icon grid, ``MAX_TAPS`` in all). When a view is used up it
   presses the MainButton (the form's submit, or "New …" again), else Telegram's Back button. When a press
   shows an error next to a field, the empty or rejected fields are filled again. Destructive controls
   (delete, reset, clear…) come last in each view; a Cancel or Close after the MainButton had its chance.
   A control under a toast is tried again once the toast has had time to go.
3. Closes the app the way Telegram does, with no warning: a save that only lands after that is lost on a
   phone. Then lets an hour pass on the page's clock with the app open (a running timer finishes).
4. Opens it again in the light theme with the richest data it saved (the most records, so the photo shows
   the app in use; the screenshot), and once more at a small Android phone's width with the data as it was
   left, because an app that renders its own saved data wrongly is the most common way a tracker breaks on
   day two.

What it reports is what a person would hit, each with the action that caused it: uncaught errors and console
errors, a tap that reloaded the whole page, requests to anywhere the app may not reach, a library that did not
load, a page wider than the phone or laid out for a desktop, an app that never saved after being used, a save
that arrives after closing, an app that froze, and on every view it visits: text that does not read in dark
mode, a selected tab that looks like the others, a native date field left in light colors, tap targets too
small for a finger, inputs small enough to make iPhone zoom, a chart shape stretched by a CSS rule, and a
control that stays covered. False alarms cost a repair round and can make a working app worse, so each check
only fires on something a person would really see.

Nothing leaves the Mac except, when an app loads a library, requests to cdn.jsdelivr.net: every HTTP request goes
through the check's router, WebSockets are answered here and closed (an attempt is a finding), and Chromium
starts with a host resolver that knows no other name and with WebRTC kept off the network. The page is served
sandboxed, as the Mini App server serves it (miniapp.APP_CSP), so browser storage throws here as it does there.

That first minute is blind: it proves nothing broke, not that the app does what it is for. So when the maker
wrote journeys for the app (the core flow as steps, and the text it must show afterwards), each one is then
walked by Jev, in a fresh copy of this same sandbox with no data (jev_verify.py, ``_journeys``). A journey that
does not go through is a problem in the same report; Jev being unreachable is "journeys not run", never one.

A check that breaks after the app opened keeps what it found: a renderer crash (an app that allocates without
end) is a finding, and only a checker that could not start at all is a skip.
"""

from __future__ import annotations

import asyncio
import base64
import datetime as _dt
import json
import logging
import re
import time
from dataclasses import dataclass, field
from typing import Any, Optional

from . import jev_verify

log = logging.getLogger(__name__)

_FROM_ENV: Any = object()                       # check_app's verifier default: Jev on this Mac's route, if set

APP_URL = "https://app.eve.test/apps/check/"
APP_HOST = "app.eve.test"
ALLOWED_HOSTS = ("cdn.jsdelivr.net",)          # reached for real: an app's library
STUBBED_HOSTS = ("telegram.org",)               # answered here: the stand-in Telegram replaces its script
PHONE = {"width": 390, "height": 844}           # an iPhone 14/15
SMALL_PHONE = {"width": 360, "height": 740}     # a small Android phone
MAX_TAPS = 40
MAX_FIELDS = 30
MAX_PER_GROUP = 3                               # taps on siblings of one kind (an icon grid, a row of days)
MAX_ROUNDS = 40                                 # a re-rendering tap ends a round; taps and time bound it
MAX_ISSUES = 12
MAX_TRACE = 200
SETTLE_MS = 600
TAP_WAIT_MS = 150
CLICK_TIMEOUT_MS = 1200
LOAD_TIMEOUT_MS = 15000
CHECK_TIMEOUT_S = 60.0                          # an app stuck in a loop cannot hold a build forever
AN_HOUR_MS = 61 * 60 * 1000                     # the clock jump at the end: a pomodoro or countdown finishes
CLOSE_GRACE_MS = 250                            # a save started by the close itself (pagehide) still counts
LATE_MS = 900                                   # how long a save that comes after the close is waited for
COVERED_WAIT_MS = 1000                          # a toast over a control is waited out, this long at a time
COVERED_FOR_S = 3.5                             # longer than any toast: something stays on top of the control
BAR_H = 100                                     # Telegram's bottom bar with the MainButton, in CSS pixels
# How the app is served, as the real server does (miniapp.APP_CSP starts with the same sandbox): an opaque
# origin, so browser storage and cookies throw, and a form cannot navigate the page away.
SANDBOX = "sandbox allow-scripts allow-forms allow-modals allow-popups allow-popups-to-escape-sandbox allow-downloads"
# No name resolves but the library CDN's (an IP address is refused too), and WebRTC never leaves by UDP: an
# app's own code cannot reach the network or the Mac's local services during the check.
CHROMIUM_ARGS = ["--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE cdn.jsdelivr.net",
                 "--force-webrtc-ip-handling-policy=disable_non_proxied_udp"]

# Telegram's two stock themes (themeParams), as the stand-in reports them.
LIGHT = {"bg_color": "#ffffff", "text_color": "#000000", "hint_color": "#8e8e93", "link_color": "#2f6fde",
         "button_color": "#2f6fde", "button_text_color": "#ffffff", "secondary_bg_color": "#f2f2f7",
         "header_bg_color": "#ffffff", "accent_text_color": "#2f6fde", "section_bg_color": "#ffffff",
         "section_header_text_color": "#6d6d72", "subtitle_text_color": "#8e8e93",
         "destructive_text_color": "#ff3b30", "bottom_bar_bg_color": "#f2f2f7",
         "section_separator_color": "#c8c7cc"}
DARK = {"bg_color": "#212121", "text_color": "#ffffff", "hint_color": "#aaaaaa", "link_color": "#8774e1",
        "button_color": "#8774e1", "button_text_color": "#ffffff", "secondary_bg_color": "#0f0f0f",
        "header_bg_color": "#212121", "accent_text_color": "#8774e1", "section_bg_color": "#212121",
        "section_header_text_color": "#aaaaaa", "subtitle_text_color": "#aaaaaa",
        "destructive_text_color": "#ff595a", "bottom_bar_bg_color": "#212121",
        "section_separator_color": "#000000"}

# The stand-ins, installed before any of the app's own code runs. Storage and errors are on the Python side
# (__eveLoad / __eveSave are exposed functions), so a reload keeps them like the real server would. The
# theme comes from window.__checkCfg, set by the init script added just before this one. The insets are those
# of the normal (not fullscreen) mode an app opens in: Telegram's header sits above the page, so nothing is
# inset at the top; the home bar is at the bottom.
STUBS_JS = r"""
(() => {
  const cfg = window.__checkCfg || {};
  const theme = cfg.theme, scheme = cfg.scheme || "light";
  const safe = { top: 0, bottom: 34, left: 0, right: 0 }, content = { top: 0, bottom: 0, left: 0, right: 0 };
  // Telegram's own script sets these on <html>; <html> does not exist yet when this runs.
  const paintVars = () => {
    const s = document.documentElement && document.documentElement.style;
    if (!s) return false;
    for (const [k, v] of Object.entries(theme)) s.setProperty("--tg-theme-" + k.replace(/_/g, "-"), v);
    for (const [k, v] of Object.entries(safe)) s.setProperty("--tg-safe-area-inset-" + k, v + "px");
    for (const [k, v] of Object.entries(content)) s.setProperty("--tg-content-safe-area-inset-" + k, v + "px");
    s.setProperty("--tg-color-scheme", scheme);
    s.setProperty("--tg-viewport-height", innerHeight + "px");
    s.setProperty("--tg-viewport-stable-height", innerHeight + "px");
    return true;
  };
  if (!paintVars()) new MutationObserver((m, o) => { if (paintVars()) o.disconnect(); }).observe(document, { childList: true });
  const button = (name) => {
    const b = { text: "", isVisible: false, isActive: true, isProgressVisible: false, color: theme.button_color,
      textColor: theme.button_text_color, hasShineEffect: false, position: "left", _cbs: [], _name: name,
      setText(t) { this.text = String(t); return this; }, show() { this.isVisible = true; return this; },
      hide() { this.isVisible = false; return this; }, enable() { this.isActive = true; return this; },
      disable() { this.isActive = false; return this; },
      showProgress() { this.isProgressVisible = true; return this; }, hideProgress() { this.isProgressVisible = false; return this; },
      setParams(p) { p = p || {}; if ("text" in p) this.text = String(p.text); if ("is_visible" in p) this.isVisible = !!p.is_visible;
        if ("is_active" in p) this.isActive = !!p.is_active; if ("color" in p) this.color = p.color;
        if ("text_color" in p) this.textColor = p.text_color; if ("position" in p) this.position = p.position; return this; },
      onClick(f) { if (typeof f === "function") this._cbs.push(f); return this; },
      offClick(f) { this._cbs = this._cbs.filter((x) => x !== f); return this; } };
    return b;
  };
  const listeners = {};
  const version = "9.1";
  const newer = (a, b) => { const x = String(a).split(".").map(Number), y = String(b).split(".").map(Number);
    for (let i = 0; i < Math.max(x.length, y.length); i++) { const d = (x[i] || 0) - (y[i] || 0); if (d) return d > 0; } return false; };
  const later = (cb, ...args) => { if (typeof cb === "function") setTimeout(() => cb(...args), 0); };
  const WebApp = {
    initData: "", initDataUnsafe: { user: { id: 1, first_name: "Owner", language_code: "en" } }, version, platform: "ios",
    colorScheme: scheme, themeParams: theme, isExpanded: true, viewportHeight: innerHeight, viewportStableHeight: innerHeight,
    isActive: true, isFullscreen: false, isClosingConfirmationEnabled: false, isVerticalSwipesEnabled: true,
    headerColor: theme.header_bg_color, backgroundColor: theme.bg_color, bottomBarColor: theme.bottom_bar_bg_color,
    safeAreaInset: safe, contentSafeAreaInset: content,
    ready() {}, expand() {}, close() {}, isVersionAtLeast(v) { return !newer(v, version); },
    onEvent(n, f) { (listeners[n] = listeners[n] || []).push(f); },
    offEvent(n, f) { listeners[n] = (listeners[n] || []).filter((x) => x !== f); },
    setHeaderColor() {}, setBackgroundColor() {}, setBottomBarColor() {},
    enableClosingConfirmation() { this.isClosingConfirmationEnabled = true; }, disableClosingConfirmation() { this.isClosingConfirmationEnabled = false; },
    enableVerticalSwipes() { this.isVerticalSwipesEnabled = true; }, disableVerticalSwipes() { this.isVerticalSwipesEnabled = false; },
    requestFullscreen() {}, exitFullscreen() {}, lockOrientation() {}, unlockOrientation() {},
    showAlert(m, cb) { later(cb); }, showConfirm(m, cb) { later(cb, true); },
    showPopup(p, cb) { const bs = (p && p.buttons) || [{ id: "", type: "close" }];
      const b = bs.find((x) => x.type !== "cancel" && x.type !== "close") || bs[0]; later(cb, (b && b.id) || ""); },
    openLink() {}, openTelegramLink() {}, sendData() {}, shareMessage() {}, switchInlineQuery() {}, shareToStory() {},
    readTextFromClipboard(cb) { later(cb, ""); }, requestWriteAccess(cb) { later(cb, true); }, requestContact(cb) { later(cb, false); },
    HapticFeedback: { impactOccurred() { return this; }, notificationOccurred() { return this; }, selectionChanged() { return this; } },
    MainButton: button("MainButton"), SecondaryButton: button("SecondaryButton"),
    BackButton: { isVisible: false, _cbs: [], show() { this.isVisible = true; return this; }, hide() { this.isVisible = false; return this; },
      onClick(f) { if (typeof f === "function") this._cbs.push(f); return this; }, offClick(f) { this._cbs = this._cbs.filter((x) => x !== f); return this; } },
    SettingsButton: { isVisible: false, _cbs: [], show() { this.isVisible = true; return this; }, hide() { this.isVisible = false; return this; },
      onClick(f) { if (typeof f === "function") this._cbs.push(f); return this; }, offClick(f) { this._cbs = this._cbs.filter((x) => x !== f); return this; } },
    CloudStorage: { setItem(k, v, cb) { later(cb, null, true); }, getItem(k, cb) { later(cb, null, ""); }, getItems(k, cb) { later(cb, null, {}); },
      removeItem(k, cb) { later(cb, null, true); }, removeItems(k, cb) { later(cb, null, true); }, getKeys(cb) { later(cb, null, []); } },
  };
  WebApp.BottomButton = WebApp.MainButton;
  window.Telegram = { WebApp };
  window.confirm = () => true; window.alert = () => {}; window.prompt = (m, d) => (d !== undefined && d !== null && d !== "" ? d : "Test");
  window.eve = {
    slug: "check",
    load: async () => { const raw = await window.__eveLoad(); return raw === null ? null : JSON.parse(raw); },
    save: async (value) => {
      const raw = JSON.stringify(value);
      if (raw === undefined) throw new Error("eve.save was given a value that is not JSON");
      if (raw.length > 1000000) throw new Error("eve.save: more than 1 MB");
      await window.__eveSave(raw); return true;
    },
  };
})();
"""

# The visible controls of one kind, in document order, each tagged with data-check-id for the next step. A
# field's label is what a person reads next to it (aria-label, <label for>, a wrapping <label>), never its
# placeholder: the placeholder is returned apart, as the example to type.
CONTROLS_JS = r"""
(kind) => {
  const shown = (el) => { const r = el.getBoundingClientRect(); const s = getComputedStyle(el);
    return r.width > 0 && r.height > 0 && s.visibility !== "hidden" && s.display !== "none" && !el.disabled
      && !el.closest("[hidden],[aria-hidden=true],[inert]"); };
  const sel = kind === "fields"
    ? "input:not([type]),input[type=text],input[type=number],input[type=date],input[type=time],input[type=datetime-local],"
      + "input[type=month],input[type=week],input[type=email],input[type=url],input[type=tel],input[type=search],textarea,select"
    : "button,[role=button],[role=tab],[role=checkbox],[role=switch],[role=radio],[role=menuitem],[role=option],"
      + "input[type=checkbox],input[type=radio],input[type=submit],input[type=button],a[href^='#'],summary,[onclick],label:has(input[type=checkbox],input[type=radio])";
  const squash = (t) => (t || "").trim().replace(/\s+/g, " ").slice(0, 60);
  const labelled = (el) => {
    const aria = el.getAttribute("aria-label"); if (aria && aria.trim()) return aria;
    const by = el.getAttribute("aria-labelledby");
    if (by) { const t = by.split(/\s+/).map((i) => document.getElementById(i)).filter(Boolean).map((e) => e.textContent).join(" ");
      if (t.trim()) return t; }
    if (el.id) { const l = document.querySelector(`label[for="${CSS.escape(el.id)}"]`); if (l && l.textContent.trim()) return l.textContent; }
    const wrap = el.closest("label");
    if (wrap) { const c = wrap.cloneNode(true); c.querySelectorAll("input,select,textarea").forEach((x) => x.remove());
      if (c.textContent.trim()) return c.textContent; }
    return "";
  };
  const seen = new Set();
  return Array.from(document.querySelectorAll(sel)).filter((el) => {
    if (seen.has(el) || !shown(el)) return false;
    // a label that wraps a checkbox is the same control as the checkbox: keep one of them
    if (el.tagName === "INPUT" && el.closest("label") && seen.has(el.closest("label"))) return false;
    seen.add(el); return true;
  }).map((el, i) => {
    const id = kind + "-" + Math.random().toString(36).slice(2, 8) + i;
    el.setAttribute("data-check-id", id);
    const type = (el.getAttribute("type") || "").toLowerCase();
    const isField = kind === "fields";
    const named = isField ? squash(labelled(el)) : "";
    const placeholder = squash(el.getAttribute("placeholder"));
    const chosen = el.tagName === "SELECT" && el.selectedIndex >= 0 ? el.options[el.selectedIndex] : null;
    const label = isField
      ? squash(named || placeholder || (chosen && chosen.textContent) || el.getAttribute("name") || "")
      : squash(el.getAttribute("aria-label") || el.getAttribute("title") || el.textContent || el.value || "");
    // siblings of one kind (an icon grid, a row of days) share a group: a few of them say as much as all
    const par = el.parentElement, cls = (e) => (e && e.getAttribute("class")) || "";
    const group = (par ? par.tagName + "." + cls(par) : "") + ">" + el.tagName + "." + cls(el);
    return { id, tag: el.tagName.toLowerCase(), type, label, group, named, placeholder,
      inputmode: (el.getAttribute("inputmode") || "").toLowerCase(), min: el.getAttribute("min") || "",
      max: el.getAttribute("max") || "", step: el.getAttribute("step") || "",
      value: isField && el.tagName !== "SELECT" ? String(el.value || "") : "",
      options: el.tagName === "SELECT" ? el.options.length : 0,
      chosen: el.tagName === "SELECT" ? el.selectedIndex : -1, chosenValue: chosen ? chosen.value : "",
      rejected: isField && el.tagName !== "SELECT" && (el.value === "" || !el.checkValidity() || el.getAttribute("aria-invalid") === "true"),
      destructive: /\b(delete|remove|reset|clear|erase|wipe|discard)\b|🗑/i.test(label + " " + cls(el)),
      leave: /^(cancel|close|dismiss|not now|back|‹|←|✕|✖|×|x)$/i.test(label),
      primary: type === "submit" || /^(\+|(add|save|create|log|done|check.?in|start|new|record|track|mark)\b)/i.test(label) };
  });
}
"""

# Is the control reachable by a finger: scroll it into view, then ask what is on top at its center. When
# something else is, say what, and whether it is a small thing pinned to the screen (a toast, a bar) rather
# than a sheet or dialog that is meant to cover the page.
REACH_JS = r"""
(id) => {
  const el = document.querySelector(`[data-check-id="${id}"]`);
  if (!el || !el.isConnected) return { state: "gone" };
  el.scrollIntoView({ block: "center", inline: "center" });
  const r = el.getBoundingClientRect();
  if (r.width <= 0 || r.height <= 0) return { state: "hidden" };
  const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
  if (!hit) return { state: "hidden" };
  if (hit === el || el.contains(hit) || hit.contains(el)) return { state: "ok" };
  const lbl = hit.closest("label");
  if (lbl && (lbl.contains(el) || (el.id && lbl.htmlFor === el.id))) return { state: "ok" };
  let pinned = null;
  for (let e = hit; e && e !== document.body; e = e.parentElement) {
    const p = getComputedStyle(e).position; if (p === "fixed" || p === "sticky") { pinned = e; break; } }
  const box = (pinned || hit).getBoundingClientRect();
  return { state: "covered", by: ((pinned || hit).textContent || "").trim().replace(/\s+/g, " ").slice(0, 40),
    small: !!pinned && box.width * box.height < 0.4 * innerWidth * innerHeight };
}
"""

PRESS_JS = r"""
(name) => {
  const b = window.Telegram && window.Telegram.WebApp && window.Telegram.WebApp[name];
  if (!b || !b.isVisible || (b.isActive === false) || !b._cbs.length) return null;
  const text = b.text || name;
  b._cbs.slice().forEach((f) => f());
  return text;
}
"""

# The visible text of the messages an app shows next to a field when what was typed is not accepted.
ERRORS_JS = r"""
() => Array.from(document.querySelectorAll("[role=alert],[aria-live=assertive],[class~=err],[class~=error],"
    + "[class*=-error],[class*=error-],[class*=-err],[class~=invalid]"))
  .filter((e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0 && !e.closest("[hidden]"); })
  .map((e) => (e.textContent || "").trim()).filter(Boolean).join(" | ").slice(0, 200)
"""

# Which view is on screen, by its heading: a MainButton label is pressed a few times per view.
VIEW_JS = r"""
() => { const h = Array.from(document.querySelectorAll("h1,h2,[role=heading]")).find((e) => {
    const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0 && !e.closest("[hidden]"); });
  return h ? h.textContent.trim().slice(0, 40) : ""; }
"""

# Telegram closing the app ("hidden"): the page is hidden, then torn down, with nothing in between. A phone
# waking up with the app still open ("visible").
CLOSE_JS = r"""
(state) => {
  Object.defineProperty(document, "visibilityState", { value: state, configurable: true });
  Object.defineProperty(document, "hidden", { value: state === "hidden", configurable: true });
  document.dispatchEvent(new Event("visibilitychange"));
  if (state === "hidden") window.dispatchEvent(new PageTransitionEvent("pagehide", { persisted: false }));
}
"""

# How the current view looks, checked on every view a check visits. Each finding is a kind and a few
# examples; `dark` adds the checks only a dark theme can fail.
LOOK_JS = r"""
({ dark, pageBg, pencil }) => {
  const out = { tiny: [], font: [], shape: [], contrast: [], selected: [], picker: [], corner: [] };
  const vis = (el) => { const r = el.getBoundingClientRect(); const s = getComputedStyle(el);
    return r.width > 0 && r.height > 0 && s.visibility !== "hidden" && s.display !== "none"
      && !el.closest("[hidden],[aria-hidden=true],[inert]"); };
  const forLabel = (el) => { const l = el.id && document.querySelector(`label[for="${CSS.escape(el.id)}"]`);
    return (l && l.textContent) || (el.closest("label") && el.closest("label").textContent) || ""; };
  const name = (el) => (el.getAttribute("aria-label") || el.getAttribute("title") || el.innerText || el.textContent
    || (el.matches("input,select,textarea") && forLabel(el)) || el.getAttribute("placeholder") || el.id || el.tagName.toLowerCase())
    .trim().replace(/\s+/g, " ").slice(0, 30);
  const rgba = (c) => {
    let m = c && c.match(/^rgba?\(([^)]+)\)/);
    if (m) { const p = m[1].split(/[\s,\/]+/).filter(Boolean).map(Number); return [p[0], p[1], p[2], p.length > 3 ? p[3] : 1]; }
    m = c && c.match(/^color\(srgb ([^)]+)\)/);
    if (m) { const p = m[1].split(/[\s\/]+/).filter(Boolean).map(Number); return [p[0] * 255, p[1] * 255, p[2] * 255, p.length > 3 ? p[3] : 1]; }
    return null;
  };
  const over = (top, under) => { const a = top[3]; return [0, 1, 2].map((i) => top[i] * a + under[i] * (1 - a)).concat(1); };
  const base = rgba(pageBg) || [255, 255, 255, 1];
  // the color behind an element: its own background and its ancestors', stacked; null when an image is in it
  const behind = (el) => {
    const layers = [];
    for (let e = el; e; e = e.parentElement) {
      const s = getComputedStyle(e);
      if (s.backgroundImage && s.backgroundImage !== "none") return null;
      const c = rgba(s.backgroundColor); if (!c) return null;
      if (c[3] > 0) { layers.push(c); if (c[3] >= 0.99) break; }
    }
    return layers.reverse().reduce((under, top) => over(top, under), base);
  };
  const lum = (c) => { const f = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; };
    return 0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2]); };
  const ratio = (a, b) => { const x = lum(a), y = lum(b); return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05); };
  const faded = (el) => { let o = 1; for (let e = el; e; e = e.parentElement) o *= Number(getComputedStyle(e).opacity); return o < 0.95; };

  // a finger needs about 44px: flag only what is under 32px both ways with no wider hit area around it
  for (const el of document.querySelectorAll("button,[role=button],[role=tab],[role=checkbox],[role=radio],a[href],select,summary,input[type=checkbox],input[type=radio]")) {
    if (!vis(el) || (el.matches("input") && el.closest("label")) || el.closest("svg")) continue;
    const r = el.getBoundingClientRect();
    if (r.width >= 32 || r.height >= 32 || r.bottom < 0 || r.top > innerHeight) continue;
    const cx = r.left + r.width / 2, cy = r.top + r.height / 2;
    const hits = [[cx - 20, cy], [cx + 20, cy], [cx, cy - 20], [cx, cy + 20]].filter(([x, y]) => {
      const h = document.elementFromPoint(x, y); return h && (h === el || el.contains(h)); }).length;
    if (hits < 2) out.tiny.push(`"${name(el)}" ${Math.round(r.width)}x${Math.round(r.height)}px`);
  }
  // eve draws its Change pencil over every app's top-right corner (apps_maker.EVE_JS), scrolling with the
  // page: an app's own control there sits under it (on the phone the pencil steps aside, and is then missing)
  const px = document.documentElement.clientWidth - pencil.right - pencil.size;
  for (const el of document.querySelectorAll("button,[role=button],[role=tab],[role=link],a[href],input,select,textarea,summary,[onclick]")) {
    if (!vis(el) || (el.matches("input") && el.closest("label") && el.closest("label") !== el)) continue;
    const r = el.getBoundingClientRect(), left = r.left + scrollX, top = r.top + scrollY;
    const w = Math.min(left + r.width, px + pencil.size) - Math.max(left, px);
    const h = Math.min(top + r.height, pencil.top + pencil.size) - Math.max(top, pencil.top);
    if (w > 8 && h > 8) out.corner.push(`"${name(el)}"`);
  }
  for (const el of document.querySelectorAll("input:not([type=checkbox]):not([type=radio]):not([type=range]):not([type=color]):not([type=hidden]):not([type=button]):not([type=submit]),textarea,select")) {
    if (vis(el) && parseFloat(getComputedStyle(el).fontSize) < 15.5) out.font.push(`"${name(el)}" ${getComputedStyle(el).fontSize}`);
  }
  // CSS width/height on an SVG shape's class beat its attributes: the bar a chart meant is not the bar drawn
  for (const el of document.querySelectorAll("svg rect")) {
    if (!vis(el)) continue;
    let bb; try { bb = el.getBBox(); } catch (e) { continue; }
    for (const [attr, got] of [["width", bb.width], ["height", bb.height]]) {
      const raw = el.getAttribute(attr) || "", want = parseFloat(raw);
      if (want > 0 && !/%/.test(raw) && Math.abs(got - want) > Math.max(2, want * 0.25))
        out.shape.push(`a <rect${el.getAttribute("class") ? ` class="${el.getAttribute("class")}"` : ""}> with ${attr}="${raw}" drawn ${Math.round(got)} ${attr === "width" ? "wide" : "tall"}`);
    }
  }
  if (dark) {
    const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    const done = new Set();
    for (let n = walk.nextNode(); n; n = walk.nextNode()) {
      const text = n.textContent.trim(), el = n.parentElement;
      if (!el || done.has(el) || !/[A-Za-z0-9]/.test(text) || el.closest("svg,script,style,option,noscript") || !vis(el)) continue;
      done.add(el);
      const fg = rgba(getComputedStyle(el).color), bg = behind(el);
      if (!fg || !bg || fg[3] < 0.5 || faded(el)) continue;
      const r = ratio(over(fg, bg), bg);
      if (r < 2.5) out.contrast.push(`"${text.replace(/\s+/g, " ").slice(0, 30)}" ${r.toFixed(1)}:1`);
    }
    // a selected tab or segment must stand out from the one next to it by more than a faint fill
    const picked = (e) => e.getAttribute("aria-selected") === "true" || e.getAttribute("aria-pressed") === "true"
      || (e.getAttribute("aria-checked") === "true" && e.getAttribute("role") !== "switch")
      || (e.hasAttribute("aria-current") && e.getAttribute("aria-current") !== "false")
      || /(^|\s)(active|selected|current|on|is-active|is-selected)(\s|$)/.test(e.getAttribute("class") || "");
    // everything but the fill that could show the state, the element's and its insides' (a check mark, a dot).
    // Not the weight (a bolder label alone is easy to miss) and not a dark shadow (invisible on a dark page).
    const shadow = (v) => (v || "").match(/rgba?\([^)]*\)/g) && v.match(/rgba?\([^)]*\)/g).every((c) => {
      const x = rgba(c); return x && Math.max(x[0], x[1], x[2]) < 80; }) ? "none" : v;
    const own = (e) => { const s = getComputedStyle(e), a = getComputedStyle(e, "::after"), b = getComputedStyle(e, "::before");
      return [s.color, s.borderTopColor, s.borderTopWidth, s.borderBottomColor, s.borderBottomWidth, shadow(s.boxShadow),
        s.outlineStyle, s.textDecorationLine, s.opacity, s.transform, a.content, a.backgroundColor, a.opacity, b.content,
        b.backgroundColor, b.opacity].join("|"); };
    const look = (e) => [own(e)].concat(Array.from(e.querySelectorAll("*")).slice(0, 20).map((c) =>
      own(c) + "|" + getComputedStyle(c).backgroundColor + "|" + getComputedStyle(c).display + "|" + getComputedStyle(c).fill)).join("#");
    for (const el of document.querySelectorAll("button,[role=tab],[role=radio],[role=button],a,li")) {
      if (!picked(el) || !vis(el) || !el.parentElement || el.matches("input")) continue;
      const other = Array.from(el.parentElement.children).find((e) => e !== el && e.tagName === el.tagName && vis(e) && !picked(e));
      if (!other || el.querySelectorAll("*").length !== other.querySelectorAll("*").length || look(el) !== look(other)) continue;
      const a = behind(el), b = behind(other);
      if (a && b && ratio(a, b) < 1.25) out.selected.push(`the selected "${name(el)}" next to "${name(other)}" (${ratio(a, b).toFixed(2)}:1)`);
    }
    for (const el of document.querySelectorAll("input[type=date],input[type=time],input[type=datetime-local],input[type=month],input[type=week]")) {
      const bg = behind(el);        // a field the app keeps light in the dark theme is fine with a light picker
      if (!/dark/.test(getComputedStyle(el).colorScheme || "") && bg && lum(bg) < 0.2) { out.picker.push(`"${name(el)}"`); break; }
    }
  }
  for (const k of Object.keys(out)) out[k] = Array.from(new Set(out[k])).slice(0, 3);
  return out;
}
"""

LOOKS = {
    "tiny": "Tap targets too small for a finger while {action}: {items} (under 32x32px with no wider hit area). "
            "Make them at least 44x44px, with padding or a ::after that widens the hit area.",
    "font": "Inputs with text under 16px while {action}: {items}. iPhone zooms the page in when one is focused; "
            "use font-size: 16px or more on inputs, selects and textareas.",
    "shape": "A chart shape is drawn at another size than its SVG attribute says while {action}: {items}. A CSS "
             "rule (width, height, x, y) on its class overrides the attribute; give SVG shapes their own classes "
             "with no layout rules.",
    "contrast": "In Telegram's dark theme, text does not stand out from its background while {action}: {items} "
                "(contrast under 2.5:1). A color is likely fixed instead of coming from the theme variables.",
    "selected": "In Telegram's dark theme, a selected item looks like the unselected one next to it while "
                "{action}: {items}. Give the selected state a clearly different fill (the button color with the "
                "button text color, or an accent underline) and weight.",
    "picker": "In Telegram's dark theme, the date or time field {items} draws its picker in light colors (a dark "
              "icon on a dark field). Declare :root {{ color-scheme: light dark; }} and set "
              "document.documentElement.style.colorScheme from Telegram's colorScheme.",
    "corner": "eve's Change pencil sits over the top-right corner of every view (a 44x44px button, 6px from the top "
              "and the right edge, scrolling with the page), and it covers {items} while {action}. Keep the top-right "
              "56x56px of every view free of controls: give the header padding-right: 56px.",
}

# Where eve's Change pencil sits (apps_maker.EVE_JS, from the top-right corner of the page, in CSS pixels).
PENCIL = {"top": 6, "right": 6, "size": 44}

# For the photo only: Telegram draws the MainButton natively under the page, so the owner would see it.
BAR_JS = r"""
() => { const w = window.Telegram.WebApp, b = w.MainButton;
  return b.isVisible && b.text ? { text: b.text, color: b.color, textColor: b.textColor, bg: w.themeParams.bottom_bar_bg_color } : null; }
"""

LAYOUT_JS = r"""
() => { const m = document.querySelector('meta[name="viewport"]');
  return { inner: window.innerWidth, wide: document.documentElement.scrollWidth - window.innerWidth,
           meta: /width\s*=\s*device-width/i.test((m && m.getAttribute("content")) || ""),
           text: (document.body && document.body.innerText || "").trim().length }; }
"""


@dataclass
class CheckReport:
    ok: bool = True
    issues: list[str] = field(default_factory=list)
    taps: int = 0
    fields: int = 0
    saves: int = 0
    screenshot: Optional[bytes] = None
    photo_from: str = ""                       # 'the journey "…"' (its last screen) or "the scripted check"
    saved: Any = None                          # what the app had saved when the check ended
    photo_data: Any = None                     # the data the screenshot shows: the richest it saved
    secs: float = 0.0
    skipped: str = ""                          # why no check ran (Playwright missing): not a pass, not a fail
    trace: list[str] = field(default_factory=list)   # every action, in order: for a person reading a check
    journeys: Optional[jev_verify.JourneyRun] = None  # Jev walking the maker's journeys (None: the app has none)

    def summary(self) -> str:
        if self.skipped:
            return f"not tested ({self.skipped})"
        walked = self.journeys.summary() if self.journeys is not None else ""
        return (f"{'passed' if self.ok else f'{len(self.issues)} issue(s)'}: {self.fields} fields filled, "
                f"{self.taps} taps, {self.saves} saves" + (f", {walked}" if walked else ""))

    def made(self) -> str:
        """What the check's use of the app created ("habits 2, habits.done 5"), or "no records": whether it got
        as far as the app's core flow, for a person reading an eval."""
        return data_shape(self.photo_data) or "no records"


def data_shape(value: Any) -> str:
    """What saved data holds, in a few words ("habits 2, habits.done 5"): whether the check got as far as
    making the app's records, for a person reading the summary."""
    if not isinstance(value, dict):
        return ""
    counts: dict[str, int] = {}
    for key, v in value.items():
        if isinstance(v, (list, dict)) and v:
            counts[key] = len(v)
            for item in (v if isinstance(v, list) else v.values()):
                if isinstance(item, dict):
                    for k2, v2 in item.items():
                        if isinstance(v2, (list, dict)) and v2:
                            counts[f"{key}.{k2}"] = counts.get(f"{key}.{k2}", 0) + len(v2)
    return ", ".join(f"{k} {n}" for k, n in list(counts.items())[:6])


def _richness(raw: Optional[str]) -> int:
    """How much is in saved data: its count of values. Adding a record raises it, a delete lowers it."""
    def count(v: Any) -> int:
        if isinstance(v, dict):
            return sum(count(x) for x in v.values()) + 1
        if isinstance(v, list):
            return sum(count(x) for x in v) + 1
        return 1
    try:
        return count(json.loads(raw)) if raw else 0
    except ValueError:
        return 0


class _Session:
    """One app's check: its storage and what went wrong, kept outside the page so a reload cannot lose them."""

    def __init__(self, seed: Any) -> None:
        self.stored: Optional[str] = None if seed is None else json.dumps(seed)
        self.richest: Optional[str] = self.stored
        self.saves = self.loads = self.primary_taps = 0
        self.action = "opening the app"
        self.issues: list[str] = []
        self.trace: list[str] = []
        self.closed = False                    # Telegram closed the app: a save now is lost
        self.late: Optional[str] = None
        self.looked: set[str] = set()          # the kinds of look finding already reported
        self.opened = False                    # a page was opened: a failure from here on is the app's too
        self.crashed = False

    def note(self, text: str) -> None:
        if text not in self.issues:
            self.issues.append(text)

    def step(self, text: str) -> None:
        self.action = text
        if len(self.trace) < MAX_TRACE and (not self.trace or self.trace[-1] != text):
            self.trace.append(text)

    async def load(self) -> Optional[str]:
        self.loads += 1
        return self.stored

    async def save(self, raw: str) -> bool:
        if self.closed:
            self.late = raw
            return True
        self.stored, self.saves = raw, self.saves + 1
        if _richness(raw) >= _richness(self.richest):
            self.richest = raw
        return True

    def data(self) -> Any:
        return None if self.stored is None else json.loads(self.stored)


def _where(stack: str) -> str:
    """The first line of the app's own code in a stack ("line 212"), so a repair can go straight to it."""
    m = re.search(re.escape(APP_HOST) + r"/apps/check/[^:\s]*:(\d+):\d+", stack or "")
    return f" (line {m.group(1)})" if m else ""


# What a person would type, by what the field is for. First match wins, so the specific comes first.
_NUMBERS = [(r"\breps?\b|repetitions", 8), (r"\bsets?\b", 3), (r"weight|\bkg\b|\blbs?\b|pounds", 60),
            (r"minutes?|\bmins?\b", 25), (r"hours?|\bhrs?\b", 2), (r"\bml\b|millilit", 250), (r"pages?", 20),
            (r"steps", 8000), (r"calories|kcal", 450), (r"distance|\bkm\b|miles", 5),
            (r"budget|limit|allowance", 400), (r"amount|price|cost|spent|paid|total|income|salary|[$€£₹]", 24.5),
            (r"rating|stars?|score", 4), (r"goal|target", 8)]
_WORDS = [(r"habit", "Read 10 pages"), (r"exercise|workout|lift|movement", "Bench press"),
          (r"\bbook", "The Hobbit"), (r"author|writer", "Ursula K. Le Guin"),
          (r"task|to-?do|chore|project", "Write the report"),
          (r"item|product|grocer|ingredient|shopping", "Milk"), (r"categor", "Groceries"),
          (r"trip|event|occasion", "Lisbon weekend"), (r"place|city|location|destination|where", "Lisbon"),
          (r"person|people|friend|member|participant|payer|who\b|contact|first name|your name", "Alex")]
_PRIMARY = re.compile(r"^\s*(\+|(add|save|create|log|done|check.?in|start|new|record|track|mark)\b)", re.I)
_NOTE = re.compile(r"note|comment|descr|memo|journal|thought|detail", re.I)
_EXAMPLE = re.compile(r"^\s*(?:e\.?\s?g\.?|for example|for instance|such as|like|try)\s*[:,]?\s*(.+)$", re.I)


def _example(placeholder: str) -> str:
    """The example in a placeholder ("e.g. Stretch for 5 minutes"): the first one, without quotes or dots."""
    m = _EXAMPLE.match(placeholder or "")
    if not m:
        return ""
    first = re.split(r",\s|\s+or\s+|\s[·/|]\s", m.group(1))[0]
    return first.strip().strip("\"'“”‘’").rstrip(".…").strip()[:60]


def _number(ctl: dict[str, Any], words: str, alt: bool) -> str:
    """A number for a numeric field: its current value when it has one, else one that fits its label, kept
    inside min/max and on its step (a type=number field without a step takes whole numbers)."""
    def num(s: str) -> Optional[float]:
        try:
            return float(s)
        except (TypeError, ValueError):
            return None

    current = num(ctl.get("value", ""))
    value = next((float(v) for rx, v in _NUMBERS if re.search(rx, words, re.I)), 12.0)
    if current is not None and not alt:
        value = current
    lo, hi = num(ctl.get("min", "")), num(ctl.get("max", ""))
    step = num(ctl.get("step", "")) or (1.0 if ctl.get("type") == "number" else None)
    if alt and lo is not None:
        value = lo + (step or 1)
    if lo is not None and hi is not None and not lo <= value <= hi:
        value = (lo + hi) / 2
    elif lo is not None and value < lo:
        value = lo
    elif hi is not None and value > hi:
        value = hi
    if step:
        value = (lo or 0) + round((value - (lo or 0)) / step) * step
        if hi is not None and value > hi:
            value -= step
    text = f"{value:.2f}".rstrip("0").rstrip(".")
    return text if text != "-0" else "0"


def _value_for(ctl: dict[str, Any], today: _dt.date, alt: bool = False) -> Optional[str]:
    """What a person would type into this field. ``alt``: a second try after the app rejected the first."""
    t, tag = ctl.get("type", ""), ctl.get("tag", "input")
    named, placeholder = ctl.get("named", ctl.get("label", "")), ctl.get("placeholder", "")
    words = f"{named} {placeholder}".lower()
    by_type = {"date": today.isoformat(), "time": "08:30", "datetime-local": f"{today.isoformat()}T08:30",
               "month": today.isoformat()[:7], "week": f"{today.isocalendar()[0]}-W{today.isocalendar()[1]:02d}",
               "email": "owner@example.com", "url": "https://example.com", "tel": "5550100"}
    if t in by_type:
        return by_type[t]
    if t == "number" or ctl.get("inputmode") in ("numeric", "decimal") or (
            t in ("", "text") and any(re.search(rx, named, re.I) for rx, _ in _NUMBERS)):
        return _number(ctl, words, alt)
    if t == "search" or re.search(r"search|filter", words):
        return ""                                      # typing a search would hide the list it filters
    if ctl.get("value", "").strip() and not alt:
        return ctl["value"]                            # what the app put there (an edit form): kept, retyped
    example = _example(placeholder)
    if example:
        return example
    if tag == "textarea" or _NOTE.search(words):
        return "Felt good today."
    return next((v for rx, v in _WORDS if re.search(rx, words, re.I)), "Morning run")


async def check_app(html: str, *, seed: Any = None, browser: Any = None,
                    journeys: Optional[list[jev_verify.Journey]] = None, verifier: Any = _FROM_ENV) -> CheckReport:
    """Run ``html`` like a person's first minute with it. Never raises; a missing Playwright is a skip.

    ``seed`` is the data the app starts with (an existing app's real data when it is being changed, so a data
    migration is tested too); it is copied, never written back. ``browser`` reuses a running Chromium.

    ``journeys`` (the maker's, jev_verify.py) run after the scripted pass, each on a fresh app with no data, and
    ``verifier`` walks them (default: Jev on this Mac's route, jev_verify.from_env; None: journeys not run). A
    journey that does not go through is a problem in this report like any other; a verifier that is down is
    "journeys not run (why)" and never a problem."""
    try:
        from playwright.async_api import async_playwright
    except ImportError:
        return CheckReport(skipped="Playwright is not installed")
    t0 = time.perf_counter()
    report, sess = CheckReport(), _Session(seed)
    today = _dt.date.today()

    async def go(b: Any) -> None:
        await _run(b, html, sess, report, today, PHONE, dark=True, interact=True)
        after = sess.stored
        sess.stored = sess.richest if sess.richest is not None else after
        report.photo_data = None if sess.stored is None else json.loads(sess.stored)
        report.screenshot = await _run(b, html, sess, report, today, PHONE, dark=False, interact=False,
                                       label="reopening the app with the data it saved", shoot=True)
        sess.stored = after
        await _run(b, html, sess, report, today, SMALL_PHONE, dark=False, interact=False,
                   label="reopening the app on a 360px-wide phone")

    async def walk(b: Any) -> None:
        if not journeys:
            return
        if verifier is _FROM_ENV:
            v, why = jev_verify.from_env()
        else:
            v, why = verifier, "" if verifier is not None else "no verifier"
        report.journeys = await _journeys(b, html, journeys, v, why)

    async def both(b: Any) -> None:
        try:
            await asyncio.wait_for(go(b), CHECK_TIMEOUT_S)
        except asyncio.CancelledError:
            raise                              # the build was stopped: nothing more to walk
        except asyncio.TimeoutError:
            # the app hangs: every journey would wait on it too, for JOURNEY_TIMEOUT_S, and find nothing more
            if journeys:
                report.journeys = jev_verify.JourneyRun(
                    total=len(journeys), not_run="the app stopped responding in the scripted check")
            raise
        except BaseException:
            await walk(b)                      # a scripted pass that broke still leaves the journeys to walk
            raise
        await walk(b)

    try:
        if browser is not None:
            await both(browser)
        else:
            async with async_playwright() as p:
                b = await p.chromium.launch(headless=True, args=CHROMIUM_ARGS)
                try:
                    await both(b)
                finally:
                    await b.close()
    except asyncio.TimeoutError:
        sess.note(f"The app stopped responding while {sess.action}: nothing finished within "
                  f"{CHECK_TIMEOUT_S:.0f} s (an endless loop, or work that never yields).")
    except Exception as e:  # noqa: BLE001 — a checker that breaks is a skip, never a failed build...
        crashed = sess.opened and (sess.crashed or "crash" in str(e).lower())
        if crashed:                            # ...but a page the app crashed is the app's problem
            sess.note(f"The app crashed the page while {sess.action} (the browser tab died: most likely memory "
                      "that grows without end, or a loop that allocates on every turn).")
        elif not (sess.opened and sess.issues):
            log.warning("app-check: the check could not run (%s: %s)", type(e).__name__, str(e)[:200])
            return CheckReport(skipped=f"the check could not run ({type(e).__name__})", secs=time.perf_counter() - t0)
        else:                                  # it broke part way: what it found so far still stands
            log.warning("app-check: the check stopped early (%s: %s)", type(e).__name__, str(e)[:200])
    if sess.crashed or any(i.startswith("The app crashed the page") for i in sess.issues):
        pass                                   # a dead tab saves nothing: the crash is the finding
    elif sess.saves == 0 and sess.loads and report.taps and (report.fields or sess.primary_taps):
        sess.note("Opened with the owner's saved data, the app never called eve.save after being used: most "
                  "likely it could not read that data (its migrate() does not convert this shape), so the owner "
                  "cannot use their data in this version." if seed is not None else
                  "After its fields were filled and its buttons tapped (Add, Done and the like), the app never "
                  "called eve.save, so nothing entered would survive closing it.")
    elif sess.loads == 0 and (report.fields or report.taps):
        sess.note("The app never called eve.load, so it cannot show anything it saved before.")
    report.saves, report.saved = sess.saves, sess.data()
    if report.photo_data is None:
        report.photo_data = report.saved
    report.photo_from = "the scripted check" if report.screenshot else ""
    shot = next((r for r in report.journeys.results if r.passed and r.screenshot), None) \
        if report.journeys is not None else None
    if shot is not None:
        # The owner's picture is the app doing what it is for: the end of a journey that passed (the splitter
        # showing who owes whom), not the scripted pass's reopened screen, which can be a setup card, or look
        # right on an app whose journeys failed.
        report.screenshot, report.photo_data = shot.screenshot, shot.data
        report.photo_from = f'the journey "{shot.name}"'
    report.issues = sess.issues[:MAX_ISSUES]
    if report.journeys is not None:
        # the core purpose first: a journey that fails says more than a look finding, so it is never cut
        walked = report.journeys.issues()
        report.issues = (walked + [i for i in report.issues if i not in walked])[:max(MAX_ISSUES, len(walked))]
    report.ok = not report.issues
    report.trace = sess.trace
    report.secs = time.perf_counter() - t0
    return report


async def _run(browser: Any, html: str, sess: _Session, report: CheckReport, today: _dt.date,
               viewport: dict[str, int], *, dark: bool, interact: bool, label: str = "", shoot: bool = False) -> Any:
    """One open of the app. Shooting: returns the screenshot. Issues go to ``sess``."""
    ctx, page, loaded = await _open(browser, html, sess, viewport, dark=dark, clock=interact, label=label)
    try:
        if not loaded:
            return None
        if interact:
            await _use(page, sess, report, today, DARK if dark else LIGHT)
        sess.step(label or "using the app")
        await _look(page, sess, DARK if dark else LIGHT, dark)
        await _layout(page, sess, viewport)
        if interact:
            await _close(page, sess)
        if shoot:
            return await _photo(page, ctx, viewport)
        return None
    finally:
        await ctx.close()


async def _open(browser: Any, html: str, sess: _Session, viewport: dict[str, int], *, dark: bool, clock: bool,
                label: str = "") -> tuple[Any, Any, bool]:
    """A new phone-sized context with the app open in it, served and fenced exactly as the server does, the
    stand-ins installed and every finding wired to ``sess``: (context, page, whether it loaded). The caller
    closes the context. ``clock``: the page's clock is Playwright's, for the hour jump at the end."""
    theme = DARK if dark else LIGHT
    ctx = await browser.new_context(viewport=viewport, device_scale_factor=2, is_mobile=True, has_touch=True,
                                    locale="en-US", color_scheme="dark" if dark else "light")
    page = await ctx.new_page()
    sess.step(label or "opening the app")
    loads = [0]

    def on_error(exc: Any) -> None:
        msg = str(getattr(exc, "message", "") or exc).splitlines()[0][:300]
        sess.note(f"Uncaught error while {sess.action}: {msg}{_where(getattr(exc, 'stack', '') or '')}")

    def on_console(m: Any) -> None:
        if m.type == "error" and not m.text.startswith("Failed to load resource"):
            sess.note(f"Console error while {sess.action}: {m.text[:300]}")

    def on_load(_: Any = None) -> None:
        loads[0] += 1
        if loads[0] > 1:
            sess.note(f"The whole page reloaded while {sess.action} (a <form> submitted without "
                      "event.preventDefault(), or a link to another page), which throws away what is on screen.")

    def on_crash(_: Any = None) -> None:
        sess.crashed = True

    async def on_socket(ws: Any) -> None:
        sess.note(f"The app tried to open a WebSocket to {str(ws.url)[:80]} while {sess.action}; it may not "
                  "reach the network, and must keep its data with eve.save.")
        await ws.close()

    def on_response(r: Any) -> None:
        if r.status >= 400 and r.url.split("/")[2:3] == [ALLOWED_HOSTS[0]]:
            sess.note(f"A library did not load (HTTP {r.status}): {r.url[:160]}")

    async def route(r: Any) -> None:
        url = r.request.url
        host = url.split("/")[2] if url.startswith(("http://", "https://")) else ""
        if url.split("#")[0].split("?")[0] == APP_URL:
            await r.fulfill(status=200, content_type="text/html; charset=utf-8", body=html,
                            headers={"Content-Security-Policy": SANDBOX})
        elif host.endswith(STUBBED_HOSTS) or (host == APP_HOST and url.endswith("/eve.js")):
            await r.fulfill(status=200, content_type="application/javascript", body="")
        elif host == APP_HOST:
            await r.fulfill(status=404, body="")          # favicon and the like: not the app's fault
        elif host in ALLOWED_HOSTS:
            await r.continue_()
        else:
            sess.note(f"The app tried to reach {host or url[:60]} while {sess.action}; it may only load "
                      "libraries from cdn.jsdelivr.net and must keep its data with eve.save.")
            await r.abort()

    try:
        page.on("pageerror", on_error)
        page.on("console", on_console)
        page.on("load", on_load)
        page.on("response", on_response)
        page.on("crash", on_crash)
        if hasattr(ctx, "route_web_socket"):
            await ctx.route_web_socket("**/*", on_socket)
        await page.expose_function("__eveLoad", sess.load)
        await page.expose_function("__eveSave", sess.save)
        await page.route("**/*", route)
        await page.add_init_script(f"window.__checkCfg = {json.dumps({'theme': theme, 'scheme': 'dark' if dark else 'light'})};")
        await page.add_init_script(STUBS_JS)
        if clock:
            try:
                await page.clock.install()             # runs in real time until the hour jump at the end
            except Exception:  # noqa: BLE001 — no clock control: the hour is skipped, the rest runs
                pass
        sess.opened = True
        try:
            await page.goto(APP_URL, wait_until="load", timeout=LOAD_TIMEOUT_MS)
        except Exception as e:  # noqa: BLE001 — a page that never loads is the app's problem, and a finding
            if "Timeout" not in type(e).__name__:
                raise
            sess.note(f"The app did not finish loading within {LOAD_TIMEOUT_MS // 1000} s ({label or 'first open'}).")
            return ctx, page, False
        await page.wait_for_timeout(SETTLE_MS)
        return ctx, page, True
    except BaseException:
        await ctx.close()
        raise


async def _journeys(browser: Any, html: str, journeys: list[jev_verify.Journey],
                    verifier: Optional[jev_verify.Verifier], why: str = "") -> jev_verify.JourneyRun:
    """Walk every journey side by side, each in its own fresh context with no saved data. Never raises.

    A page that breaks under one journey (a crashed tab, a reload that destroyed its context, a page that stops
    answering) is that journey's failure: the app's, like the scripted pass says of a crash. Jev down, the
    time limit or a checker fault stops the journeys still running; those are "not run (why)", because a
    verifier that cannot answer must never block a build, and the journeys that already finished keep their
    result: a failure seen before Jev went down is still a problem."""
    from playwright.async_api import Error as PlaywrightError

    run = jev_verify.JourneyRun(total=len(journeys))
    if verifier is None:
        run.not_run = why or "no verifier"
        log.info("app-check: journeys not run (%s)", run.not_run)
        return run
    t0 = time.perf_counter()

    async def one(j: jev_verify.Journey) -> jev_verify.JourneyResult:
        sess = _Session(None)
        at = [0]

        def on_step(text: str) -> None:
            at[0] += 1
            sess.step(text)

        ctx, page, loaded = await _open(browser, html, sess, PHONE, dark=False, clock=False,
                                        label=f'opening the app for the journey "{j.name}"')
        try:
            if not loaded:
                return jev_verify.JourneyResult(j.name, "step", steps=len(j.steps), step=1,
                                                reason="the app did not finish loading.", evidence="(nothing)")
            try:
                res = await jev_verify.walk(page, j, verifier, errors=sess.issues, on_step=on_step)
            except PlaywrightError as e:
                first = (str(e).splitlines() or [""])[0][:160]
                return jev_verify.JourneyResult(
                    j.name, "step", steps=len(j.steps), step=max(1, at[0]), evidence="(the page broke)",
                    step_words=j.steps[max(1, at[0]) - 1].words(),
                    reason=f"the page stopped working during step {max(1, at[0])} ({type(e).__name__}: {first}).")
            if res.passed:
                try:
                    res.screenshot, res.data = await _photo(page, ctx, PHONE), sess.data()
                except Exception as e:  # noqa: BLE001 — no picture from this journey; the result stands
                    log.info("app-check: no picture from the journey %r (%s)", j.name, type(e).__name__)
            return res
        finally:
            await ctx.close()

    tasks = {asyncio.ensure_future(one(j)): n for n, j in enumerate(journeys)}
    finished: dict[int, jev_verify.JourneyResult] = {}
    loop = asyncio.get_running_loop()
    deadline = loop.time() + jev_verify.JOURNEY_TIMEOUT_S
    try:
        waiting = set(tasks)
        while waiting and not run.not_run:
            left = deadline - loop.time()
            if left <= 0:
                run.not_run = f"the journeys took longer than {jev_verify.JOURNEY_TIMEOUT_S:.0f} s"
                break
            done, waiting = await asyncio.wait(waiting, timeout=left, return_when=asyncio.FIRST_EXCEPTION)
            for task in done:
                if task.cancelled():
                    continue
                e = task.exception()
                if e is None:
                    finished[tasks[task]] = task.result()
                elif isinstance(e, jev_verify.JevDown):
                    run.not_run = run.not_run or f"Jev did not answer: {e}"
                elif not run.not_run:               # a checker fault is never the app's
                    run.not_run = f"the journeys could not run ({type(e).__name__})"
                    log.warning("app-check: journeys could not run (%s: %s)", type(e).__name__, str(e)[:200])
    finally:
        # the journeys still going must not go on asking Jev and driving pages after this check returned, into
        # a browser its caller may close
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
    run.results = [finished[n] for n in sorted(finished)]
    run.secs = time.perf_counter() - t0
    run.jev = verifier.calls.as_dict()
    log.info("app-check: %s in %.1f s (Jev %d calls, %d ms, p50 %d ms, %d input tokens, $%.5f, %d errors)",
             run.summary(), run.secs, run.jev["calls"], run.jev["ms"], run.jev["p50_ms"], run.jev["input_tokens"],
             run.jev["usd"], run.jev["errors"])
    return run


async def _layout(page: Any, sess: _Session, viewport: dict[str, int]) -> None:
    lay = await page.evaluate(LAYOUT_JS)
    # A phone browser widens the layout to fit what overflows, so innerWidth grows too: the overflow is how far
    # the page (layout plus scroll) reaches past the screen.
    over = lay["inner"] + max(0, lay["wide"]) - viewport["width"]
    if not lay["meta"]:
        sess.note(f"The page is laid out for a desktop ({lay['inner']}px wide on a {viewport['width']}px phone): "
                  'it needs <meta name="viewport" content="width=device-width, initial-scale=1">.')
    elif over > 2:
        sess.note(f"The page is {over}px wider than a {viewport['width']}px phone while {sess.action}, "
                  "so it shrinks or scrolls sideways.")
    if lay["text"] == 0:
        sess.note(f"The screen is blank while {sess.action}: nothing is rendered.")


async def _look(page: Any, sess: _Session, theme: dict[str, str], dark: bool) -> None:
    """The look checks (LOOK_JS) on the view on screen now; each kind of finding is reported once."""
    try:
        found = await page.evaluate(LOOK_JS, {"dark": dark, "pageBg": _rgb(theme["bg_color"]), "pencil": PENCIL})
    except Exception:  # noqa: BLE001 — a page mid-navigation: the next view is checked instead
        return
    for kind, items in found.items():
        if items and kind not in sess.looked:
            sess.looked.add(kind)
            sess.note(LOOKS[kind].format(action=sess.action, items="; ".join(items)))


def _rgb(hex_color: str) -> str:
    h = hex_color.lstrip("#")
    return f"rgb({int(h[0:2], 16)}, {int(h[2:4], 16)}, {int(h[4:6], 16)})"


async def _close(page: Any, sess: _Session) -> None:
    """Telegram closes the app without warning: a save that only comes after that never reaches the Mac. Then
    the other way an app is left: the phone locks with it open for an hour (a running timer finishes) and
    wakes up again."""
    sess.step("closing the app")
    await page.evaluate(CLOSE_JS, "hidden")
    await page.wait_for_timeout(CLOSE_GRACE_MS)
    sess.closed = True
    at_close = sess.stored
    await page.wait_for_timeout(LATE_MS)
    sess.closed = False
    if sess.late is not None and sess.late != at_close:
        sess.note("The app saved a change only after it was closed (a delayed save, such as a debounce). Telegram "
                  "closes the app without warning, so that change is lost: save straight after each change, or "
                  "also save on visibilitychange (hidden) and pagehide.")
    sess.late = None
    sess.step("leaving the app open on a locked phone for an hour")
    await page.evaluate(CLOSE_JS, "visible")
    try:
        await page.clock.fast_forward(AN_HOUR_MS)
    except Exception as e:  # noqa: BLE001 — a timer the app set threw: Playwright raises it here, not as a pageerror
        text = str(e)
        if "Error" in text.split("\n")[0]:
            msg = text.split("\n")[0].split(": ", 1)[-1][:300]
            sess.note(f"Uncaught error while {sess.action}: {msg}{_where(text)}")
    await page.wait_for_timeout(SETTLE_MS)


async def _photo(page: Any, ctx: Any, viewport: dict[str, int]) -> bytes:
    """The app as the owner sees it: when the MainButton shows, the page gets the screen above Telegram's
    bottom bar (as on the phone) and the bar is drawn under it, never over the page's content."""
    bar = await page.evaluate(BAR_JS)
    if bar:
        await page.evaluate("() => document.documentElement.style.setProperty('--tg-safe-area-inset-bottom', '0px')")
        await page.set_viewport_size({"width": viewport["width"], "height": viewport["height"] - BAR_H})
        await page.wait_for_timeout(TAP_WAIT_MS)
    await page.evaluate("() => window.scrollTo(0, 0)")
    if not bar:
        return await page.screenshot(type="jpeg", quality=80)
    shot = base64.b64encode(await page.screenshot(type="png")).decode()
    frame = await ctx.new_page()
    await frame.set_content(
        '<meta name="viewport" content="width=device-width, initial-scale=1">'
        f'<body style="margin:0;background:{bar["bg"]}"><img alt="" src="data:image/png;base64,{shot}" '
        f'style="display:block;width:{viewport["width"]}px;height:{viewport["height"] - BAR_H}px">'
        f'<div style="padding:8px 16px 42px"><div style="height:50px;border-radius:12px;display:flex;'
        f'align-items:center;justify-content:center;font:600 17px -apple-system,system-ui,sans-serif;'
        f'background:{bar["color"]};color:{bar["textColor"]}" id="b"></div></div></body>')
    await frame.evaluate("(t) => { document.getElementById('b').textContent = t; }", bar["text"])
    return await frame.screenshot(type="jpeg", quality=80)


async def _use(page: Any, sess: _Session, report: CheckReport, today: _dt.date, theme: dict[str, str]) -> None:
    """A person's first minute, view by view. What it saved is kept by ``sess`` (the richest data too)."""
    tapped: set[tuple[str, str]] = set()
    filled: dict[tuple[str, str, str], int] = {}
    pressed: dict[str, int] = {}
    groups: dict[str, int] = {}
    covered: dict[tuple[str, str], float] = {}         # first seen covered, by control
    waits, refill = 0, False
    for _ in range(MAX_ROUNDS):
        if report.taps >= MAX_TAPS:
            break
        await _look(page, sess, theme, True)
        moved = await _fill(page, sess, report, today, filled, refill)
        refill = False
        controls = [c for c in await page.evaluate(CONTROLS_JS, "taps")
                    if (c["label"], c["tag"]) not in tapped and groups.get(c["group"], 0) < MAX_PER_GROUP]
        # the likely "Add" first, before a tab hides it; destructive last in each view, so it is used first;
        # a way out (Cancel, Close) very last
        controls.sort(key=lambda c: (c["leave"], c["destructive"], not c["primary"]))
        for ctl in controls:
            if report.taps >= MAX_TAPS:
                break
            # Once the view's own Add-like taps are done, its MainButton goes before anything that may leave
            # the view or empty it (a form is submitted before its Cancel; "Add expense" before "Settings").
            if (not ctl["primary"] or ctl["leave"]) and await _press(
                    page, sess, "MainButton", pressed, fresh=True, primary_only=not ctl["leave"]):
                moved = True
                refill = await _rejected(page)
                break
            reach = await page.evaluate(REACH_JS, ctl["id"])
            key = (ctl["label"], ctl["tag"])
            if reach["state"] == "gone":
                moved = True                                    # the page re-rendered: read it again
                break
            if reach["state"] == "covered":
                first = covered.setdefault(key, time.perf_counter())
                if time.perf_counter() - first >= COVERED_FOR_S:
                    tapped.add(key)
                    if reach.get("small"):
                        sess.note(f"\"{ctl['label'] or ctl['tag']}\" cannot be tapped: \"{reach.get('by', '')}\" "
                                  f"stays on top of it (after {sess.action}). A toast or bar pinned to the screen "
                                  "must not cover controls: hide it when the view changes, and keep content "
                                  "clear of it.")
                continue
            tapped.add(key)
            if reach["state"] != "ok":
                continue
            sess.step(f"tapping \"{ctl['label'] or ctl['tag']}\"")
            try:
                await page.locator(f"[data-check-id='{ctl['id']}']").click(timeout=CLICK_TIMEOUT_MS)
            except Exception:  # noqa: BLE001 — moved or covered between the check and the tap: skip it
                continue
            report.taps += 1
            groups[ctl["group"]] = groups.get(ctl["group"], 0) + 1
            sess.primary_taps += bool(ctl["primary"])
            moved = True
            await page.wait_for_timeout(TAP_WAIT_MS)
            if ctl["primary"]:
                refill = await _rejected(page)
        if moved:
            continue
        pending = [k for k in covered if k not in tapped]
        if pending and waits < 5:
            waits += 1                                          # a toast on top: give it time to go
            await page.wait_for_timeout(COVERED_WAIT_MS)
            continue
        # This view is used up: its primary action (again: "New habit" opens the form a second time), then back
        # out of it; when there is neither, the app has been seen.
        before = await _errors(page)
        if any([await _press(page, sess, name, pressed) for name in ("MainButton", "SecondaryButton")]):
            after = await _errors(page)
            refill = bool(after) and after != before
            continue
        if not await _press(page, sess, "BackButton", pressed):
            break
    await page.wait_for_timeout(TAP_WAIT_MS)


async def _errors(page: Any) -> str:
    try:
        return str(await page.evaluate(ERRORS_JS))
    except Exception:  # noqa: BLE001 — mid-navigation
        return ""


async def _rejected(page: Any) -> bool:
    """Did the last press show an error next to a field: then its fields are filled again."""
    return bool(await _errors(page))


async def _fill(page: Any, sess: _Session, report: CheckReport, today: _dt.date,
                filled: dict[tuple[str, str, str], int], refill: bool = False) -> bool:
    """Each field once: typed into (a prefilled one with what it holds, so its change handler runs), then left,
    as a finger leaves it for the next control; a select keeps a real choice, else takes its second option.
    ``refill``: the app just rejected what was typed, so the empty or rejected fields get a second value (once
    each)."""
    moved = False
    for ctl in await page.evaluate(CONTROLS_JS, "fields"):
        key = (ctl["label"], ctl["tag"], ctl["type"])
        # a field seen before is filled again when it is empty (the same form opened anew), or once more when
        # the app rejected what was typed
        again = key in filled and filled[key] < 3 and ctl["tag"] != "select" and (
            ctl["value"] == "" or (refill and ctl["rejected"]))
        if (key in filled and not again) or report.fields >= MAX_FIELDS:
            continue
        filled[key] = filled.get(key, 0) + 1
        loc = page.locator(f"[data-check-id='{ctl['id']}']")
        sess.step(f"typing into the field \"{ctl['label'] or ctl['type'] or ctl['tag']}\"")
        try:
            if ctl["tag"] == "select":
                if ctl["options"] < 2:
                    continue
                keep = ctl["chosen"] > 0 or (ctl["chosen"] == 0 and ctl["chosenValue"] != "")
                await loc.select_option(index=ctl["chosen"] if keep else 1, timeout=CLICK_TIMEOUT_MS)
            else:
                value = _value_for(ctl, today, alt=again and ctl["value"] != "")
                if not value:
                    continue
                await loc.fill(value, timeout=CLICK_TIMEOUT_MS)
                await loc.evaluate("(el) => el.blur()", timeout=CLICK_TIMEOUT_MS)
        except Exception:  # noqa: BLE001 — a field that cannot be filled is skipped, not an app bug
            continue
        report.fields += 1
        moved = True
        await page.wait_for_timeout(40)
    return moved


async def _press(page: Any, sess: _Session, name: str, pressed: dict[str, int], *, fresh: bool = False,
                 primary_only: bool = False) -> bool:
    """Press Telegram's MainButton / SecondaryButton / BackButton when the app shows it. Each MainButton or
    SecondaryButton label twice at most (``fresh``: only one never pressed; ``primary_only``: only when its
    label reads like Add, Save or Create); Back as often as there is a view to back out of."""
    label = await page.evaluate("(n) => { const b = window.Telegram.WebApp[n]; return b && b.isVisible "
                                "&& b.isActive !== false ? (b.text || n) : null; }", name)
    if label is None or (primary_only and not _PRIMARY.match(label)):
        return False
    # counted per view: "Add expense" that opens the form and "Add expense" that submits it are two buttons
    key = f"{name}:{label}@{await page.evaluate(VIEW_JS)}"
    cap = 1 if fresh else MAX_ROUNDS if name == "BackButton" else 2
    if pressed.get(key, 0) >= cap:
        return False
    pressed[key] = pressed.get(key, 0) + 1
    before = sess.action
    sess.step(f"pressing Telegram's {name} (\"{label}\")" if name != "BackButton" else "pressing Telegram's Back button")
    try:
        done = await page.evaluate(PRESS_JS, name)
    except Exception as e:  # noqa: BLE001 — the app's handler threw: evaluate re-raises it as ours
        sess.note(f"Uncaught error while {sess.action}: {str(e).splitlines()[0][:300]}")
        return True
    if done is None:
        sess.action = before                    # nothing was pressed: later issues belong to the last action
        return False
    await page.wait_for_timeout(TAP_WAIT_MS)
    return True
