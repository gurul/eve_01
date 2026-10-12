"""Eve Link's chat: the owner texts eve in the iPhone app instead of Telegram (docs/action-button.md).

Owner, 2026-10-08: the chat moves from Telegram into Eve Link. The app draws the conversation with OpenUI in a web
view and does all its networking natively (ios/EveLink); this module is the daemon's half. Two HTTP paths beside
``/api/stick`` on the Mini App's server (miniapp.py), reached through its Cloudflare tunnel:

* ``POST /api/link/turn`` ``{"text", "client_id"}``: the words are handled exactly as a message the owner typed in
  the Mac chat window (``TelegramInlet.hear(text, on_call=False)``), marked as the app's (``app=<turn id>``, the
  inlet's ``_FROM_APP``). The answer streams back as OpenAI Chat Completions SSE chunks, one eve line at a time
  (lines after the first are joined by a blank line), then ``data: [DONE]``. A ``: ping`` comment every
  ``PING_SECS`` keeps the tunnel from timing out a quiet stream. The stream ends when no turn the owner started is
  in flight (``TelegramInlet.owner_busy``), when the turn asks the owner something (a chief's Go, an approval: the
  answer is the owner's next text from the app), or after ``TURN_SECS``. The same ``client_id`` again within
  ``DEDUPE_SECS`` (the phone retried after a drop) is never heard twice: it streams the first turn's reply.
* ``GET /api/link/messages?after=<cursor>&wait=<0..25>``: the conversation as the transcript has it
  (transcripts.py), so the app shows what was said in Telegram, on a call or by a task that answered later.
  Without ``after``: the last ``HISTORY_LIMIT`` lines of the last ``HISTORY_DAYS`` transcript days. With it: newer
  lines only, and with ``wait`` a long poll woken by the transcript's own append (``Transcripts.listeners``),
  with a re-read every ``POLL_SECS`` for writes from another process (the forget CLI).

* ``POST /api/link/turn`` also takes ``"grade"``, the app's speed dial: ``auto`` (the default: the chat as it is),
  ``instant`` (the chat's model at low effort, no think_hard) or ``think`` (``EVE_APP_THINK_MODEL`` at
  ``EVE_APP_THINK_EFFORT``), applied by telegram.graded to every model call of the turn. Quick answers by code
  still answer first in each grade.
* ``GET /api/link/jobs``: the chief's cards (chief.py) for the app's Work view, each ``Chief._as_data`` plus
  ``rev``, a status ``line``, the steps done, ``needs_you`` and the ``actions`` that apply now; open cards first
  (the ones that need the owner first), then the last ``FINISHED_SHOWN`` finished. 503 when the chief is off.
* ``POST /api/link/jobs`` ``{"goal", "detail"?, "client_id"?}`` (owner, 2026-10-09: "work tab should allow me to start
  long horizon task not only just track"): the Work tab's New job. A card comes only from the brain's ``take_on``
  (chief_card.parse needs its whole shape), so the words are heard as an app turn, exactly as ``POST
  /api/link/turn`` hears typed words, led by ``JOB_LEAD``: the brain, its tools, take_on's parse, door floor and
  backbrief, and every chief rule after it are the chat's own, and nothing here files a card. The request waits
  for that turn (at most ``JOB_SECS``) and answers ``{"ok": true, "job": <one card as GET gives it>, "said": [...]}``
  for the card that turn's own take_on filed (``TelegramInlet.app_filed``, keyed by the turn's id: a card another
  turn files meanwhile is never returned), or ``{"ok": false, "reason", "said"}`` when the brain answered without
  one (a question back, a one-step task, a refused card). ``said``: eve's lines on that turn, OpenUI programs made
  plain; take_on's backbrief is one of them, with its Change and Drop as the words that answer them
  (``telegram.reply_words``), and the chat shows it from the transcript.
  The same ``client_id`` within ``DEDUPE_SECS`` (the phone retried) waits for the first request's turn, never a
  second card. 503 ``NO_CHIEF`` when the chief is off, ``NO_CHAT`` when the chat is not running.
* ``POST /api/link/jobs/<id>/<go|done|drop|raise|happened|didnt>``: the chief's own methods (``approve(via="id")``,
  ``done``, ``drop``, ``raise_budget``, ``happened``, ``didnt``), exactly as "go c12" and the other code words in
  Telegram, with every chief rule they keep; a Go may carry the ``rev`` the app showed, and a card changed since is
  refused. A Go on a card whose act may already have happened is refused: the app offers "It happened" and "It
  didn't" there, as Telegram's buttons do (Chief.choices), and a deliberate re-run stays the typed "go c12".
  Their dict is the answer.
* ``POST /api/link/jobs/<id>/hide`` (owner, 2026-10-10: "i should also have ability to delete completed jobs i dont
  want to be forced to see them"): a finished card leaves the Work list for good (``HiddenJobs``); the chief's
  ledger keeps it, so its record and memory stay whole. An open card is refused: Drop ends it first.
* ``POST /api/link/upload``: one attachment (a photo, a PDF, a text file or an Office document; link_uploads.py),
  the raw bytes as the body, its name in ``X-Filename`` (percent-encoded), at most ``link_uploads.MAX_BYTES``. The
  answer is ``{"ok": true, "id", "kind", "name", "mime", "size"}``; 413 too large, 415 a type eve does not take.
  ``POST /api/link/turn`` then takes ``"attachments": [id, ...]`` (at most ``MAX_ATTACHMENTS``; the text may be
  empty then): the turn reaches the brain as a Telegram photo does (TelegramInlet._image: an image as vision input,
  or saved for the Claude and Codex relays), and a file with it (link_uploads.brain_parts). The owner's transcript
  line keeps the attachments' ids, names and kinds (``att``), and ``/api/link/messages`` gives each owner message
  its ``attachments`` with an image's small ``thumb`` (a data: URL) while the upload lives (24 h).
* **Reactions** (link_reactions.py; owner, 2026-10-09: "reactions … like telegram"). An app turn's owner message
  carries eve's receipt as Telegram's does: 👀 while it reads a photo or file, ✍ a note kept or note taking
  started, 🏆 a fact starred, 👍 typed to Claude or Codex. The turn's id is the app's ``client_id`` when that is a
  plain id (``turn_id``), so the page knows which bubble a reaction is for. The owner's transcript line keeps it as
  ``mid``; ``/api/link/messages`` gives that row ``rid`` and, when it has one, its ``reaction``, plus ``reactions``
  (every reaction kept, by ``rid``) so a reaction that changes on a message already shown reaches the app. The
  cursor then ends ``~<version>`` (``Reactions.version``): a poll whose cursor names an older version answers at
  once, and a change wakes a waiting poll as a new line does.
* **eve's pictures and files** (owner, 2026-10-09: "It can't send attachments" / "Like images"). Every photo,
  screenshot, file or app picture eve sends the owner goes through TelegramInlet._send_media, which keeps a copy in
  the same store (``UploadStore.keep``) and writes a ``kind: "media"`` transcript line for eve with ``att``, its
  caption as the text (maybe empty). ``/api/link/messages`` returns it as an eve message with ``attachments``, so
  the app shows it in the chat whether Telegram is on or app-only. A file over ``link_uploads.MAX_BYTES`` is a line
  naming it instead. They are not in the model's history (no prompt block, chat log, seed or index takes the
  kind): it already had the picture as its tool's result. The transcript ``read`` and ``search`` tools show them as
  any other line.
* **Sources** (memory_sources.py; ChatGPT's memory article: "Sources may appear below the response"). A turn that
  read memory (``memory_search``, ``memory_read``) writes what those reads found on its reply's transcript line as
  ``src``, at most ``memory_sources.MAX_SOURCES``; ``/api/link/messages`` gives that eve row ``sources``
  (``[{"kind": "fact" | "remembered" | "said", "text", "when"?}]``, sanitized again here), and the app shows
  "Sources · N" under the reply. The model never sees them: its tool results and history are unchanged.
* ``GET /api/link/file/<id>``: one kept upload's bytes (the owner's or eve's), so the app opens a picture or file
  full size. An image or PDF is served inline with its own ``Content-Type``; anything else is
  ``Content-Disposition: attachment``, and an active type (HTML, SVG, XML, script) is ``application/octet-stream``
  (link_uploads.served). The id is the only thing that names it (``link_uploads.ID_RE``, 32 hex); 404
  for anything else, never stored, or expired.
* ``GET /api/link/apps`` (the owner's built apps, apps_maker.AppStore) and ``POST /api/link/apps/<slug>/open``:
  the app's page at the tunnel's origin with a fresh app token (miniapp.MiniAppServer.app_link).
* ``GET /api/link/controls`` and ``POST /api/link/controls/<lights|spotify|firetv>`` (owner, 2026-10-08: the Work
  tab's Apps are eve's own controls, "like the spotify, lights and remote"): the first says which of the Mini
  App's Lights, Music and TV remote cards this Mac has (``{"lights": [listing], "spotify": bool, "firetv":
  bool}``, the lights' names and rooms as ``/api/me`` gives them); the second is that card's own ``panel(body)``
  (lights.Lights, spotify.Spotify, firetv.FireTV), the same objects the Mini App's ``/api/lights``,
  ``/api/spotify`` and ``/api/firetv`` call, read off its server on every request, and the same JSON back. 404
  for a control this Mac does not have.
* ``POST /api/link/controls/pika`` and ``GET /api/link/controls/pika/stream`` (pika.py, docs/pika.md; owner,
  2026-10-10: "can we build a controller for pika as an app"): hold-to-drive for the owner's robot car, and its
  video as multipart JPEGs, one viewer. Listed only with EVE_PIKA=1 and its config; 404 otherwise.
* ``POST /api/link/controls/obsbot`` and ``GET /api/link/controls/obsbot/stream`` (obsbot_panel.py, docs/obsbot.md;
  owner, 2026-10-11: "build in an app into eve for obsbot"): the OBSBOT webcam through OBSBOT Center's OSC, with
  hold-to-turn arrows the way Pika drives, and its picture in Pika's multipart format. Listed while OBSBOT Center
  is installed and EVE_OBSBOT isn't 0.
* ``POST /api/link/location`` (phone_location.py; owner, 2026-10-12: "give access to live phone location through
  app"): ``{"lat", "lon", "accuracy_m", "at", "place", "speed_mps"}``, the phone's latest fix, kept as the only one
  (no history) for eve's ``phone_location`` tool; ``{"ok": true}``. 400 for a body that is not a fix, 403 while
  ``EVE_PHONE_LOCATION`` is 0. A fix older than the one kept is ignored. Logged as its age and accuracy only.
* ``GET /api/link/today`` (link_today.py, docs/widget.md): the home-screen widget's one read. What needs the owner
  and what is going (the chief's open cards and the watches), the calendar's events left today and what plays now
  (Spotify, the TV), each source within its own short limit; one that is off or fails is left out and named in
  ``sources``, never failing the answer.
* ``GET /api/link/updates``, ``POST /api/link/updates/dismiss`` ``{"id"}`` and ``POST /api/link/updates/refresh``
  (updates.py, docs/updates.md; owner, 2026-10-12: "find actionable steps from our emails notes everything we have and
  suggest things"): the Updates pane's cards, each one sentence (a verb and the rest), made in the background from
  the owner's mail, calendar, notes, journal and the chief's cards. Dismiss hides every card citing the dismissed
  card's sources for 30 days; refresh starts one rebuild and never waits for the model (``started: false`` while one
  runs). With ``EVE_UPDATES=0`` the list is empty and nothing is read.

* ``GET /api/link/sessions``, ``POST /api/link/sessions/start`` ``{"task", "client_id"?}``, ``GET
  /api/link/sessions/<id>``, ``POST /api/link/sessions/<id>/reply`` ``{"text"}`` and ``POST .../<id>/stop``
  (work_sessions.py, docs/work-sessions.md; owner, 2026-10-12: "make a folder called work under personal where this
  happens"): the Work tab's Claude Code sessions in ``~/Documents/personal/work``. Start opens one Terminal window
  running ``claude`` with the task as its first prompt, never through a shell string; detail is the transcript's
  latest turns, words only, secrets masked; reply types into that session's own tab only while it runs; stop ends
  it. 503 while ``EVE_WORK_SESSIONS=0`` (``work`` is None).

**Who may call.** The link token (stick_link.LinkToken), as ``Authorization: Bearer``, checked in constant time.
A browser sends an Origin on every cross-site fetch and the app's URLSession sends none, so any Origin is refused
(403) before the token is even read: a web page cannot reach this door, whatever it holds. The token never reaches
the app's JavaScript; the page asks native code, which calls here.

**Where the answer goes.** Everything an app turn makes eve say goes to the app's open stream and is not posted
to Telegram (TelegramInlet._to_app): the owner asked in the app, so the answer is in the app, and a second copy in
Telegram is noise. The transcript keeps it either way, marked ``via: "app"``. When no stream is reading (the phone
locked mid-turn, or a task the turn started finishes minutes later), the line goes to Telegram as before, so
nothing is lost and the owner still gets a notification; an OpenUI Lang program in it becomes one plain line there
(``strip_openui``), as it does in the history of a later Telegram, voice or call turn; a later app turn reads the
program as written (``history_text``), so a follow-up has what the block held. Photos and files reach the app as
transcript lines (above) and still go through the Bot API as before (Telegram, or the notifier when it is muted). Web
App buttons and a task's progress card (its Stop button is edited in place) stay in Telegram: the app has no channel
for them yet. The SAY buttons under eve's own messages (take_on's Change and Drop, a card's buttons) reach the app
as the words that answer them, as a picker's do (TelegramInlet.tell_owner, ``_send_choices``).

**Generative UI.** For an app turn the brain's instructions also carry when to use OpenUI (``APP_RULE``, first, with
one small example), the openui-lang reference (``openui_prompt.txt``, written by
ios/EveLink/Web/scripts/gen-prompt.mjs from the library the app renders with, trimmed to the components a phone chat
uses) and a one-line reminder (``APP_LINE``); together, ``app_block``. The texting style that asks for plain text
and no lists is swapped for one that leaves the form to that rule (telegram.APP_STYLE). Telegram turns are unchanged.

Logs carry counts and ids, never what was said.
"""
from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import re
import secrets
import time
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any, Callable, Collection, Optional
from urllib.parse import parse_qsl

from . import memory_sources

log = logging.getLogger(__name__)

TURN_PATH = "/api/link/turn"
MESSAGES_PATH = "/api/link/messages"
JOBS_PATH = "/api/link/jobs"
APPS_PATH = "/api/link/apps"
UPLOAD_PATH = "/api/link/upload"
JOB_ACTION_RE = re.compile(r"^/api/link/jobs/(c\d{1,6})/([a-z]{1,16})$")
APP_OPEN_RE = re.compile(r"^/api/link/apps/([A-Za-z0-9_-]{1,80})/open$")
CONTROLS_PATH = "/api/link/controls"
CONTROL_RE = re.compile(r"^/api/link/controls/([a-z]{1,16})$")
CONTROLS = ("lights", "spotify", "firetv", "memory", "notes", "pika", "obsbot")  # the Mini App server's attributes, each with ``panel(body)``
PIKA_STREAM_PATH = "/api/link/controls/pika/stream"   # pika.py: the car's video, one viewer
OBSBOT_STREAM_PATH = "/api/link/controls/obsbot/stream"   # obsbot_panel.py: the webcam's picture, one viewer
TODAY_PATH = "/api/link/today"           # link_today.py: the home-screen widget's one read
UPDATES_PATH = "/api/link/updates"       # updates.py: the Updates pane's cards
UPDATES_DISMISS_PATH = "/api/link/updates/dismiss"
UPDATES_REFRESH_PATH = "/api/link/updates/refresh"
LOCATION_PATH = "/api/link/location"     # phone_location.py: the phone's latest fix
SESSIONS_PATH = "/api/link/sessions"     # work_sessions.py: the Work tab's Claude Code sessions
SESSION_START_PATH = "/api/link/sessions/start"
SESSION_RE = re.compile(r"^/api/link/sessions/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})"
                        r"(?:/(reply|stop))?$")
# one kept upload's bytes, full size; anything after the prefix that is not 32 hex is a 404 (never a path)
FILE_RE = re.compile(r"^/api/link/file/([^/]{0,64})$")
PATHS = (TURN_PATH, MESSAGES_PATH, JOBS_PATH, APPS_PATH, UPLOAD_PATH, CONTROLS_PATH, TODAY_PATH, PIKA_STREAM_PATH,
         OBSBOT_STREAM_PATH, UPDATES_PATH, UPDATES_DISMISS_PATH, UPDATES_REFRESH_PATH, LOCATION_PATH, SESSIONS_PATH,
         SESSION_START_PATH)
POST_PATHS = (TURN_PATH, UPLOAD_PATH, UPDATES_DISMISS_PATH, UPDATES_REFRESH_PATH, LOCATION_PATH, SESSION_START_PATH)
MAX_CARD_ID_CHARS = 64                # a dismissed Updates card's id (updates.card_id: 12 hex)
GRADES = ("auto", "instant", "think")    # telegram.GRADES: the speed dial
JOB_ACTIONS = ("go", "done", "drop", "raise", "happened", "didnt", "hide")
HIDDEN_JOBS_FILE = "~/.config/eve-bridge/chat/hidden-jobs.json"   # the finished cards the owner removed from Work
FINISHED_SHOWN = 20                   # finished cards listed after the open ones
MAX_GOAL_CHARS = 1000                 # a New job's goal, after trimming
MAX_DETAIL_CHARS = 2000               # and its details
# What a New job's words are led by when heard: the Work tab's button said it, so the brain hears it. No word in it
# is consequential (chief_card.verbs), so it never grounds a one-way step the owner did not name (test_link_chat).
JOB_LEAD = "Take this on as a job: "
JOB_DETAIL_LEAD = "Details: "
JOB_SECS = 90.0                       # the longest a New job waits for its turn (the app waits 120 s)
JOB_GRACE_SECS = 5.0                  # once the card is filed, how long the turn gets to finish its backbrief
MAX_TEXT_CHARS = 4000                 # one message from the app, after trimming
MAX_CLIENT_ID_CHARS = 64
MAX_ATTACHMENTS = 4                   # attachments on one turn
# Photos and PDFs go to the model inside the request (base64, a third larger), so their sum on one turn is capped:
# past it the turn is refused with a line the composer shows, rather than the model call failing (review, 2026-10-08).
MAX_INLINE_BYTES = 20 * 1024 * 1024
INLINE_KINDS = ("image", "pdf")
UPLOAD_SECS = 180.0                   # to read an upload's body (20 MB through the tunnel on a phone's uplink)
MAX_BODY_BYTES = 64_000               # 4000 characters of any script, JSON-escaped, fit with room to spare
BODY_SECS = 30.0                      # to read a request's body
TURN_SECS = 300.0                     # a turn's stream ends after this, done or not
PING_SECS = 15.0                      # ": ping" this long into a quiet stream (the tunnel drops idle streams)
SETTLE_SECS = 0.2                     # how often a stream checks whether the owner's turn is still in flight
DEDUPE_SECS = 600.0                   # the same client_id within this is the same turn
MAX_KEPT_TURNS = 256                  # remembered turns, for dedupe; the oldest finished ones go first
HISTORY_LIMIT = 100                   # lines returned without a cursor
HISTORY_DAYS = 3                      # transcript days read without a cursor
PAGE_LIMIT = 200                      # lines returned after a cursor; the next poll takes the rest
WAIT_MAX_SECS = 25.0                  # the longest long poll (the tunnel and URLSession both outlast it)
CLOSE_SETTLE_SECS = 0.5               # on close, how long open streams get to write their end
POLL_SECS = 2.0                       # a long poll re-reads the files this often, woken or not
DISCARD_MAX_BYTES = 1024 * 1024        # a refused body read and dropped before the refusal, at most this much
DISCARD_SECS = 2.0                    # and for at most this long
ROLES = ("owner", "eve")
KINDS = ("say", "tool", "image", "media")    # "media": a picture or file eve sent (TelegramInlet._send_media)
VIAS = ("app", "telegram", "voice", "other")
CURSOR_RE = re.compile(r"^(\d{4}-\d{2}-\d{2}):(-1|\d{1,9})$")
REACTION_VERSION_RE = re.compile(r"^\d{1,16}$")         # a cursor's "~<version>" (link_reactions.Reactions.version)
TURN_ID_RE = re.compile(r"^[A-Za-z0-9_-]{8,64}$")        # a client_id that is also a good turn id
REASONS = {200: "OK", 400: "Bad Request", 401: "Unauthorized", 403: "Forbidden", 404: "Not Found",
           405: "Method Not Allowed", 409: "Conflict", 411: "Length Required", 413: "Payload Too Large",
           415: "Unsupported Media Type", 500: "Internal Server Error", 503: "Service Unavailable"}
NO_CHAT = "eve's chat isn't running, or no owner has texted it yet"
NO_CHIEF = "eve's chief of staff is off on this Mac (EVE_CHIEF), so there are no jobs"
NO_APPS = "the app builder is off on this Mac, so there are no apps"
NO_CONTROL = {"lights": "eve has no lights on this computer.",
              "spotify": "eve has no Spotify login on this computer.",
              "firetv": "eve has no TV on this computer.",
              "memory": "eve's memory is off on this Mac (EVE_MEMORY).",
              "notes": "Your notes are off on this Mac (EVE_SECOND_BRAIN).",
              "pika": "Pika isn't set up on this Mac (EVE_PIKA and pika.json).",
              "obsbot": "The camera is off on this Mac (no OBSBOT camera is plugged in, or EVE_OBSBOT=0)."}
NO_MEMORY = "eve's memory is off on this Mac (EVE_MEMORY), so there is no conversation to read"
NO_LOCATION = "phone location is off on this Mac (EVE_PHONE_LOCATION=0)"
NO_WORK = "Work sessions are off on this Mac"

PROMPT_PATH = Path(__file__).with_name("openui_prompt.txt")
# What an app turn's instructions end with (app_block), in this order: APP_RULE, the openui-lang reference
# (PROMPT_PATH), APP_LINE. The rule is first because it decides whether the reference is used at all: with the rule as
# one sentence after 54,000 characters of signatures (until 2026-10-08), 1 of 24 app replies in a week used OpenUI.
# GPT-6 models follow format instructions literally and stall on conflicting ones (OpenAI's GPT-6 guide), so the rule
# is a plain two-way decision, the texting style that forbade lists is swapped out for app turns
# (telegram.APP_STYLE), and the example below is one small program that the app's own parser accepts
# (tests/test_openui_eval.py runs ios/EveLink/Web/scripts/validate.mjs on it). Every component it names is in the
# reference (ios/EveLink/Web/scripts/gen-prompt.mjs, CHAT_COMPONENTS).
APP_EXAMPLE = """Here's how they compare.
```openui-lang
root = Card([header, table, next])
header = CardHeader("Tea or coffee", "One cup")
table = Table([Col("Drink", ["Tea", "Coffee"]), Col("Caffeine (mg)", [47, 95], "number")])
next = FollowUpBlock([f1, f2])
f1 = FollowUpItem("Which is better in the evening?")
f2 = FollowUpItem("What about green tea?")
```"""
APP_RULE = (
    "## Replying in the Eve Link app\n\n"
    "This message came from the Eve Link app on the owner's iPhone. The app shows plain text as Markdown and "
    "draws an openui-lang block as native cards, tables, charts and buttons.\n\n"
    "Before you write, decide the form of the reply:\n"
    "- Write an openui-lang block when the answer has a shape the owner will scan: a comparison of two or more "
    "things, options to pick from, steps to follow, a plan or schedule by time or day, numbers that make a chart, "
    "or a question the owner can answer with a tap.\n"
    "- Write plain text, one to three short sentences, for everything else: chat, a greeting, thanks, feelings, a "
    "single fact or number, a yes or no, a confirmation that you started something, or a question back to the "
    "owner.\n"
    "In the app, a table, a list of options or a set of steps goes in the openui-lang block rather than in "
    "Markdown. When the owner asks how to do something or for a plan, give the usual answer in the block first; "
    "if a detail would change it, ask about that in one short sentence after the block. You will see the block in "
    "the next turn, so the owner's answer can change it.\n\n"
    "The block: at most one short sentence of plain text before it, then the program in a fence that opens with "
    "```openui-lang and closes with ```. The program starts with root = Card(...) and stays small enough for a "
    "phone screen. A tool's answer can be shown this way too: a list of events, results or prices is a table or "
    "a list.\n\n"
    "A complete reply with a block:\n" + APP_EXAMPLE)
APP_LINE = ("This turn is from the Eve Link app: use one openui-lang block when a comparison, options, steps, a "
            "schedule, a chart or tap choices make the answer easier to scan, and one to three short plain sentences "
            "otherwise.")


# ---- the generative UI prompt ----------------------------------------------------------------------------

_prompt: dict[str, Any] = {"key": None, "text": "", "warned": False}


def app_block(path: Optional[Path] = None) -> str:
    """What an app turn's instructions end with: ``APP_RULE`` (when to use OpenUI, with one example), the
    openui-lang reference (``PROMPT_PATH``) and ``APP_LINE``. The file is read once per change (mtime and size). An
    absent or empty file means no addition at all, logged once: the brain then answers in plain Markdown, which
    the app also shows, and is never told to write a language it has no reference for."""
    path = path or PROMPT_PATH
    try:
        st = path.stat()
        key = (str(path), st.st_mtime_ns, st.st_size)
        if _prompt["key"] != key:
            _prompt["key"], _prompt["text"] = key, path.read_text(encoding="utf-8").strip()
    except OSError:
        _prompt["key"], _prompt["text"] = None, ""
    if not _prompt["text"]:
        if not _prompt["warned"]:
            _prompt["warned"] = True
            log.warning("link chat: %s is missing or empty; app turns get no OpenUI guide "
                        "(run ios/EveLink/Web/scripts/gen-prompt.mjs)", path.name)
        return ""
    return APP_RULE + "\n\n" + _prompt["text"] + "\n\n" + APP_LINE


OPENUI_PLACEHOLDER = "(A table or chart is here: open Eve Link to see it.)"
_OPENUI_FENCE = re.compile(r"```openui-lang[ \t]*\n.*?(?:\n```|\Z)", re.DOTALL)
_OPENUI_BARE_LINE = re.compile(r"(?:^|\n)[ \t]*root[ \t]*=[ \t]*[A-Z][A-Za-z0-9]*\(")   # replyParts.ts ROOT_RE


def strip_openui(text: str) -> str:
    """``text`` with each OpenUI Lang program (a ```openui-lang fence, or a whole reply that is a bare ``root =``
    program, as the app's replyParts.ts reads them) swapped for one plain line. For an app turn's line that goes
    to Telegram after all (nobody reading in the app), and for eve's lines in the history of a Telegram, voice or
    call turn (``history_text``), so Telegram never shows a raw program and those turns never read past replies in
    OpenUI Lang and copy the format. An unclosed fence runs to the end, as the app renders it."""
    # a program at the start, or after some prose, as replyParts.ts draws it (`root = Card(` at a line start, no ``` before it):
    # the prose stays, the program goes (review, 2026-10-08: it reached Telegram and voice history unstripped)
    at = _OPENUI_BARE_LINE.search(text)
    if at and "```" not in text[: at.start() + 1]:
        before = text[: at.start()].rstrip()
        return f"{before}\n{OPENUI_PLACEHOLDER}".strip() if before else OPENUI_PLACEHOLDER
    if "```openui-lang" not in text:
        return text
    return _OPENUI_FENCE.sub(OPENUI_PLACEHOLDER, text).strip()


# An eve line the chat log holds from before app history kept programs (2026-10-08): the placeholder is all that is
# left of its block. An app turn is shown this instead, since "open Eve Link" makes no sense to a turn in the app.
OPENUI_GONE = "(A card was shown here; what it held is no longer in this history.)"


def history_text(text: str, app: bool) -> str:
    """One of eve's lines as the model's history shows it. The chat log keeps the reply whole, programs included.
    An app turn (``app``) reads it as written, so a follow-up ("make it 4 days", "the second one", a tapped
    button's words) has the block's titles, rows and options to work from. A Telegram, voice or call turn reads
    each program as ``OPENUI_PLACEHOLDER`` (``strip_openui``): those channels cannot draw it, and a reply there
    must not copy the format."""
    if not app:
        return strip_openui(text)
    return text.replace(OPENUI_PLACEHOLDER, OPENUI_GONE) if OPENUI_PLACEHOLDER in text else text


# ---- the conversation, read back from the transcript -------------------------------------------------------

def _row(day: str, i: int, line: dict[str, Any]) -> Optional[dict[str, Any]]:
    """One transcript line as the app's message, or None for a line the app does not show: the system's markers, a
    command word, a line sent over the memory bus, and a forgotten line.

    "claude on" relays the owner's words to a Claude Code session and its answers back (``kind: "relay"``). They
    used to show only in Telegram; with Telegram app-only (2026-10-08) the app is the one place left, so the owner's
    relayed line shows as theirs and Claude's answers as eve's (owner: "i had claude on ... why didnt it get
    relayed")."""
    from .transcripts import FORGOTTEN, TALK

    who, kind, ch = line.get("who"), line.get("kind"), line.get("ch")
    relayed = kind == "relay" and who in ("owner", "claude")
    if relayed:
        who, kind = ("eve" if who == "claude" else "owner"), "say"
    if who not in ROLES or kind not in KINDS or ch not in TALK or line.get("text") == FORGOTTEN:
        return None
    try:
        t = datetime.fromisoformat(str(line["ts"])).timestamp()
    except (KeyError, TypeError, ValueError):
        return None
    via = line.get("via")
    if relayed and who == "eve":
        via = "other"                          # Claude Code's words, not eve's own turn
    elif via not in VIAS:
        via = {"voice": "voice", "telegram": "telegram"}.get(str(ch), "other")
    text = str(line.get("text") or "")
    att = _att(line.get("att"))
    if who == "owner" and att:
        # The history's note for the attachments ("[image attached]", telegram._attached), which the relay keeps
        # in the line: the app shows the attachments themselves, and matches its own bubble by the words alone,
        # so the note made a second copy of every photo sent while the relay was on (owner, 2026-10-09).
        text = ATTACHED_NOTE.sub("", text)
    row = {"id": f"{day}:{i}", "role": who, "text": text, "t": t, "via": via, "kind": kind}
    mid = line.get("mid")
    if who == "owner" and isinstance(mid, str) and TURN_ID_RE.match(mid):
        row["rid"] = mid                                 # its reaction's key (link_reactions.py)
    if att:
        row["attachments"] = att
    if who == "eve":
        sources = memory_sources.sanitize(line.get("src"))
        if sources:
            row["sources"] = sources                     # the memory lines the reply leaned on (memory_sources.py)
    return row


# telegram._attached's note at the end of an owner line: "[image attached]", "[2 images, file a.pdf attached]".
ATTACHED_NOTE = re.compile(r"\s*\[(?:\d+ images|image|file [^\[\]]+)(?:, (?:\d+ images|image|file [^\[\]]+))* attached\]\Z")

ATT_KINDS = ("image", "pdf", "text", "doc", "file")      # link_uploads.KINDS; "file" is eve's own only


def att_record(uploads: list[Any]) -> list[dict[str, str]]:
    """What the owner's transcript line keeps of a turn's attachments (link_uploads.Upload): id, name and kind."""
    return [{"id": u.id, "name": u.name, "kind": u.kind} for u in uploads]


def _att(raw: Any) -> list[dict[str, str]]:
    """A transcript line's ``att``, as the app may show it: well-formed entries only, at most MAX_ATTACHMENTS."""
    from .link_uploads import ID_RE, MAX_NAME_CHARS

    if not isinstance(raw, list):
        return []
    out = []
    for a in raw[:MAX_ATTACHMENTS]:
        if (isinstance(a, dict) and isinstance(a.get("id"), str) and ID_RE.match(a["id"])
                and a.get("kind") in ATT_KINDS and isinstance(a.get("name"), str)):
            out.append({"id": a["id"], "name": a["name"][:MAX_NAME_CHARS], "kind": a["kind"]})
    return out


def read_messages(store: Any, after: Optional[str] = None) -> tuple[list[dict[str, Any]], str]:
    """(messages oldest first, cursor). A message's id and the cursor are ``<day>:<line index>``: a day's file is
    only ever appended to (a forget rewrites a line in place), so a position stays the same line. The cursor is
    the last line looked at, shown or not, so the next poll starts after it, across the 04:00 day boundary too.
    Blocking (file reads, cached by transcripts.py): call it in a thread."""
    days = store.days()
    if after is None:
        rows: list[dict[str, Any]] = []
        cursor = f"{store.day_of(store.now())}:-1"
        for day in days[-HISTORY_DAYS:]:
            lines = store.lines(day)
            rows.extend(r for r in (_row(day, i, ln) for i, ln in enumerate(lines)) if r is not None)
            if lines:
                cursor = f"{day}:{len(lines) - 1}"
        return rows[-HISTORY_LIMIT:], cursor
    m = CURSOR_RE.match(after)
    if m is None:
        raise ValueError("bad cursor")
    since, last = m.group(1), int(m.group(2))
    rows, cursor = [], after
    for day in (d for d in days if d >= since):
        lines = store.lines(day)
        for i in range(last + 1 if day == since else 0, len(lines)):
            cursor = f"{day}:{i}"
            row = _row(day, i, lines[i])
            if row is not None:
                rows.append(row)
                if len(rows) >= PAGE_LIMIT:
                    return rows, cursor
    return rows, cursor


def split_cursor(after: Optional[str]) -> tuple[Optional[str], Optional[int]]:
    """A cursor as the app sends it back → (the transcript position, the reaction version it saw or None). Raises
    ValueError for one this door never made."""
    if after is None:
        return None, None
    base, tilde, version = after.partition("~")
    if CURSOR_RE.match(base) is None or (tilde and REACTION_VERSION_RE.match(version) is None):
        raise ValueError("bad cursor")
    return base, int(version) if tilde else None


def _length(headers: dict[str, str]) -> int:
    """The request's Content-Length, -1 when it is not a number."""
    try:
        return int(headers.get("content-length") or 0)
    except ValueError:
        return -1


def turn_id(client_id: str, taken: Any = ()) -> str:
    """An app turn's id: the app's ``client_id`` when it is a plain id not in use (the page then knows which of its
    bubbles a reaction is for), else a random one."""
    if TURN_ID_RE.match(client_id or "") and client_id not in taken:
        return client_id
    return secrets.token_hex(8)


# ---- one turn's stream ------------------------------------------------------------------------------------

@dataclass(eq=False)
class _Turn:
    """One app turn: eve's lines for it, in order, and whether it is over. ``readers``: streams writing it now
    (two when the phone retried with the same client_id); a line nobody reads goes to Telegram (``deliver``).
    ``asked``: the turn asked the owner something and waits; the owner's answer comes as a new turn, whose
    stream carries the rest of this one's reply."""
    id: str
    client_id: str
    born: float
    chunks: list[str] = field(default_factory=list)
    done: bool = False
    asked: bool = False
    readers: int = 0
    changed: asyncio.Event = field(default_factory=asyncio.Event)

    def poke(self) -> None:
        """Wake every stream writing this turn. A fresh event each time, so no waiter misses a change."""
        old, self.changed = self.changed, asyncio.Event()
        old.set()


def sse(content: str) -> bytes:
    """One OpenAI Chat Completions streaming chunk carrying ``content``, framed as one SSE event."""
    body = {"choices": [{"index": 0, "delta": {"content": content}}]}
    return b"data: " + json.dumps(body).encode() + b"\n\n"


DONE = b"data: [DONE]\n\n"
PING = b": ping\n\n"
STREAM_HEAD = (b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n"
               b"X-Accel-Buffering: no\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")


async def _chunk(writer: asyncio.StreamWriter, data: bytes) -> None:
    """One HTTP/1.1 chunk (the stream's length is not known up front), flushed now: the tunnel passes each on."""
    writer.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
    await writer.drain()


# ---- the chief's cards, for the Work view ------------------------------------------------------------------

_RUNNING = {"research": "Researching", "assess": "Weighing the options", "act": "Doing it", "watch": "Watching"}
_WAITING = {"go": "Waiting on your Go", "budget": "Waiting on a budget raise", "question": "Waiting on your answer",
            "mac_slot": "Waiting for the Mac"}


def _go_ready(card: Any) -> bool:
    """A Go would approve something now (Chief.approve's own conditions): the first queued one-way step, every
    step before it done, no yes on it yet."""
    phase = next((p for p in card.phases if p.status == "queued" and p.door == "one_way"), None)
    return (phase is not None and phase.approval is None
            and all(p.status == "ok" for p in card.phases if p.n < phase.n))


def job_needs_you(chief: Any, card: Any) -> bool:
    """The owner's move: a Go, a budget, an answer, a receipt not confirmed, or an act that may have happened."""
    if card.terminal:
        return False
    return ((card.status == "waiting" and card.waiting_for in ("go", "budget", "question"))
            or card.status == "unverified" or chief._doubtful(card) is not None)


def job_actions(chief: Any, card: Any) -> list[str]:
    """The subset of JOB_ACTIONS that does something to this card now, the way Chief.choices picks its buttons."""
    if card.terminal:
        return ["hide"]
    out = []
    if chief._doubtful(card) is not None:
        # the act may already have happened: never a one-tap Go (that would run it twice), only the answer
        # Telegram's buttons ask for (Chief.choices), or a drop
        return ["happened", "didnt", "drop"]
    if _go_ready(card):
        out.append("go")
    if card.status == "unverified" or card.kind == "commitment":
        out.append("done")
    if card.status != "unverified":
        out.append("drop")
    if card.status == "waiting" and card.waiting_for == "budget":
        out.append("raise")
    return out


def job_line(chief: Any, card: Any) -> str:
    """One muted status line, composed by code: "Waiting on your Go", "Researching", "Done"."""
    if card.status == "done":
        return "Done"
    if card.status == "dropped":
        return "Dropped"
    if card.status == "unverified":
        return "Not confirmed: did it go through?"
    if chief._doubtful(card) is not None:
        return "May have happened: check"
    if card.status == "waiting":
        return _WAITING.get(card.waiting_for or "", "Waiting")
    if card.status == "proposed":
        return "Proposed"
    running = next((p for p in card.phases if p.status == "running"), None)
    if running is not None:
        return _RUNNING.get(running.do, "Working")
    if card.kind == "commitment" and card.cue is not None and card.cue.at:
        return f"Reminder {card.cue.at}"
    return "Up next" if any(p.status == "queued" for p in card.phases) else "Working"


def job_entry(chief: Any, card: Any) -> dict[str, Any]:
    """One card for ``GET /api/link/jobs``: ``Chief._as_data`` and what the app needs to draw and act on it."""
    return {**chief._as_data(card), "rev": card.rev, "updated": card.updated, "line": job_line(chief, card),
            "steps": len(card.phases), "steps_done": sum(1 for p in card.phases if p.status == "ok"),
            "needs_you": job_needs_you(chief, card), "actions": job_actions(chief, card)}


def list_jobs(chief: Any, hidden: Collection[str] = ()) -> dict[str, Any]:
    """Open cards first (the ones that need the owner first, then the newest), then the last ``FINISHED_SHOWN``
    finished, newest first, without the ones the owner removed (``hidden``); ``needs_you`` counts the cards that
    need the owner."""
    cards = chief.ledger.all()
    open_ = [job_entry(chief, c) for c in cards if not c.terminal]
    open_.sort(key=lambda j: (not j["needs_you"], -j["updated"]))
    finished = sorted((c for c in cards if c.terminal and c.id not in hidden), key=lambda c: c.updated,
                      reverse=True)[:FINISHED_SHOWN]
    return {"jobs": open_ + [job_entry(chief, c) for c in finished],
            "needs_you": sum(1 for j in open_ if j["needs_you"])}


class HiddenJobs:
    """The finished cards' ids the owner removed from the Work list, kept in one small file (0600, its folder
    0700). Card ids are never reused (chief_card.Card.id), so an id alone names its card for good. A file that
    cannot be read is an empty list: every finished card shows again, nothing is lost."""

    def __init__(self, path: str | Path = HIDDEN_JOBS_FILE) -> None:
        self.path = Path(path).expanduser()
        self.ids: set[str] = set()
        try:
            got = json.loads(self.path.read_text(encoding="utf-8"))
            self.ids = {i for i in got.get("hidden", []) if isinstance(i, str)}
        except FileNotFoundError:
            pass
        except (OSError, ValueError, AttributeError) as e:
            log.warning("link chat: the hidden jobs could not be read (%s); all are shown", type(e).__name__)

    def add(self, cid: str) -> None:
        self.ids.add(cid)
        self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".tmp")
        tmp.write_text(json.dumps({"hidden": sorted(self.ids)}), encoding="utf-8")
        tmp.chmod(0o600)
        tmp.replace(self.path)


# ---- the door ---------------------------------------------------------------------------------------------

class LinkChat:
    """The app's chat door. ``inlet`` returns the Telegram chat's brain (TelegramInlet) or None; it is read on
    every request, so a chat that starts after the Mini App is found. The inlet's ``app_out`` is pointed at
    ``deliver`` and its transcript store's ``listeners`` at this door's wake-up on the first request."""

    def __init__(self, inlet: Callable[[], Any], *, clock: Callable[[], float] = time.monotonic,
                 turn_secs: float = TURN_SECS, ping_secs: float = PING_SECS, settle_secs: float = SETTLE_SECS,
                 poll_secs: float = POLL_SECS, uploads: Any = None, today: Any = None,
                 job_secs: float = JOB_SECS, hidden: Optional[HiddenJobs] = None, updates: Any = None,
                 work: Optional[Callable[[], Any]] = None) -> None:
        from .link_today import Today
        from .link_uploads import UploadStore
        from .updates import Updates

        self.today = today if today is not None else Today()   # link_today.Today: the widget's feed
        # updates.Updates: the Updates pane's cards; the daemon runs its refresh loop (daemon.Daemon.run)
        self.updates = updates if updates is not None else Updates(inlet)
        self._inlet = inlet
        self._work = work                  # work_sessions.WorkSessions, read per request; None: the routes are a 503
        self.uploads = uploads if uploads is not None else UploadStore()   # link_uploads.UploadStore
        self._clock = clock
        self.turn_secs, self.ping_secs, self.settle_secs, self.poll_secs = turn_secs, ping_secs, settle_secs, poll_secs
        self.job_secs = job_secs
        self.hidden = hidden if hidden is not None else HiddenJobs()     # finished cards removed from Work
        self._job_starts: dict[str, _Turn] = {}             # New job client_id -> its turn
        self._turns: dict[str, _Turn] = {}                 # by turn id, oldest first
        self._by_client: dict[str, _Turn] = {}
        self._drivers: set[asyncio.Task] = set()
        self._streams: set[asyncio.Task] = set()           # the requests writing a turn's stream now
        self._appended: Optional[asyncio.Event] = None     # set (and replaced) when a transcript line is written
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._store: Any = None

    # -- wiring --
    def _attach(self, inlet: Any) -> None:
        self._loop = asyncio.get_running_loop()
        if self._appended is None:
            self._appended = asyncio.Event()
        if getattr(inlet, "app_out", None) != self.deliver:
            inlet.app_out = self.deliver
        if getattr(inlet, "link_uploads", self.uploads) is not self.uploads:
            inlet.link_uploads = self.uploads           # eve's own pictures and files land in this door's store
        store = getattr(inlet, "transcripts", None)
        if store is not None and store is not self._store and hasattr(store, "listeners"):
            store.listeners.append(self._on_append)
            self._store = store
        reactions = getattr(inlet, "app_reactions", None)
        if reactions is not None and self._on_append not in reactions.listeners:
            reactions.listeners.append(self._on_append)      # a reaction wakes the long polls as a line does

    def _on_append(self, day: str = "") -> None:
        """A transcript line was written, on whatever thread: wake the long polls on the loop's."""
        loop = self._loop
        if loop is None or loop.is_closed():
            return
        try:
            if asyncio.get_running_loop() is loop:
                self._wake()
                return
        except RuntimeError:
            pass
        with contextlib.suppress(RuntimeError):
            loop.call_soon_threadsafe(self._wake)

    def _wake(self) -> None:
        old, self._appended = self._appended, asyncio.Event()
        if old is not None:
            old.set()

    def close(self) -> None:
        """The server is stopping: every turn ends, and its streams with it."""
        for task in list(self._drivers):
            task.cancel()
        for turn in self._turns.values():
            if not turn.done:
                turn.done = True
                turn.poke()

    async def settle(self, timeout: float = CLOSE_SETTLE_SECS) -> None:
        """After ``close``: let the open streams write their [DONE] and the 0 chunk, for at most ``timeout``, before
        the server cancels their connections. Without it the cancel landed first and the app saw a cut stream on
        every daemon restart (review, 2026-10-08)."""
        streams = [t for t in self._streams if not t.done()]
        if streams:
            await asyncio.wait(streams, timeout=timeout)

    # -- where eve's lines go --
    def deliver(self, turn_id: str, text: str, kind: str = "say") -> bool:
        """One of eve's lines from work app turn ``turn_id`` began (TelegramInlet.app_out) → whether a stream
        is reading it. Its own turn's stream while that is open; once it ended on a question, the newest open
        stream (the owner's answer, sent from the app, whose stream carries the rest of the reply). False: no
        stream will show it, so the inlet sends it to Telegram. An "ask" ends the turn's stream after it."""
        turn = self._turns.get(turn_id)
        if turn is not None and turn.done:
            turn = self._newest_open() if turn.asked else None
        if turn is None:
            return False
        turn.chunks.append(text)
        if kind == "ask":
            turn.asked = True
        turn.poke()
        return turn.readers > 0

    def _newest_open(self) -> Optional[_Turn]:
        return next((t for t in reversed(self._turns.values()) if not t.done), None)

    def _prune(self) -> None:
        now = self._clock()
        for tid, turn in list(self._turns.items()):
            if turn.done and (now - turn.born >= DEDUPE_SECS or len(self._turns) > MAX_KEPT_TURNS):
                del self._turns[tid]
                if turn.client_id and self._by_client.get(turn.client_id) is turn:
                    del self._by_client[turn.client_id]

    # -- HTTP --
    async def serve(self, method: str, path: str, query: str, headers: dict[str, str],
                    reader: asyncio.StreamReader, writer: asyncio.StreamWriter,
                    token_check: Optional[Callable[[str], bool]], *, apps: Any = None) -> None:
        """One request on ``/api/link/*``, its head already read by miniapp.MiniAppServer. ``token_check`` is the
        link token's (stick_link.LinkToken.check), or None before the phone was ever paired. ``apps``: the Mini
        App's server (its ``store`` and ``app_link``), or None."""
        if headers.get("origin"):
            await self._json(writer, 403, {"ok": False, "error": "the Eve Link app only"})
            return
        scheme, _, presented = headers.get("authorization", "").partition(" ")
        if scheme.lower() != "bearer" or token_check is None or not token_check(presented.strip()):
            await self._json(writer, 401, {"ok": False, "error": "the link token is missing or wrong"})
            return
        action = JOB_ACTION_RE.match(path)
        app_open = APP_OPEN_RE.match(path)
        control = CONTROL_RE.match(path)
        file = FILE_RE.match(path)
        session = SESSION_RE.match(path)
        if (path not in PATHS and app_open is None and file is None and session is None
                and (action is None or action.group(2) not in JOB_ACTIONS)
                and (control is None or control.group(1) not in CONTROLS)):
            await self._json(writer, 404, {"ok": False, "error": "not found"})
            return
        if path == JOBS_PATH and method in ("GET", "POST"):
            pass                                            # GET lists the cards; POST starts one (New job)
        elif method != ("POST" if path in POST_PATHS or action or app_open or control
                        or (session is not None and session.group(2)) else "GET"):
            await self._json(writer, 405, {"ok": False, "error": "wrong method"})
            return
        if file is not None:
            await self._file(file.group(1), writer)
            return
        if path in (SESSIONS_PATH, SESSION_START_PATH) or session is not None:
            await self._sessions(path, session, headers, reader, writer)
            return
        if path == JOBS_PATH or action is not None:
            await self._jobs(headers, reader, writer, action, method)
            return
        if path == APPS_PATH or app_open is not None:
            await self._apps(apps, headers, writer, app_open.group(1) if app_open else None)
            return
        if path == UPLOAD_PATH:
            await self._upload(headers, reader, writer)
            return
        if path == CONTROLS_PATH or control is not None:
            await self._controls(apps, headers, reader, writer, control.group(1) if control else None)
            return
        if path in (PIKA_STREAM_PATH, OBSBOT_STREAM_PATH):
            name = "pika" if path == PIKA_STREAM_PATH else "obsbot"
            target = getattr(apps, name, None) if apps is not None else None
            if target is None or not getattr(target, "available", True):
                await self._json(writer, 404, {"ok": False, "line": NO_CONTROL[name]})
            else:
                await target.stream(writer)
            return
        if path == TODAY_PATH:
            await self._json(writer, 200, await self.today.gather(self._inlet(), apps))
            return
        if path in (UPDATES_PATH, UPDATES_DISMISS_PATH, UPDATES_REFRESH_PATH):
            await self._updates(path, headers, reader, writer)
            return
        if path == LOCATION_PATH:
            await self._location(headers, reader, writer)
            return
        inlet = self._inlet()
        if inlet is None or getattr(inlet, "_chat_id", None) is None:
            await self._json(writer, 503, {"ok": False, "error": NO_CHAT})
            return
        self._attach(inlet)
        if path == TURN_PATH:
            await self._turn(inlet, headers, reader, writer)
        else:
            await self._messages(inlet, query, writer)

    async def _turn(self, inlet: Any, headers: dict[str, str], reader: asyncio.StreamReader,
                    writer: asyncio.StreamWriter) -> None:
        try:
            size = int(headers.get("content-length") or 0)
        except ValueError:
            size = -1
        if size <= 0 or size > MAX_BODY_BYTES:
            await self._discard(reader, size)
            await self._json(writer, 413 if size > MAX_BODY_BYTES else 400,
                             {"ok": False, "error": "the body must be JSON of at most 64 kB"})
            return
        try:
            body = json.loads(await asyncio.wait_for(reader.readexactly(size), BODY_SECS))
        except (ValueError, asyncio.TimeoutError, asyncio.IncompleteReadError):
            body = None
        text = body.get("text") if isinstance(body, dict) else None
        client_id = body.get("client_id", "") if isinstance(body, dict) else None
        if client_id is None:
            client_id = ""
        if not isinstance(text, str) or not isinstance(client_id, str):
            await self._json(writer, 400, {"ok": False, "error": "text and client_id must be strings"})
            return
        text = text.strip()
        ids = body.get("attachments", []) if isinstance(body, dict) else []
        if (not isinstance(ids, list) or len(ids) > MAX_ATTACHMENTS
                or not all(isinstance(i, str) for i in ids) or len(set(ids)) != len(ids)):
            await self._json(writer, 400, {"ok": False, "error": f"attachments must be at most {MAX_ATTACHMENTS} "
                                                                  "different upload ids"})
            return
        uploads = [await asyncio.to_thread(self.uploads.get, i) for i in ids]
        if any(u is None for u in uploads):
            await self._json(writer, 400, {"ok": False, "error": "an attachment is unknown or expired: attach it "
                                                                  "again"})
            return
        if sum(u.size for u in uploads if u.kind in INLINE_KINDS) > MAX_INLINE_BYTES:
            await self._json(writer, 413, {"ok": False, "error": "those photos and PDFs are more than "
                                           f"{MAX_INLINE_BYTES // (1024 * 1024)} MB together: send fewer at once"})
            return
        if not (1 if not uploads else 0) <= len(text) <= MAX_TEXT_CHARS:
            await self._json(writer, 400, {"ok": False, "error": f"text must be 1 to {MAX_TEXT_CHARS} characters"})
            return
        if len(client_id) > MAX_CLIENT_ID_CHARS:
            await self._json(writer, 400, {"ok": False, "error": f"client_id is at most {MAX_CLIENT_ID_CHARS}"})
            return
        grade = body.get("grade", "auto") if isinstance(body, dict) else "auto"
        if not isinstance(grade, str) or grade not in GRADES:
            await self._json(writer, 400, {"ok": False, "error": "grade must be auto, instant or think"})
            return
        self._prune()
        turn = self._by_client.get(client_id) if client_id else None
        if turn is not None:
            log.info("link chat: the same client_id again; its turn %s is streamed, not heard twice", turn.id)
            turn.readers += 1
        else:
            turn = _Turn(id=turn_id(client_id, self._turns), client_id=client_id, born=self._clock())
            # This request is the turn's reader before the words are heard: a line said before the stream's first
            # write must go to the app, never to Telegram for want of a reader.
            turn.readers += 1
            self._start(inlet, turn, text, grade, uploads)
        await self._stream(turn, reader, writer)

    def _start(self, inlet: Any, turn: _Turn, text: str, grade: str = "auto", uploads: Optional[list[Any]] = None) -> None:
        self._turns[turn.id] = turn
        if turn.client_id:
            self._by_client[turn.client_id] = turn
        # only with attachments is the keyword passed, so an inlet without it (a test's) is called as before
        extra = {"attachments": uploads} if uploads else {}
        try:
            inlet.hear(text, on_call=False, app=turn.id, grade=grade, **extra)   # typed: answered in the app
        except Exception as e:  # noqa: BLE001 — the type only: a message can quote what was said
            log.warning("link chat: the turn could not start (%s)", type(e).__name__)
            turn.done = True
            return
        log.info("link chat: turn %s started (%d chars, %d attachments, %s)", turn.id, len(text), len(uploads or ()),
                 grade)
        task = asyncio.ensure_future(self._drive(turn, inlet))
        self._drivers.add(task)
        task.add_done_callback(self._drivers.discard)

    async def _drive(self, turn: _Turn, inlet: Any) -> None:
        """End the turn's stream when no turn the owner started is in flight, when it asked the owner something,
        or at ``turn_secs``. Polled every ``settle_secs`` (two set sizes), and woken at once by a new line."""
        busy = getattr(inlet, "owner_busy", None)
        try:
            while not turn.done and not turn.asked and self._clock() - turn.born < self.turn_secs:
                if busy is None or not busy():
                    break
                with contextlib.suppress(asyncio.TimeoutError):
                    await asyncio.wait_for(turn.changed.wait(), self.settle_secs)
        finally:
            turn.done = True
            turn.poke()
            self._wake()                                    # a long poll re-reads ``busy`` now, not at its timeout

    async def _stream(self, turn: _Turn, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        """Write the turn's lines as SSE chunks as they come, then [DONE]. The caller has counted this request
        as one of the turn's readers. A client that hangs up stops counting at once (its EOF), so the rest goes
        to Telegram; the stream itself ends at the first failed write, as a half-closed connection may still be
        reading."""
        reading = True
        me = asyncio.current_task()
        if me is not None:
            self._streams.add(me)
        eof = asyncio.ensure_future(reader.read(1))
        sent = 0
        try:
            writer.write(STREAM_HEAD)
            await writer.drain()
            while True:
                changed = turn.changed
                while sent < len(turn.chunks):
                    text = turn.chunks[sent]
                    await _chunk(writer, sse(text if sent == 0 else "\n\n" + text))
                    sent += 1
                if turn.done:
                    await _chunk(writer, DONE)
                    writer.write(b"0\r\n\r\n")
                    await writer.drain()
                    return
                waiter = asyncio.ensure_future(changed.wait())
                watch = {waiter, eof} if not eof.done() else {waiter}
                try:
                    done, _ = await asyncio.wait(watch, timeout=self.ping_secs, return_when=asyncio.FIRST_COMPLETED)
                finally:
                    waiter.cancel()
                if reading and eof in done and (eof.cancelled() or eof.exception() is not None or not eof.result()):
                    reading = False                       # hung up: what comes next goes to Telegram
                    turn.readers -= 1
                if not done:
                    await _chunk(writer, PING)
        except (ConnectionError, OSError):
            return
        finally:
            eof.cancel()
            if me is not None:
                self._streams.discard(me)
            if reading:
                turn.readers -= 1

    async def _messages(self, inlet: Any, query: str, writer: asyncio.StreamWriter) -> None:
        q = dict(parse_qsl(query, keep_blank_values=True))
        after = q.get("after") or None
        try:
            wait = min(max(float(q.get("wait") or 0), 0.0), WAIT_MAX_SECS)
        except ValueError:
            await self._json(writer, 400, {"ok": False, "error": "wait must be a number of seconds"})
            return
        store = getattr(inlet, "transcripts", None)
        if store is None or not getattr(store, "enabled", False):
            await self._json(writer, 503, {"ok": False, "error": NO_MEMORY})
            return
        try:
            after, seen_version = split_cursor(after)
        except ValueError:
            await self._json(writer, 400, {"ok": False, "error": "bad cursor"})
            return
        reactions = getattr(inlet, "app_reactions", None)

        def stale() -> bool:
            # the app holds an older picture of the reactions than this one: answer now, with the new one
            return reactions is not None and seen_version is not None and reactions.version != seen_version

        loop = asyncio.get_running_loop()
        deadline = loop.time() + (wait if after is not None else 0.0)
        was_busy = self._busy(inlet)
        while True:
            woken = self._appended                          # taken before the read: no append is missed
            rows, cursor = await asyncio.to_thread(read_messages, store, after)
            left = deadline - loop.time()
            # A change of ``busy`` also ends the wait: eve's last line is written while the turn is still in
            # flight, so without this the app's "replying" dots stayed up until the poll's 25 s ran out.
            if rows or left <= 0 or woken is None or self._busy(inlet) != was_busy or stale():
                break
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(woken.wait(), min(left, self.poll_secs))
        await asyncio.to_thread(self._thumbs, rows)
        payload: dict[str, Any] = {"messages": rows, "cursor": cursor, "busy": self._busy(inlet)}
        if reactions is not None:
            version, kept = reactions.version, reactions.snapshot()
            for row in rows:
                if row.get("rid") in kept:
                    row["reaction"] = kept[row["rid"]]
            payload["cursor"] = f"{cursor}~{version}"
            payload["reactions"] = kept
        await self._json(writer, 200, payload)

    def _thumbs(self, rows: list[dict[str, Any]]) -> None:
        """Each attachment on the page of rows gets its ``thumb`` (images) and ``gone`` (expired) now. Blocking."""
        for row in rows:
            for a in row.get("attachments", ()):
                alive = self.uploads.get(a["id"]) is not None
                if not alive:
                    a["gone"] = True
                elif a["kind"] == "image":
                    thumb = self.uploads.thumb_url(a["id"])
                    if thumb:
                        a["thumb"] = thumb

    async def _upload(self, headers: dict[str, str], reader: asyncio.StreamReader,
                      writer: asyncio.StreamWriter) -> None:
        """``POST /api/link/upload``: the raw bytes, kept by link_uploads.UploadStore → its id. Needs no chat
        running: the turn that uses it does."""
        from urllib.parse import unquote

        from .link_uploads import MAX_BYTES, UploadError

        if "chunked" in headers.get("transfer-encoding", "").lower():
            await self._json(writer, 411, {"ok": False, "error": "send the upload with a Content-Length"})
            return
        try:
            size = int(headers.get("content-length") or 0)
        except ValueError:
            size = -1
        if size > MAX_BYTES:
            await self._discard(reader, size)
            await self._json(writer, 413, {"ok": False, "error": "that file is too large: 20 MB at most"})
            return
        if size <= 0:
            await self._json(writer, 400, {"ok": False, "error": "the upload is empty"})
            return
        try:
            data = await asyncio.wait_for(reader.readexactly(size), UPLOAD_SECS)
        except (asyncio.TimeoutError, asyncio.IncompleteReadError):
            await self._json(writer, 400, {"ok": False, "error": "the upload did not arrive whole"})
            return
        name = unquote(headers.get("x-filename", ""))[:1000]
        try:
            upload = await asyncio.to_thread(self.uploads.save, data, name)
        except UploadError as exc:
            await self._json(writer, exc.status, {"ok": False, "error": str(exc)})
            return
        except OSError as e:
            log.warning("link chat: an upload could not be kept (%s)", type(e).__name__)
            await self._json(writer, 500, {"ok": False, "error": "eve couldn't keep that file"})
            return
        await self._json(writer, 200, {"ok": True, **upload.public()})

    async def _file(self, upload_id: str, writer: asyncio.StreamWriter) -> None:
        """``GET /api/link/file/<id>``: a kept upload's bytes, for the app to open full size.
        ``UploadStore.get`` takes nothing but 32 hex characters and checks the expiry, so no other file is reachable.
        Needs no chat running."""
        upload = await asyncio.to_thread(self.uploads.get, upload_id)
        try:
            data = await asyncio.to_thread(upload.path.read_bytes) if upload is not None else None
        except OSError:
            data = None
        if upload is None or data is None:
            await self._json(writer, 404, {"ok": False, "error": "that file is gone (kept for a day) or never was"})
            return
        # an image or PDF opens in place; anything else is a download, and an active type (HTML, SVG, script) is
        # never served as itself (link_uploads.served)
        from .link_uploads import served

        mime, disposition = served(upload.kind, upload.mime)
        writer.write(f"HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Disposition: {disposition}\r\n"
                     f"Content-Length: {len(data)}\r\n"
                     "Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n".encode()
                     + data)
        await writer.drain()
        log.info("link chat: file %s sent (%s, %d bytes)", upload.id, upload.kind, len(data))

    async def _jobs(self, headers: dict[str, str], reader: asyncio.StreamReader, writer: asyncio.StreamWriter,
                    action: Optional[re.Match[str]], method: str = "GET") -> None:
        """``GET /api/link/jobs`` or ``POST /api/link/jobs/<id>/<action>``: the chief's cards and its own methods,
        as the Telegram code words call them (telegram.TelegramInlet._chief_command). ``POST /api/link/jobs``: a
        New job (``_new_job``)."""
        inlet = self._inlet()
        chief = getattr(inlet, "chief", None) if inlet is not None else None
        if chief is None:
            if method == "POST":
                await self._discard(reader, _length(headers))
            await self._json(writer, 503, {"ok": False, "error": NO_CHIEF})
            return
        if action is None and method == "POST":
            await self._new_job(inlet, chief, headers, reader, writer)
            return
        if action is None:
            await self._json(writer, 200, list_jobs(chief, self.hidden.ids))
            return
        cid, verb = action.group(1), action.group(2)
        body = await self._optional_body(headers, reader, writer)
        if body is None:
            return
        rev = body.get("rev")
        if rev is not None and (not isinstance(rev, int) or isinstance(rev, bool)):
            await self._json(writer, 400, {"ok": False, "error": "rev must be a whole number"})
            return
        if chief.ledger.get(cid) is None:
            await self._json(writer, 404, {"ok": False, "reason": f"there is no card {cid}"})
            return
        if verb == "go" and chief._doubtful(chief.ledger.get(cid)) is not None:
            # via="id" skips the chief's own in-doubt refusal (typed "go c12" is a deliberate re-run); a tap is not
            await self._json(writer, 200, {"ok": False, "reason": f"{cid} may already have done that; "
                                                                    "say whether it happened first"})
            return
        if verb == "hide":
            if not chief.ledger.get(cid).terminal:
                await self._json(writer, 200, {"ok": False, "reason": f"{cid} is still open; drop it first"})
                return
            try:
                self.hidden.add(cid)
            except OSError as e:
                log.warning("link chat: could not hide %s (%s)", cid, type(e).__name__)
                await self._json(writer, 500, {"ok": False, "reason": "I couldn't remove that just now."})
                return
            log.info("link chat: hide %s from the app: ok", cid)
            await self._json(writer, 200, {"ok": True, "hidden": cid})
            return
        try:
            if verb == "go":
                result = chief.approve(cid, via="id", rev=rev)   # the owner's yes, as "go c12" in Telegram
            elif verb == "done":
                result = await chief.done(cid)
            elif verb == "drop":
                result = chief.drop(cid)
            elif verb == "happened":
                result = await chief.happened(cid)
            elif verb == "didnt":
                result = chief.didnt(cid)
            else:
                result = chief.raise_budget(cid)
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001 — the type only
            log.warning("link chat: the chief's %s on %s failed (%s)", verb, cid, type(e).__name__)
            await self._json(writer, 500, {"ok": False, "reason": "I couldn't do that just now."})
            return
        log.info("link chat: %s %s from the app: %s", verb, cid, "ok" if result.get("ok") else "refused")
        await self._json(writer, 200, result)

    async def _new_job(self, inlet: Any, chief: Any, headers: dict[str, str], reader: asyncio.StreamReader,
                       writer: asyncio.StreamWriter) -> None:
        """``POST /api/link/jobs``: the owner's goal heard as an app turn led by ``JOB_LEAD``; the card is whatever
        that turn's take_on filed. Nothing here parses, files or approves a card: the brain and the chief do, on
        their own path, with every gate they keep (a one-way step still waits on its Go)."""
        body = await self._optional_body(headers, reader, writer)
        if body is None:
            return
        goal, detail, client_id = body.get("goal"), body.get("detail", ""), body.get("client_id", "")
        detail = "" if detail is None else detail
        if not isinstance(goal, str) or not isinstance(detail, str) or not isinstance(client_id, str):
            await self._json(writer, 400, {"ok": False, "error": "goal, detail and client_id must be strings"})
            return
        goal, detail = goal.strip(), detail.strip()
        if not 1 <= len(goal) <= MAX_GOAL_CHARS:
            await self._json(writer, 400, {"ok": False, "error": f"goal must be 1 to {MAX_GOAL_CHARS} characters"})
            return
        if len(detail) > MAX_DETAIL_CHARS:
            await self._json(writer, 400, {"ok": False, "error": f"detail is at most {MAX_DETAIL_CHARS} characters"})
            return
        if len(client_id) > MAX_CLIENT_ID_CHARS:
            await self._json(writer, 400, {"ok": False, "error": f"client_id is at most {MAX_CLIENT_ID_CHARS}"})
            return
        if getattr(inlet, "_chat_id", None) is None:
            await self._json(writer, 503, {"ok": False, "error": NO_CHAT})
            return
        self._attach(inlet)
        self._prune()
        now = self._clock()
        for key, old in list(self._job_starts.items()):
            if old.done and now - old.born >= DEDUPE_SECS:
                del self._job_starts[key]
        kept = self._job_starts.get(client_id) if client_id else None
        if kept is not None:
            log.info("link chat: the same New job again; its turn %s is waited on, not heard twice", kept.id)
            turn = kept
            turn.readers += 1
        else:
            turn = _Turn(id=turn_id("", self._turns), client_id="", born=now)
            # This request reads the turn before the words are heard: its lines come back here, never to Telegram
            # for want of a reader (as _turn does).
            turn.readers += 1
            self._start(inlet, turn, JOB_LEAD + goal + (f"\n\n{JOB_DETAIL_LEAD}{detail}" if detail else ""))
            if turn.done and not turn.chunks:
                turn.readers -= 1
                await self._json(writer, 500, {"ok": False, "reason": "eve couldn't start that just now."})
                return
            if client_id:
                self._job_starts[client_id] = turn
        try:
            card = await self._await_card(inlet, chief, turn)
        finally:
            turn.readers -= 1
        said = [s for s in (strip_openui(c).strip() for c in turn.chunks) if s]
        log.info("link chat: New job, turn %s (%d+%d chars): %s", turn.id, len(goal), len(detail),
                 card.id if card is not None else "no card")
        if card is not None:
            await self._json(writer, 200, {"ok": True, "job": job_entry(chief, card), "said": said})
        elif turn.done:
            await self._json(writer, 200, {"ok": False, "said": said,
                                           "reason": "eve answered without starting a job"})
        else:
            await self._json(writer, 200, {"ok": False, "said": said,
                                           "reason": "eve is still on it; the job shows in Work once it is filed"})

    async def _await_card(self, inlet: Any, chief: Any, turn: _Turn) -> Any:
        """The card this turn's own take_on filed (``TelegramInlet.app_filed``, recorded against the turn's id),
        or None, once the turn is over; the turn gets at most ``job_secs``, and ``JOB_GRACE_SECS`` once the card is
        there (its backbrief is said right after it is filed). A card another turn files meanwhile (a chat turn, a
        second New job) is never this one's."""
        filed = getattr(inlet, "app_filed", None)
        found_at: Optional[float] = None
        while True:
            changed = turn.changed                          # taken before the read: no poke is missed
            cid = filed(turn.id) if filed is not None else None
            card = chief.ledger.get(cid) if cid else None
            now = self._clock()
            if card is not None and found_at is None:
                found_at = now
            if (turn.done or now - turn.born >= self.job_secs
                    or (found_at is not None and now - found_at >= JOB_GRACE_SECS)):
                return card
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(changed.wait(), self.settle_secs)

    async def _optional_body(self, headers: dict[str, str], reader: asyncio.StreamReader,
                             writer: asyncio.StreamWriter) -> Optional[dict[str, Any]]:
        """A request's JSON object body, {} when it has none; None when it was refused (the answer is written)."""
        try:
            size = int(headers.get("content-length") or 0)
        except ValueError:
            size = -1
        if size == 0:
            return {}
        if size < 0 or size > MAX_BODY_BYTES:
            await self._discard(reader, size)
            await self._json(writer, 413 if size > MAX_BODY_BYTES else 400,
                             {"ok": False, "error": "the body must be JSON of at most 64 kB"})
            return None
        try:
            body = json.loads(await asyncio.wait_for(reader.readexactly(size), BODY_SECS))
        except (ValueError, asyncio.TimeoutError, asyncio.IncompleteReadError):
            body = None
        if not isinstance(body, dict):
            await self._json(writer, 400, {"ok": False, "error": "the body must be a JSON object"})
            return None
        return body

    async def _updates(self, path: str, headers: dict[str, str], reader: asyncio.StreamReader,
                       writer: asyncio.StreamWriter) -> None:
        """``GET /api/link/updates``: the cards (updates.Updates.view). ``POST .../dismiss`` ``{"id"}``: that card's
        sources are dismissed for 30 days (404 when the store does not know the id). ``POST .../refresh``: one
        rebuild starts in the background; the answer never waits for it."""
        if path == UPDATES_PATH:
            await self._json(writer, 200, await asyncio.to_thread(self.updates.view))
            return
        body = await self._optional_body(headers, reader, writer)
        if body is None:
            return
        if path == UPDATES_REFRESH_PATH:
            await self._json(writer, 200, {"ok": True, "started": self.updates.refresh()})
            return
        cid = body.get("id")
        if not isinstance(cid, str) or not cid.strip() or len(cid) > MAX_CARD_ID_CHARS:
            await self._json(writer, 400, {"ok": False, "error": "id must be a card's id"})
            return
        if not await asyncio.to_thread(self.updates.dismiss, cid.strip()):
            # an id the store does not know (not now, not among the cards a recent rebuild replaced): nothing was
            # dismissed, so say so; the app puts the card back and reads the list again
            await self._json(writer, 404, {"ok": False, "error": "no such card"})
            return
        await self._json(writer, 200, {"ok": True})

    async def _location(self, headers: dict[str, str], reader: asyncio.StreamReader,
                        writer: asyncio.StreamWriter) -> None:
        """``POST /api/link/location``: the phone's latest fix (phone_location.py). 403 while EVE_PHONE_LOCATION is
        off, 400 for a body that is not a fix; an older fix than the one kept is answered ok and ignored. The log
        says the fix's age and accuracy, never where it is."""
        from . import phone_location

        if not phone_location.enabled():
            await self._discard(reader, _length(headers))
            await self._json(writer, 403, {"ok": False, "error": NO_LOCATION})
            return
        body = await self._optional_body(headers, reader, writer)
        if body is None:
            return
        try:
            fix = phone_location.parse(body)
        except phone_location.BadFix as e:
            await self._json(writer, 400, {"ok": False, "error": str(e)})
            return
        try:
            kept = await asyncio.to_thread(phone_location.store, fix)
        except OSError as e:
            log.warning("link chat: the location was not kept (%s)", type(e).__name__)
            await self._json(writer, 500, {"ok": False, "error": "the Mac could not keep the location"})
            return
        log.info("link chat: location %s (%s)", "updated" if kept else "ignored, not newer",
                 phone_location.describe(fix))
        await self._json(writer, 200, {"ok": True})

    async def _sessions(self, path: str, session: Optional[re.Match[str]], headers: dict[str, str],
                        reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        """The Work tab's Claude Code sessions (work_sessions.py, docs/work-sessions.md): ``GET /api/link/sessions``,
        ``POST .../start`` ``{"task", "client_id"?}``, ``GET .../<id>``, ``POST .../<id>/reply`` ``{"text"}`` and
        ``POST .../<id>/stop``. The token and Origin checks in ``serve`` come first; 503 while they are off."""
        work = self._work() if self._work is not None else None
        if work is None:
            if path == SESSION_START_PATH or (session is not None and session.group(2)):
                await self._discard(reader, _length(headers))     # the body is read and dropped before the 503
            await self._json(writer, 503, {"ok": False, "error": NO_WORK})
            return
        if path == SESSIONS_PATH:
            status, out = await work.list()
        elif path == SESSION_START_PATH:
            body = await self._optional_body(headers, reader, writer)
            if body is None:
                return
            status, out = await work.start(body.get("task"), body.get("client_id", ""))
        else:
            assert session is not None
            sid, verb = session.group(1), session.group(2)
            if verb is None:
                status, out = await work.detail(sid)
            elif verb == "reply":
                body = await self._optional_body(headers, reader, writer)
                if body is None:
                    return
                status, out = await work.reply(sid, body.get("text"))
            else:
                status, out = await work.stop(sid)
        await self._json(writer, status, out)

    async def _apps(self, apps: Any, headers: dict[str, str], writer: asyncio.StreamWriter,
                    slug: Optional[str]) -> None:
        """``GET /api/link/apps``: the owner's built apps, last changed first. ``POST /api/link/apps/<slug>/open``:
        that app's page at the tunnel's origin with a fresh app token; 404 for an app that does not exist."""
        store = getattr(apps, "store", None) if apps is not None else None
        if store is None:
            await self._json(writer, 503, {"ok": False, "error": NO_APPS})
            return
        if slug is None:
            infos = await asyncio.to_thread(store.list)
            await self._json(writer, 200, {"apps": [{"slug": a.slug, "name": a.title, "updated": a.updated}
                                                    for a in infos]})
            return
        if await asyncio.to_thread(store.info, slug) is None:
            await self._json(writer, 404, {"ok": False, "error": "there is no such app"})
            return
        log.info("link chat: app %s opened from the app", slug)
        await self._json(writer, 200, {"url": apps.app_link(slug, headers)})

    async def _controls(self, server: Any, headers: dict[str, str], reader: asyncio.StreamReader,
                        writer: asyncio.StreamWriter, name: Optional[str]) -> None:
        """``GET /api/link/controls``: which of the Lights, Music and TV remote cards this Mac has.
        ``POST /api/link/controls/<name>``: that card's ``panel(body)``, its JSON as it is. The objects are the
        Mini App server's own (set by the daemon), read now, so a card that came or went since is seen."""
        def get(n: str) -> Any:
            return getattr(server, n, None) if server is not None else None

        if name is None:
            lights = get("lights")
            memory = get("memory")
            notes = get("notes")
            await self._json(writer, 200, {"lights": lights.listing() if lights is not None else [],
                                           "spotify": get("spotify") is not None,
                                           "firetv": get("firetv") is not None,
                                           # the Memory card (link_memory.py): its object is always there, its
                                           # memory only while EVE_MEMORY is on and the daemon has built it
                                           "memory": memory is not None and bool(getattr(memory, "available", True)),
                                           # Notes (link_notes.py): while the second brain is on and its vault is there
                                           "notes": notes is not None and bool(getattr(notes, "available", True)),
                                           # Pika, the car (pika.py): only with EVE_PIKA=1 and its config
                                           "pika": get("pika") is not None,
                                           # the OBSBOT webcam (obsbot_panel.py): while OBSBOT Center is installed
                                           "obsbot": get("obsbot") is not None
                                           and bool(getattr(get("obsbot"), "available", True))})
            return
        target = get(name)
        if target is not None and not getattr(target, "available", True):
            target = None
        if target is None:
            await self._json(writer, 404, {"ok": False, "line": NO_CONTROL[name]})
            return
        body = await self._optional_body(headers, reader, writer)
        if body is None:
            return
        try:
            result = await target.panel(body)
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001 — the type only
            log.warning("link chat: the %s control failed (%s)", name, type(e).__name__)
            await self._json(writer, 500, {"ok": False, "line": "That didn't work just now."})
            return
        if body.get("action") not in ("drive", "hold"):     # a held arrow (Pika, the camera) beats every 150 ms
            log.info("link chat: %s %s from the app: %s", name, str(body.get("action") or "status")[:24],
                     "ok" if result.get("ok", True) else "refused")
        await self._json(writer, 200, result)

    @staticmethod
    def _busy(inlet: Any) -> bool:
        """The payload's ``busy``: a turn the owner started is in flight and is not just waiting on the owner's
        answer to a question (no "replying" dots under eve's own question)."""
        busy = getattr(inlet, "owner_busy", None)
        waiting = getattr(inlet, "waiting_on_owner", None)
        return bool(busy and busy()) and not bool(waiting and waiting())

    @staticmethod
    async def _discard(reader: asyncio.StreamReader, size: int) -> None:
        """Read and drop a refused body (at most DISCARD_MAX_BYTES, for DISCARD_SECS) before the refusal is written.
        Answering and closing while the client still sends made the client see "connection reset" instead of the
        413 (a test caught it about once in 35 runs, 2026-10-08). A larger body is still cut off."""
        left = min(max(size, 0), DISCARD_MAX_BYTES)
        try:
            async with asyncio.timeout(DISCARD_SECS):
                while left > 0:
                    chunk = await reader.read(min(left, 65536))
                    if not chunk:
                        return
                    left -= len(chunk)
        except (TimeoutError, OSError):
            return

    @staticmethod
    async def _json(writer: asyncio.StreamWriter, status: int, obj: dict[str, Any]) -> None:
        body = json.dumps(obj).encode()
        writer.write(f"HTTP/1.1 {status} {REASONS.get(status, 'OK')}\r\nContent-Type: application/json\r\n"
                     f"Content-Length: {len(body)}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n".encode()
                     + body)
        await writer.drain()
