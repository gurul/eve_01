#!/usr/bin/env python3
"""Which planner model should drive the browser lane? Measured on eve's real path, per model.

    .venv/bin/python tools/browser_model_eval.py run --models gpt-6-astra --repeats 2 --out-dir DIR
    .venv/bin/python tools/browser_model_eval.py report --out-dir DIR

Owner, 2026-09-24: "can u also test models for browser use" — and, before that, "think about cost
efficiency". The browser lane (chrome_lane.py → ComputerAgent.run_in_browser) asks ONE planner call for a
typed plan, then plan_executor walks it with Jev grounding each click. Only the planner is swapped here:
the plan request (plan_contract.plan_request), the executor, the Jev step asker (browser_lane.make_step_asker,
the daemon's own wiring) and the page read are exactly production's.

What is different from production, on purpose:
- The browser is eve's OWN Chromium (non-attach mode), headless, in a fresh temporary profile per run.
  Never the owner's Chrome.
- Fixture sites are served from a local HTTP server on 127.0.0.1. The plan contract only opens clean https
  URLs, so the fixture sites have https names under ``.eve-eval.test`` and the browser context routes those
  requests to the loopback server (Playwright ``context.route``). The planner sees ordinary https sites.
- The owner's yes/no is answered by this tool: "yes" on the local fixtures (their submit buttons change only
  the fixture server's memory), "no" on the public sites (read-only tasks; nothing consequential may happen).
- Codex is NOT behind the lane: a run that hands off is recorded as a handoff, so the numbers are the lane's.

Every model goes to OpenRouter's Responses-compatible API (https://openrouter.ai/api/v1, OPENROUTER_API_KEY) with
the same request; a bare OpenAI name ("gpt-6-astra") is sent as "openai/gpt-6-astra" (eve_bridge.openrouter).
Until 2026-10-08 OpenAI models went to OpenAI directly on OPENAI_API_KEY.

Success is decided by code, never by a model: the fixture server's recorded state (a submission, the cart,
the saved settings, which page was opened), or a pattern in the spoken answer for a question.
"""

from __future__ import annotations

import argparse
import asyncio
import dataclasses
import http.server
import json
import os
import re
import shutil
import sys
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Optional
from urllib.parse import urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

FIXTURE_DOMAIN = "eve-eval.test"
TASK_TIMEOUT_SECS = 180.0
OPENROUTER_BASE = "https://openrouter.ai/api/v1"
DEFAULT_MODELS = ("gpt-6-astra", "gpt-6-sol", "gpt-6-luna", "google/gemini-3.7-flash", "z-ai/glm-5.3-flash")

# USD per million tokens: (input, cached input, output). gpt-6-astra from pricing.py; the rest from OpenRouter's
# /api/v1/models listing, read 2026-09-24 (it lists the openai/* ids at OpenAI's own rates). A model not here
# prices to None — never a guess. OpenRouter's own usage.cost wins when the reply carries it.
RATES: dict[str, tuple[float, float, float]] = {
    "gpt-6-astra": (10.0, 1.0, 50.0),
    "gpt-6-sol": (2.0, 0.20, 10.0),
    "gpt-6-luna": (0.10, 0.01, 0.50),
    "google/gemini-3.7-flash": (0.75, 0.075, 3.75),
    "z-ai/glm-5.3-flash": (0.15, 0.05, 0.50),
}


def is_openai_direct(model: str) -> bool:
    """An OpenAI model id goes to the OpenAI API with the owner's key; anything with a vendor prefix to OpenRouter."""
    return "/" not in model


def call_cost(model: str, usage: dict[str, Any]) -> Optional[float]:
    """USD for one Responses usage object: OpenRouter's usage.cost when present, else the RATES table, else None."""
    if isinstance(usage.get("cost"), (int, float)) and not isinstance(usage.get("cost"), bool):
        return float(usage["cost"])
    rate = RATES.get(model)
    if rate is None:
        return None
    details = usage.get("input_tokens_details") if isinstance(usage.get("input_tokens_details"), dict) else {}
    cached = int(details.get("cached_tokens") or 0)
    inp, out = int(usage.get("input_tokens") or 0), int(usage.get("output_tokens") or 0)
    return (max(0, inp - cached) * rate[0] + cached * rate[1] + out * rate[2]) / 1_000_000.0


# ---- the fixture sites --------------------------------------------------------------------------------------

_HEAD = ("<!doctype html><html lang=en><head><meta charset=utf-8><title>{title}</title><style>"
         "body{{font-family:system-ui,sans-serif;margin:24px;max-width:900px}} button{{margin:4px;padding:6px 10px}}"
         " .card{{border:1px solid #ccc;padding:8px;margin:6px 0}} [role=tabpanel][hidden]{{display:none}}"
         " table{{border-collapse:collapse}} td,th{{border:1px solid #999;padding:4px 8px}}</style></head><body>")
_TAIL = "</body></html>"


def _page(title: str, body: str) -> str:
    return _HEAD.format(title=title) + body + _TAIL


_POST_JS = ("<script>async function post(path, data){const r=await fetch(path,{method:'POST',"
            "headers:{'Content-Type':'application/json'},body:JSON.stringify(data)});return r.json();}</script>")

PRODUCTS = [
    ("Office", "Stapler"), ("Office", "Desk Lamp"), ("Garden", "Watering Can"), ("Office", "Notebook"),
    ("Garden", "Pruning Shears"), ("Office", "Pen Set"), ("Kitchen", "Blue Glass Mug"), ("Garden", "Seed Tray"),
    ("Kitchen", "Blue Ceramic Bowl"), ("Kitchen", "Blue Ceramic Mug"), ("Kitchen", "Red Ceramic Mug"),
    ("Garden", "Garden Gloves"), ("Kitchen", "Tea Towel"), ("Office", "Paper Tray"),
]

TOWNS = [("Ashford", "12,904", "31.2"), ("Brookfield", "33,410", "44.8"), ("Cedar Falls", "40,713", "75.1"),
         ("Riverton Heights", "12,870", "18.3"), ("Maple Grove", "71,025", "92.4"), ("Riverton", "48,213", "56.9"),
         ("Oakdale", "20,176", "27.5"), ("Pine Bluff", "41,253", "118.0"), ("Silverton", "7,432", "12.6"),
         ("Westfield", "18,009", "39.1")]


def _shop_page() -> str:
    cards = "".join(
        f'<div class="card" data-cat="{cat}"><b>{name}</b> — {cat} '
        f'<button aria-label="Add {name} to cart" onclick="add(\'{name}\')">Add to cart</button></div>'
        for cat, name in PRODUCTS)
    return _page("Eve Home Goods", _POST_JS + f"""
<h1>Eve Home Goods</h1><p id=cart>Cart (0)</p>
<div role=tablist aria-label="Category">
 <button role=tab aria-selected=true onclick="filt('All',this)">All</button>
 <button role=tab aria-selected=false onclick="filt('Kitchen',this)">Kitchen</button>
 <button role=tab aria-selected=false onclick="filt('Garden',this)">Garden</button>
 <button role=tab aria-selected=false onclick="filt('Office',this)">Office</button></div>
<div id=list>{cards}</div>
<script>
function filt(cat, el){{document.querySelectorAll('[role=tab]').forEach(t=>t.setAttribute('aria-selected', t===el));
 let shown=0; document.querySelectorAll('.card').forEach(c=>{{
  const ok = cat==='All' ? shown<6 : c.dataset.cat===cat; c.style.display = ok ? '' : 'none'; if(ok) shown++; }});}}
async function add(name){{const r=await post('/api/cart',{{item:name}}); document.getElementById('cart').textContent='Cart ('+r.count+')';
 const n=document.createElement('p'); n.textContent='Added '+name+' to your cart.'; document.body.appendChild(n);}}
filt('All', document.querySelector('[role=tab]'));
</script>""")


def fixture_pages() -> dict[str, dict[str, str]]:
    """{site: {path: html}}. Everything a task needs lives here; nothing leaves 127.0.0.1."""
    contact = _page("Contact us", _POST_JS + """
<h1>Contact us</h1>
<label for=n>Full name</label><br><input id=n name=name><br>
<label for=e>Email address</label><br><input id=e name=email type=email><br>
<label for=m>Your message</label><br><textarea id=m name=message rows=4></textarea><br>
<button onclick="send()">Send message</button><div id=out></div>
<script>async function send(){const d={name:n.value,email:e.value,message:m.value};await post('/api/contact',d);
document.getElementById('out').textContent='Thanks, '+d.name+'. Your message was sent.';}</script>""")
    docs_home = _page("Eve Docs", """
<h1>Eve Docs</h1><form action="/search" method=get role=search>
<input type=search name=q aria-label="Search docs" placeholder="Search docs"></form>
<p><a href="/article/getting-started">Getting started</a></p>""")
    results = [("solar-cleaning", "Solar Panel Cleaning Tips"), ("solar-lights", "Installing Solar Lights"),
               ("solar-panel-guide", "Solar Panel Installation Guide"), ("solar-faq", "Solar FAQ"),
               ("panel-wiring", "Panel Wiring Basics"), ("solar-panel-guide-v1", "Solar Panel Installation Guide (2019, archived)")]
    docs_search = _page("Search results — Eve Docs", "<h1>Search results</h1>" + "".join(
        f'<p><a href="/article/{slug}">{title}</a></p>' for slug, title in results))
    docs_article = _page("Article — Eve Docs", "<h1>Article</h1><p>This is an article.</p>")
    towns = _page("Towns of Lake County", "<h1>Towns of Lake County</h1><table><tr><th>Town</th><th>Population</th>"
                  "<th>Area (km²)</th></tr>" + "".join(f"<tr><td>{t}</td><td>{p}</td><td>{a}</td></tr>" for t, p, a in TOWNS)
                  + "</table>")
    settings = _page("Settings", _POST_JS + """
<h1>Account settings</h1>
<div role=tablist><button role=tab id=t1 aria-selected=true onclick="tab(1)">General</button>
<button role=tab id=t2 aria-selected=false onclick="tab(2)">Notifications</button>
<button role=tab id=t3 aria-selected=false onclick="tab(3)">Privacy</button></div>
<div role=tabpanel id=p1><label for=dn>Display name</label> <input id=dn value="Sam"></div>
<div role=tabpanel id=p2 hidden>
 <p><button role=switch id=push aria-checked=true onclick="flip(this)">Push notifications</button></p>
 <p><button role=switch id=digest aria-checked=false onclick="flip(this)">Weekly email digest</button></p>
 <p><button role=switch id=sms aria-checked=false onclick="flip(this)">SMS alerts</button></p></div>
<div role=tabpanel id=p3 hidden><button role=switch id=share aria-checked=false onclick="flip(this)">Share usage data</button></div>
<p><button onclick="save()">Save changes</button></p><div id=out></div>
<script>function tab(i){[1,2,3].forEach(j=>{document.getElementById('t'+j).setAttribute('aria-selected', i===j);
 document.getElementById('p'+j).hidden = i!==j;});}
function flip(b){b.setAttribute('aria-checked', b.getAttribute('aria-checked')!=='true');}
async function save(){const s={}; ['push','digest','sms','share'].forEach(k=>s[k]=document.getElementById(k).getAttribute('aria-checked')==='true');
 await post('/api/settings', s); document.getElementById('out').textContent='Your settings were saved.';}</script>""")
    recipes_home = _page("Eve Recipes", """
<h1>Eve Recipes</h1><ul><li><a href="/r/apple-crumble">Apple Crumble</a></li><li><a href="/r/lemon-tart">Lemon Tart</a></li>
<li><a href="/r/lemon-drizzle">Lemon Drizzle Cake</a></li><li><a href="/r/tomato-soup">Tomato Soup</a></li></ul>
<div id=cookie role=dialog aria-label="Cookie consent" style="position:fixed;inset:0;background:rgba(0,0,0,.55);display:flex;
align-items:center;justify-content:center"><div style="background:#fff;padding:24px;max-width:420px">
<p>We use cookies to improve your experience.</p>
<button onclick="document.getElementById('cookie').remove()">Accept all cookies</button>
<button onclick="document.getElementById('cookie').remove()">Reject non-essential cookies</button></div></div>""")
    recipe = _page("Recipe — Eve Recipes", "<h1>Recipe</h1><p>Mix, bake, enjoy.</p>")
    filler = "".join(f"<h2>Feature {i}</h2><p>{'Everything you need to run a tidy little business. ' * 6}</p>"
                     for i in range(1, 13))
    pricing_page = _page("Pricing — Eve Cloud", _POST_JS + f"""
<nav><a href="/">Home</a> <a href="/#features">Features</a> <a href="/#faq">FAQ</a></nav><h1>Eve Cloud pricing</h1><p>Simple plans for everyone.</p>{filler}
<p><button onclick="cmp()">Compare all plans</button></p><div id=out></div>
<script>async function cmp(){{await post('/api/compare',{{}});document.getElementById('out').textContent=
'Starter $5, Team $15, Business $40 per month.';}}</script>""")
    reserve = _page("Reserve a table — Bistro Eve", _POST_JS + """
<h1>Reserve a table</h1>
<fieldset><legend>Party size</legend>""" + "".join(
        f'<label><input type=radio name=party value={n}> {n} people</label> ' for n in (2, 4, 6)) + """</fieldset>
<fieldset><legend>Day</legend>""" + "".join(
        f'<label><input type=radio name=day value={d}> {d}</label> ' for d in ("Thursday", "Friday", "Saturday")) + """</fieldset>
<fieldset><legend>Time</legend>""" + "".join(
        f'<label><input type=radio name=time value="{t}"> {t}</label> ' for t in ("6:00 pm", "7:00 pm", "8:00 pm")) + """</fieldset>
<label for=who>Name for the booking</label> <input id=who><br>
<button onclick="book()">Request reservation</button><div id=out></div>
<script>async function book(){const v=n=>(document.querySelector('input[name='+n+']:checked')||{}).value||'';
const d={party:v('party'),day:v('day'),time:v('time'),name:document.getElementById('who').value};
await post('/api/reserve',d); document.getElementById('out').textContent='Reservation requested for '+d.name+'.';}</script>""")
    help_home = _page("Help Center", """
<h1>Help Center</h1><ul><li><a href="/returns">Returns and refunds</a></li><li><a href="/shipping">Shipping</a></li>
<li><a href="/payments">Payments</a></li><li><a href="/account">Your account</a></li></ul>""")
    help_ship = _page("Shipping — Help Center", """<h1>Shipping</h1>
<p>Standard shipping takes 5 to 7 business days. Express shipping takes 2 business days and costs $12.</p>
<p>Orders placed after 2 pm ship the next business day.</p>""")
    help_ret = _page("Returns — Help Center", "<h1>Returns and refunds</h1><p>Returns are accepted within 30 days.</p>")
    return {
        "forms": {"/contact": contact},
        "shop": {"/": _shop_page()},
        "docs": {"/": docs_home, "/search": docs_search, **{f"/article/{s}": docs_article for s, _ in results},
                 "/article/getting-started": docs_article},
        "data": {"/towns": towns},
        "settings": {"/": settings},
        "recipes": {"/": recipes_home, **{f"/r/{s}": recipe for s in ("apple-crumble", "lemon-tart", "lemon-drizzle",
                                                                         "tomato-soup")}},
        "pricing": {"/": pricing_page},
        "bistro": {"/reserve": reserve},
        "help": {"/": help_home, "/shipping": help_ship, "/returns": help_ret, "/payments": help_ret,
                 "/account": help_ret},
    }


class FixtureState:
    """What the fixture sites saw: every page visit and every POST. A check reads only this and the answer."""

    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.reset()

    def reset(self) -> None:
        with self.lock:
            self.visits: list[str] = []            # "site/path"
            self.posts: list[tuple[str, dict[str, Any]]] = []

    def snapshot(self) -> dict[str, Any]:
        with self.lock:
            return {"visits": list(self.visits), "posts": [{"path": p, "data": d} for p, d in self.posts]}


def serve_fixtures(state: FixtureState) -> tuple[http.server.ThreadingHTTPServer, int]:
    """The fixture sites on 127.0.0.1 (port chosen by the OS). Paths are /<site><path>."""
    pages = fixture_pages()

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_a: Any) -> None:
            pass

        def _split(self) -> tuple[str, str]:
            path = urlsplit(self.path).path
            site, _, rest = path.lstrip("/").partition("/")
            return site, "/" + rest

        def do_GET(self) -> None:  # noqa: N802 — the stdlib's name
            site, path = self._split()
            html = pages.get(site, {}).get(path)
            if html is None:
                self.send_response(404)
                self.end_headers()
                return
            with state.lock:
                state.visits.append(site + path)
            body = html.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self) -> None:  # noqa: N802
            site, path = self._split()
            raw = self.rfile.read(int(self.headers.get("Content-Length") or 0))
            try:
                data = json.loads(raw or b"{}")
            except ValueError:
                data = {"raw": raw.decode(errors="replace")}
            with state.lock:
                state.posts.append((site + path, data if isinstance(data, dict) else {"value": data}))
                count = sum(1 for p, _ in state.posts if p == site + path)
            body = json.dumps({"ok": True, "count": count}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, srv.server_address[1]


def local_url(https_url: str, port: int) -> Optional[str]:
    """https://<site>.eve-eval.test/<path>?q → http://127.0.0.1:<port>/<site>/<path>?q, or None for any other URL."""
    parts = urlsplit(https_url)
    host = (parts.hostname or "").lower()
    if not host.endswith("." + FIXTURE_DOMAIN):
        return None
    site = host[: -len(FIXTURE_DOMAIN) - 1]
    return f"http://127.0.0.1:{port}/{site}{parts.path or '/'}" + (f"?{parts.query}" if parts.query else "")


# ---- the tasks ----------------------------------------------------------------------------------------------

def _posts(state: dict[str, Any], path: str) -> list[dict[str, Any]]:
    return [p["data"] for p in state.get("posts") or [] if p.get("path") == path]


def _norm(s: Any) -> str:
    return " ".join(str(s or "").split()).casefold()


def check_contact(answer: str, state: dict[str, Any]) -> bool:
    subs = _posts(state, "forms/api/contact")
    return len(subs) == 1 and _norm(subs[0].get("name")) == "ada lovelace" and \
        _norm(subs[0].get("email")) == "ada@example.com" and "call me back" in _norm(subs[0].get("message"))


def check_cart(answer: str, state: dict[str, Any]) -> bool:
    return [_norm(d.get("item")) for d in _posts(state, "shop/api/cart")] == ["blue ceramic mug"]


def check_search_link(answer: str, state: dict[str, Any]) -> bool:
    visits = state.get("visits") or []
    return "docs/article/solar-panel-guide" in visits and any(v.startswith("docs/search") for v in visits)


def check_settings(answer: str, state: dict[str, Any]) -> bool:
    saves = _posts(state, "settings/api/settings")
    return bool(saves) and saves[-1] == {"push": True, "digest": True, "sms": False, "share": False}


def check_recipe(answer: str, state: dict[str, Any]) -> bool:
    return "recipes/r/lemon-tart" in (state.get("visits") or [])


def check_below_fold(answer: str, state: dict[str, Any]) -> bool:
    return len(_posts(state, "pricing/api/compare")) >= 1


def check_reservation(answer: str, state: dict[str, Any]) -> bool:
    subs = _posts(state, "bistro/api/reserve")
    return len(subs) == 1 and subs[0].get("party") == "4" and subs[0].get("day") == "Friday" and \
        subs[0].get("time") == "7:00 pm" and _norm(subs[0].get("name")) == "grace hopper"


def answer_matches(pattern: str, *, forbid: str = "") -> Callable[[str, dict[str, Any]], bool]:
    rx, bad = re.compile(pattern, re.I), re.compile(forbid, re.I) if forbid else None

    def check(answer: str, state: dict[str, Any]) -> bool:
        text = answer or ""
        return bool(rx.search(text)) and not (bad is not None and bad.search(text))
    return check


@dataclass(frozen=True)
class Task:
    id: str
    where: str                        # local | public
    goal: str
    check: Callable[[str, dict[str, Any]], bool]
    start: str = ""                   # a URL already open when the task begins; "" = a blank tab (production's new tab)
    note: str = ""                    # what the task exercises


TASKS: tuple[Task, ...] = (
    Task("contact_form", "local", "On the contact form at https://forms.eve-eval.test/contact, fill in the name Ada Lovelace, the email ada@example.com and "
         "the message Please call me back, then send it.", check_contact,
         start="https://forms.eve-eval.test/contact", note="form fill + submit (asks the owner)"),
    Task("shop_filter_cart", "local", "On the shop at https://shop.eve-eval.test, show only the Kitchen products and add the Blue Ceramic Mug to the "
         "cart.", check_cart, start="https://shop.eve-eval.test/",
         note="filter tab, then the one right item among look-alikes"),
    Task("search_then_link", "local", "Go to https://docs.eve-eval.test, search for solar panels and open the Solar "
         "Panel Installation Guide.", check_search_link, note="blank tab: open, search, pick a result"),
    Task("table_read", "local", "What is the population of Riverton in the table on https://data.eve-eval.test/towns?",
         answer_matches(r"48[,.\s]?213", forbid=r"12[,.\s]?870"), start="https://data.eve-eval.test/towns",
         note="read one cell, a near-duplicate row beside it"),
    Task("settings_tab_toggle", "local", "In my settings at https://settings.eve-eval.test, turn on the weekly email digest under Notifications and "
         "save the changes.", check_settings, start="https://settings.eve-eval.test/",
         note="toggle behind a tab, then save; other switches untouched"),
    Task("cookie_banner", "local", "Open the Lemon Tart recipe on https://recipes.eve-eval.test.", check_recipe,
         start="https://recipes.eve-eval.test/", note="a cookie dialog covers the page"),
    Task("below_the_fold", "local", "On the pricing page at https://pricing.eve-eval.test, click the Compare all plans button at the bottom.",
         check_below_fold, start="https://pricing.eve-eval.test/", note="the target is below the fold"),
    Task("reservation_form", "local", "Reserve a table at https://bistro.eve-eval.test/reserve for 4 people on Friday at 7:00 pm under the name "
         "Grace Hopper.", check_reservation, start="https://bistro.eve-eval.test/reserve",
         note="three radio groups, a text field, a submit"),
    Task("help_read_after_nav", "local", "Go to https://help.eve-eval.test, open the Shipping page and tell me how "
         "many business days standard shipping takes.", answer_matches(r"\b5\b.{0,12}\b7\b|five.{0,12}seven"),
         note="blank tab: navigate two pages, then answer from the text"),
    Task("example_heading", "public", "What does the main heading say on https://example.com?",
         answer_matches(r"example domain"), note="public, trivial read"),
    Task("wikipedia_search", "public", "Search Wikipedia for Ada Lovelace and tell me the year she was born.",
         answer_matches(r"\b1815\b"), note="public: search a real site, read one fact"),
    Task("wikipedia_fact", "public", "On the English Wikipedia page for the Mariana Trench, what is its maximum known "
         "depth in metres?", answer_matches(r"\b1[01][,.\s]?\d{3}\b"), note="public: open an article, read a number"),
)


# ---- one run --------------------------------------------------------------------------------------------------

def lane_trace(run_log: str) -> list[dict[str, Any]]:
    """What the executor did, from the agent's own run log: each plan run's status and reason, and the last
    ledger entry's effect and note. [] when the log is missing."""
    out: list[dict[str, Any]] = []
    try:
        lines = Path(run_log).read_text().splitlines() if run_log else []
    except OSError:
        return out
    for line in lines:
        try:
            d = json.loads(line)
        except ValueError:
            continue
        led = d.get("ledger")
        if isinstance(led, dict):
            last = (led.get("ledger") or [{}])[-1]
            out.append({"status": led.get("status"), "reason": str(led.get("reason") or "")[:160],
                        "last_effect": last.get("effect"), "last_note": str(last.get("note") or "")[:160]})
        elif isinstance(d.get("plan"), dict) and d["plan"].get("error"):
            out.append({"status": "plan_error", "reason": str(d["plan"]["error"])[:160]})
    return out


def outcome_of(answer: str, note: str) -> str:
    """finished (the lane answered) | part_done (a Codex handoff with a note) | nothing (Codex takes the whole task)."""
    if answer:
        return "finished"
    return "part_done" if note.strip() else "nothing"


def make_creator(model: str, env: dict[str, str], calls: list[dict[str, Any]]) -> Callable[[dict[str, Any]], Any]:
    """responses.create for `model`, recording each call's usage, on OpenRouter's Responses API (a bare OpenAI name
    gets its "openai/" prefix there). The request itself is otherwise passed through untouched."""
    from eve_bridge import openrouter

    client = openrouter.async_client(env["OPENROUTER_API_KEY"])

    async def create(request: dict[str, Any]) -> dict[str, Any]:
        t0 = time.perf_counter()
        try:
            r = await client.responses.create(**request)
        except Exception as e:  # noqa: BLE001 — recorded, then the agent sees it exactly as in production
            calls.append({"secs": round(time.perf_counter() - t0, 2), "error": f"{type(e).__name__}: {e}"[:400]})
            raise
        d = r.model_dump(exclude_none=True)
        usage = d.get("usage") if isinstance(d.get("usage"), dict) else {}
        calls.append({"secs": round(time.perf_counter() - t0, 2), "usage": usage, "usd": call_cost(model, usage),
                      "status": d.get("status"), "text": str(getattr(r, "output_text", "") or "")[:3000]})
        return d

    return create


async def run_one(task: Task, model: str, env: dict[str, str], state: FixtureState, port: int,
                  step_asker: Any) -> dict[str, Any]:
    from eve_bridge import computer_agent as ca
    from eve_bridge.browser_lane import BrowserLane, BrowserLaneConfig

    profile = Path(tempfile.mkdtemp(prefix="eve-eval-profile-"))
    lane = BrowserLane(BrowserLaneConfig(profile=profile, headless=True, attach=False),
                       step_asker=step_asker)
    calls: list[dict[str, Any]] = []
    asks: list[str] = []
    events: list[str] = []

    def install_routes() -> None:
        lane._ensure()                                          # eve's own Chromium, launched the production way

        def handle(route: Any) -> None:
            target = local_url(route.request.url, port)
            if target is None:
                route.continue_()
                return
            resp = route.fetch(url=target)
            route.fulfill(response=resp)

        lane._context.route(f"https://*.{FIXTURE_DOMAIN}/**", handle)

    async def ask_user(question: str) -> str:
        asks.append(question)
        return "yes" if task.where == "local" else "no"

    row: dict[str, Any] = {"task": task.id, "where": task.where, "model": model}
    state.reset()
    t0 = time.perf_counter()
    try:
        await lane._run(install_routes)
        if task.start:
            await lane.open_url(task.start)
        state.reset()                                           # the pre-opened page is not the model's visit
        # One run-log directory per model: two models' logs for the same second must never share a file.
        cfg = dataclasses.replace(ca.configured(env), model=model,
                                  runs_dir=Path(env["EVE_AGENT_RUNS_DIR"]) / model.replace("/", "_"))
        agent = ca.ComputerAgent(make_creator(model, env, calls), config=cfg, ask_user=ask_user, browser=lane,
                                 on_event=lambda ev: events.append(f"{ev.kind}: {str(ev.text)[:120]}"))
        t_run = time.perf_counter()
        answer, note = await asyncio.wait_for(agent.run_in_browser(task.goal), TASK_TIMEOUT_SECS)
        secs = time.perf_counter() - t_run
        bill = agent.bill()
        row.update(answer=answer, note=note.strip()[:400], outcome=outcome_of(answer, note), secs=round(secs, 2),
                   jev_calls=int(bill["jev"]["calls"]), jev_errors=int(bill["jev"]["errors"]),
                   jev_usd=float(bill["jev"]["usd"]), run_log=str(agent.run_log or ""))
    except Exception as e:  # noqa: BLE001 — a harness failure is recorded, never a crash of the whole eval
        row.update(answer="", note="", outcome="error", secs=round(time.perf_counter() - t0, 2), jev_calls=0,
                   jev_errors=0, jev_usd=0.0, error=f"{type(e).__name__}: {e}"[:400])
    finally:
        try:
            await lane.close()
        except Exception:  # noqa: BLE001
            pass
        shutil.rmtree(profile, ignore_errors=True)
    snap = state.snapshot()
    row["state"] = snap
    row["success"] = bool(task.check(row.get("answer") or "", snap))
    row["asks"] = asks
    row["planner_calls"] = len(calls)
    row["planner_errors"] = [c["error"] for c in calls if c.get("error")]
    row["planner_tokens"] = {
        "in": sum(int((c.get("usage") or {}).get("input_tokens") or 0) for c in calls),
        "out": sum(int((c.get("usage") or {}).get("output_tokens") or 0) for c in calls)}
    costs = [c.get("usd") for c in calls if not c.get("error")]
    row["planner_usd"] = None if any(x is None for x in costs) else round(sum(costs), 6)
    row["planner_secs"] = round(sum(float(c.get("secs") or 0) for c in calls), 2)
    row["events"] = events[-8:]
    row["plans"] = [c.get("text", "") for c in calls if not c.get("error")]
    row["trace"] = lane_trace(row.get("run_log") or "")
    return row


async def run_models(models: list[str], task_ids: list[str], repeats: int, out_dir: Path) -> list[dict[str, Any]]:
    from eve_bridge.browser_lane import make_step_asker
    from eve_bridge.envfile import load_env_file

    load_env_file()
    env = dict(os.environ)
    env.pop("EVE_AGENT_MODEL", None)                      # the model is this tool's variable, nothing else's
    env["EVE_AGENT_RUNS_DIR"] = str(out_dir / "agent-runs")  # run logs stay with the eval, not the owner's
    # The daemon's own wiring (daemon._make_chrome_lane): Jev grounds each click unless turned off.
    step_asker = make_step_asker({**env, "EVE_JEV_STEP": env.get("EVE_JEV_STEP", "1")})
    if step_asker is None:
        raise SystemExit("Jev is not configured (EVE_JEV_ROUTE and its key): production grounds every click with it")
    state = FixtureState()
    srv, port = serve_fixtures(state)
    tasks = [t for t in TASKS if not task_ids or t.id in task_ids]
    rows: list[dict[str, Any]] = []
    try:
        for model in models:
            path = out_dir / f"runs-{model.replace('/', '_')}.jsonl"
            for rep in range(1, repeats + 1):
                for task in tasks:
                    row = await run_one(task, model, env, state, port, step_asker)
                    row["repeat"] = rep
                    rows.append(row)
                    with path.open("a") as f:
                        f.write(json.dumps(row, ensure_ascii=False) + "\n")
                    cost = "?" if row["planner_usd"] is None else f"${row['planner_usd']:.4f}"
                    print(f"{model:26s} r{rep} {task.id:22s} {'OK  ' if row['success'] else 'FAIL'} "
                          f"{row['outcome']:9s} {row['secs']:6.1f}s {cost:>9s} jev={row['jev_calls']}"
                          + (f" err={row.get('error') or row['planner_errors'][:1]}" if row.get("error") or row["planner_errors"] else ""),
                          flush=True)
    finally:
        srv.shutdown()
    return rows


# ---- the report -------------------------------------------------------------------------------------------------

def load_rows(out_dir: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for p in sorted(out_dir.glob("runs-*.jsonl")):
        rows += [json.loads(line) for line in p.read_text().splitlines() if line.strip()]
    return rows


def aggregate(rows: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    """Per model: runs, successes, success rate, mean seconds, mean planner and Jev cost per task, handoffs
    (part_done + nothing: every run Codex would have had to finish), harness errors, and planner API errors."""
    out: dict[str, dict[str, Any]] = {}
    for r in rows:
        m = out.setdefault(r["model"], {"runs": 0, "success": 0, "finished": 0, "finished_ok": 0, "part_done": 0,
                                        "nothing": 0, "errors": 0, "secs": [], "planner_usd": [], "jev_usd": [],
                                        "jev_calls": 0, "priced": True, "api_errors": [], "by_task": {}})
        m["runs"] += 1
        m["success"] += bool(r.get("success"))
        o = r.get("outcome")
        if o == "finished":
            m["finished"] += 1
            m["finished_ok"] += bool(r.get("success"))
        elif o in ("part_done", "nothing"):
            m[o] += 1
        else:
            m["errors"] += 1
        m["secs"].append(float(r.get("secs") or 0))
        if r.get("planner_usd") is None:
            m["priced"] = False
        else:
            m["planner_usd"].append(float(r["planner_usd"]))
        m["jev_usd"].append(float(r.get("jev_usd") or 0))
        m["jev_calls"] += int(r.get("jev_calls") or 0)
        m["api_errors"] += list(r.get("planner_errors") or [])
        t = m["by_task"].setdefault(r["task"], [0, 0])
        t[0] += bool(r.get("success"))
        t[1] += 1
    for m in out.values():
        n = max(1, m["runs"])
        m["success_rate"] = round(m["success"] / n, 4)
        m["handoffs"] = m["part_done"] + m["nothing"]
        m["avg_secs"] = round(sum(m["secs"]) / n, 2)
        m["median_secs"] = round(sorted(m["secs"])[len(m["secs"]) // 2], 2) if m["secs"] else 0.0
        m["avg_planner_usd"] = round(sum(m["planner_usd"]) / len(m["planner_usd"]), 6) if m["planner_usd"] else None
        m["total_planner_usd"] = round(sum(m["planner_usd"]), 6)
        m["avg_jev_usd"] = round(sum(m["jev_usd"]) / n, 6)
        m["avg_jev_calls"] = round(m["jev_calls"] / n, 2)
        del m["secs"], m["planner_usd"], m["jev_usd"]
    return out


def recommend(summary: dict[str, dict[str, Any]], baseline: str = "gpt-6-astra", tolerance: float = 0.05) -> str:
    """The owner's rule: the cheapest model whose success rate is within `tolerance` of the baseline's (or above
    it). Unpriced models are not picked; the baseline wins when nothing cheaper holds."""
    base = summary.get(baseline)
    if base is None:
        return ""
    ok = [(m, s) for m, s in summary.items() if s.get("avg_planner_usd") is not None and s["errors"] < s["runs"]
          and s["success_rate"] >= base["success_rate"] - tolerance]
    if not ok:
        return baseline
    return min(ok, key=lambda ms: (ms[1]["avg_planner_usd"], -ms[1]["success_rate"]))[0]


def markdown(summary: dict[str, dict[str, Any]], rows: list[dict[str, Any]], pick: str) -> str:
    lines = ["| model | success rate | avg time | avg cost per task (planner) | handoffs | notes |",
             "|---|---|---|---|---|---|"]
    for model, s in sorted(summary.items(), key=lambda kv: -kv[1]["success_rate"]):
        cost = "unpriced" if s["avg_planner_usd"] is None else f"${s['avg_planner_usd']:.4f}"
        notes = [f"finished {s['finished']}/{s['runs']} ({s['finished_ok']} correct)",
                 f"part-done {s['part_done']}", f"nothing {s['nothing']}", f"Jev {s['avg_jev_calls']}/task"]
        if s["errors"]:
            notes.append(f"harness errors {s['errors']}")
        if s["api_errors"]:
            notes.append(f"API errors {len(s['api_errors'])}: {s['api_errors'][0][:80]}")
        lines.append(f"| {model} | {s['success']}/{s['runs']} ({s['success_rate']:.0%}) | {s['avg_secs']:.1f} s | "
                     f"{cost} | {s['handoffs']} | {'; '.join(notes)} |")
    tasks = sorted({r["task"] for r in rows}, key=[t.id for t in TASKS].index)
    models = sorted(summary, key=lambda m: -summary[m]["success_rate"])
    lines += ["", "Per task (successes / runs):", "", "| task | " + " | ".join(models) + " |",
              "|---" * (len(models) + 1) + "|"]
    for t in tasks:
        cells = []
        for m in models:
            ok, n = summary[m]["by_task"].get(t, [0, 0])
            cells.append(f"{ok}/{n}")
        lines.append(f"| {t} | " + " | ".join(cells) + " |")
    if pick:
        lines += ["", f"Pick by the rule (cheapest within 5 points of gpt-6-astra): **{pick}**"]
    return "\n".join(lines)


def main() -> int:
    p = argparse.ArgumentParser(prog="browser_model_eval")
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--models", default=",".join(DEFAULT_MODELS))
    r.add_argument("--tasks", default="")
    r.add_argument("--repeats", type=int, default=2)
    r.add_argument("--out-dir", required=True)
    g = sub.add_parser("report")
    g.add_argument("--out-dir", required=True)
    args = p.parse_args()
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    if args.cmd == "run":
        models = [m.strip() for m in args.models.split(",") if m.strip()]
        ids = [t.strip() for t in args.tasks.split(",") if t.strip()]
        asyncio.run(run_models(models, ids, args.repeats, out_dir))
        print("BROWSER_MODEL_EVAL_RUN_COMPLETE")
        return 0
    rows = load_rows(out_dir)
    summary = aggregate(rows)
    pick = recommend(summary)
    total = sum(s["total_planner_usd"] for s in summary.values()) + sum(float(r.get("jev_usd") or 0) for r in rows)
    (out_dir / "report.json").write_text(json.dumps({"summary": summary, "pick": pick, "total_usd": round(total, 4),
                                                     "runs": rows}, indent=1, ensure_ascii=False))
    table = markdown(summary, rows, pick)
    (out_dir / "table.md").write_text(table + "\n")
    print(table)
    print(f"\ntotal model spend (planner + Jev): ${total:.4f}")
    print("BROWSER_MODEL_EVAL_REPORT_COMPLETE")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
