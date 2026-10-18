"""Loopback-only learning workspace. Run with python -m eve_bridge.learning --demo."""
from __future__ import annotations

import argparse
import base64
import copy
import errno
import json
import logging
import os
import re
import secrets
import sys
import threading
import time
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

from . import ROBOT_APP_NOT_RUNNING
from .store import Store, default_dir
from .think_aloud import LISTEN_ACTIONS, MAX_SPOKEN_IDEA, listen_refusal
from .tutor import DemoTutor, LiveTutor, live_settings

log = logging.getLogger(__name__)
DEFAULT_PORT = 48766
MAX_BODY = 12 * 1024 * 1024
ASSETS = Path(__file__).parent / "web"
# Fonts are bundled (SIL OFL, see web/fonts/OFL.txt) because the page CSP is same-origin only.
FONTS = ASSETS / "fonts"
FONT_FILE = re.compile(r"[a-z0-9-]+\.woff2")
# The whiteboard is tldraw, built from bridge/web-canvas into web/canvas (see docs/learning.md). Its fonts, icons and
# translations are served from here too, for the same reason as the fonts above.
CANVAS = ASSETS / "canvas"
CANVAS_FILE = re.compile(r"(?:[A-Za-z0-9_-]+/)*[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)*")
CANVAS_MIME = {".js": "text/javascript; charset=utf-8", ".css": "text/css; charset=utf-8", ".svg": "image/svg+xml",
               ".woff2": "font/woff2", ".png": "image/png",
               # Not the bare "application/json": reply() encodes that one itself, and these are files.
               ".json": "application/json; charset=utf-8",
               ".md": "text/plain; charset=utf-8"}
MAX_BOARD = 4 * 1024 * 1024
# Same-origin only. Two allowances exist for the tldraw whiteboard, both as narrow as they can be:
#  - img-src blob: tldraw shows and exports pictures through blob: URLs. Only a script already running in this page
#    can mint one, so it opens nothing to another origin.
#  - a per-page-load nonce in style-src: tldraw adds <style> elements at run time (its licence watermark, and the fonts
#    it embeds in a board picture) and stamps them with the nonce it is given. No 'unsafe-inline': this page puts tutor
#    and search text into innerHTML, and the policy is what stands behind that escaping.
def content_policy(nonce=""):
    style = f"style-src 'self' 'nonce-{nonce}'" if nonce else "style-src 'self'"
    return f"default-src 'self'; img-src 'self' data: blob:; {style}; script-src 'self'; font-src 'self'; frame-ancestors 'none'"


CSP = content_policy()
NONCE_SLOT = b"__EVE_NONCE__"
# Spoken lines kept per lesson so a browser save that missed them can put them back.
SPOKEN_KEEP = 50
LISTEN_OFF = {"available": False, "state": "off", "lesson_id": None}


def validate_work(data):
    result = {}
    for key in ("ideas", "problem"):
        if key in data:
            if not isinstance(data[key], str) or len(data[key]) > 20000:
                raise ValueError("Text is too long.")
            result[key] = data[key]
    if "stuck" in data:
        result["stuck"] = bool(data["stuck"])
    for key in ("source_image", "board_image"):
        if key in data:
            value = data[key]
            if not isinstance(value, str) or len(value) > 6 * 1024 * 1024:
                raise ValueError("Image must be smaller than 4 MB.")
            if value:
                prefix, sep, encoded = value.partition(",")
                if not sep or prefix not in ("data:image/png;base64", "data:image/jpeg;base64", "data:image/webp;base64"):
                    raise ValueError("Use a PNG, JPEG or WebP image.")
                try:
                    raw = base64.b64decode(encoded, validate=True)
                except ValueError:
                    raise ValueError("Invalid image data.") from None
                if not (raw.startswith(b"\x89PNG\r\n\x1a\n") or raw.startswith(b"\xff\xd8\xff") or raw.startswith(b"RIFF")):
                    raise ValueError("Invalid image file.")
            result[key] = value
    if "strokes" in data:
        strokes = data["strokes"]
        if not isinstance(strokes, list) or len(strokes) > 10000:
            raise ValueError("Whiteboard is too large. Start another problem.")
        for stroke in strokes:
            if not isinstance(stroke, dict) or stroke.get("tool") not in ("pen", "eraser", "text"):
                raise ValueError("Invalid drawing stroke.")
            points = stroke.get("points")
            if not isinstance(points, list) or len(points) > 10000:
                raise ValueError("Invalid drawing points.")
            for p in points:
                if not isinstance(p, list) or len(p) != 2 or any(type(v) not in (int, float) or not 0 <= v <= 1 for v in p):
                    raise ValueError("Invalid drawing coordinates.")
            # A text box is a stroke with one anchor point and its words. Pen and eraser strokes carry no text.
            if stroke["tool"] == "text":
                text = stroke.get("text")
                if not isinstance(text, str) or not text.strip() or len(text) > 500:
                    raise ValueError("Invalid text box. Keep it under 500 characters.")
                if len(points) != 1:
                    raise ValueError("Invalid text box position.")
            elif "text" in stroke:
                raise ValueError("Invalid drawing stroke.")
        result["strokes"] = strokes
    if "board" in data:
        # The tldraw document. tldraw validates and migrates its own records when it loads one; the server keeps the
        # shape of the envelope and the size honest.
        board = data["board"]
        if board is not None:
            if (not isinstance(board, dict) or not isinstance(board.get("store"), dict)
                    or not isinstance(board.get("schema"), dict)
                    or any(not isinstance(r, dict) for r in board["store"].values())):
                raise ValueError("Invalid whiteboard.")
            if len(json.dumps(board, ensure_ascii=False)) > MAX_BOARD:
                raise ValueError("Whiteboard is too large. Remove a pasted image, or start another problem.")
        result["board"] = board
    return result


def mark_count(lesson):
    """How much the learner has put on the board: tldraw shapes, plus pen strokes from a lesson saved before tldraw."""
    board = lesson.get("board") or {}
    shapes = sum(1 for r in board.get("store", {}).values() if r.get("typeName") == "shape")
    return shapes + len(lesson.get("strokes", []))


def merge_spoken(ideas, lesson, base):
    """Put back spoken lines saved after revision ``base`` that ``ideas`` does not contain.

    A browser that saves typed ideas can miss a line eve appended a moment earlier. Only lines newer
    than the text the browser last loaded are restored, so a learner can still delete an older line."""
    missing = [e["text"] for e in lesson.get("spoken", []) if e["rev"] > base and e["text"] not in ideas]
    if not missing:
        return ideas
    return "\n".join(filter(None, [ideas.rstrip()] + missing))


class LearningApp:
    def __init__(self, directory=None, demo=False, tutor=None, notify=None, listener=None):
        self.store = Store(directory or default_dir())
        self.tutor = tutor or (DemoTutor() if demo else LiveTutor())
        self.demo = self.tutor.demo
        self.notify = notify
        # The daemon's microphone, for think out loud: an object with start(lesson), stop() and
        # status(). None when this server runs without the daemon.
        self.listener = listener
        self.lock = threading.RLock()
        self.active = None
        self.url = ""

    def _ask_tutor(self, action, s):
        """Call the tutor with the lesson lock released, then return (result, the lesson as it is now).

        A live tutor call takes up to 90 s, and holding the lock across it stalled every autosave and
        every spoken line for that long. The tutor answers from a copy; its answer is applied to the
        lesson re-read under the lock, so a save or a spoken append made meanwhile is kept. A lesson
        that moved to another stage (ended, completed, deleted) meanwhile is refused instead."""
        stage = s["stage"]
        snapshot = copy.deepcopy(s)
        self.lock.release()
        try:
            result = self.tutor(action, snapshot)
        finally:
            self.lock.acquire()
        fresh = self.store.get(s["id"])
        if fresh["stage"] != stage:
            raise ValueError("This lesson changed in another window. Reload it before continuing.")
        return result, fresh

    def event(self, s, action, result):
        s["events"].append({"action": action, "time": time.time(), **result})

    def dispatch(self, data):
        with self.lock:
            action = data.get("action")
            if action == "create":
                mode = data.get("mode", "learn")
                topic, level = str(data.get("topic", ""))[:200], str(data.get("level", ""))[:100]
                if mode == "learn" and not topic.strip():
                    raise ValueError("What topic would you like to learn?")
                s = self.store.create(mode, topic, level, self.demo)
                self.active = s["id"]
                return s
            key = data.get("id") or self.active
            if not key:
                raise ValueError("Open a lesson first.")
            s = self.store.get(key)
            if action == "select":
                self.active = key
                return s
            if action == "delete":
                self.store.delete(key)
                if self.active == key:
                    self.active = None
                return {"deleted": True}
            revision = data.get("revision", s["revision"])
            if revision != s["revision"]:
                # The only change since the browser loaded is spoken lines appended while eve listened:
                # merge a save into them instead of refusing it.
                if not (action == "save" and isinstance(revision, int)
                        and revision >= s.get("last_other_revision", s["revision"])):
                    raise ValueError("This lesson changed in another window. Reload it before continuing.")
            if s["demo"] != self.demo:
                raise ValueError("Open this lesson using the same demo/live mode it was created in.")
            if s["stage"] == "ended" and action != "resume":
                raise ValueError("Resume this saved lesson before editing it.")
            if action == "save":
                work = validate_work(data.get("work", {}))
                if s["stage"] not in ("input", "confirm"):
                    work.pop("problem", None)
                    work.pop("source_image", None)
                if "ideas" in work and s.get("spoken"):
                    base = data.get("ideas_base", revision)
                    work["ideas"] = merge_spoken(work["ideas"], s, base if isinstance(base, int) else revision)
                s.update(work)
            elif action == "generate":
                if s["stage"] != "setup":
                    raise ValueError("Start a new lesson to generate another problem.")
                result, s = self._ask_tutor("generate", s)
                if not result["problem"].strip():
                    raise ValueError("Eve did not generate a problem. Try again.")
                s.update(problem=result["problem"], stage="working")
                self.event(s, action, result)
            elif action == "recognize":
                if s["stage"] not in ("input", "confirm"):
                    raise ValueError("Start a new lesson to change the problem.")
                if not (s["problem"].strip() or s["source_image"] or mark_count(s)):
                    raise ValueError("Write a problem or add a screenshot first.")
                result, s = self._ask_tutor(action, s)
                s.update(problem=result["problem"], stage="confirm")
                self.event(s, action, result)
            elif action == "confirm":
                if s["stage"] != "confirm" or not s["problem"].strip():
                    raise ValueError("Confirm the problem text first.")
                s["stage"] = "working"
                s["problem_stroke_count"] = mark_count(s)
                self.event(s, action, {"feedback": "Write your ideas or show where you are stuck. If you don't know how to start, tell me."})
            elif action in ("hint", "step", "check", "recap"):
                if s["stage"] not in ("working", "complete"):
                    raise ValueError("Confirm your problem before asking for help.")
                if s["stage"] == "complete" and action != "recap":
                    raise ValueError("This problem is complete. Try another problem or review the recap.")
                if s["mode"] == "help" and not (s["ideas"].strip() or s["stuck"] or mark_count(s) > s.get("problem_stroke_count", 0)):
                    raise ValueError("Put down your ideas first, or select I don't know how to start.")
                result, s = self._ask_tutor(action, s)
                self.event(s, action, result)
                if result["status"] == "complete":
                    s["stage"] = "complete"
            elif action == "end":
                s["resume_stage"] = s["stage"]
                s["stage"] = "ended"
                self.event(s, action, {"feedback": "Your lesson and all your work are saved. Come back whenever you're ready."})
            elif action == "resume":
                if s["stage"] == "ended":
                    s["stage"] = s.pop("resume_stage", "working")
            else:
                raise ValueError("Unknown lesson action.")
            # Every change except a spoken append moves this mark: a stale save may merge only past appends.
            s["last_other_revision"] = s["revision"] + 1
            s = self.store.save(s, expected=s["revision"])
            self.active = s["id"]
            if self.notify and action != "save" and s["events"]:
                try:
                    self.notify(s["events"][-1].get("feedback", ""), s["stage"])
                except Exception:
                    log.exception("learning: robot notification failed; lesson is saved")
            return s

    # ---- think out loud ----

    def append_spoken(self, lesson_id, text):
        """Add one thing the learner said while eve listened to the end of their ideas.

        Typed ideas are never replaced. Returns True when the line was saved."""
        line = " ".join(str(text).split())[:MAX_SPOKEN_IDEA]
        if not line:
            return False
        with self.lock:
            s = self.store.get(lesson_id)
            if s["stage"] == "ended" or s["demo"] != self.demo:
                return False
            s["ideas"] = "\n".join(filter(None, [s["ideas"].rstrip(), line]))[-20000:]
            s["stuck"] = False
            s["spoken"] = (s.get("spoken", []) + [{"rev": s["revision"] + 1, "text": line}])[-SPOKEN_KEEP:]
            s = self.store.save(s, expected=s["revision"])
            return True

    def listen_status(self):
        if self.listener is None:
            return dict(LISTEN_OFF, reason=ROBOT_APP_NOT_RUNNING)
        try:
            status = self.listener.status()
        except Exception:
            log.exception("learning: listen status failed")
            return dict(LISTEN_OFF, reason="eve could not say whether it is listening.")
        return {"available": True, "state": status.get("state", "off"), "lesson_id": status.get("lesson_id"),
                "reason": status.get("reason", "")}

    def _listen_result(self, result):
        """The listener's answer plus the current state. The status goes first, so its empty "reason"
        can never overwrite the sentence that explains a refusal."""
        return {**self.listen_status(), **result}

    def _call_listener(self, name, *args):
        try:
            return getattr(self.listener, name)(*args)
        except Exception:
            log.exception("learning: %s failed", name)
            return {"ok": False, "reason": "eve could not change listening. Try again."}

    def listen(self, lesson_id=None):
        """Start think out loud for a lesson (the browser's, else the active one). Never raises."""
        if self.listener is None:
            return {**LISTEN_OFF, "ok": False, "reason": ROBOT_APP_NOT_RUNNING}
        with self.lock:
            key = lesson_id or self.active
            try:
                s = self.store.get(key) if key else None
            except KeyError:
                s = None
            refusal = listen_refusal(s)
            if refusal:
                return self._listen_result({"ok": False, "reason": refusal})
            self.active = s["id"]
        # Outside the lock: the daemon may take a moment, and saving spoken ideas needs the lock.
        return self._listen_result(self._call_listener("start", s))

    def stop_listening(self):
        if self.listener is None:
            return {**LISTEN_OFF, "ok": False, "reason": ROBOT_APP_NOT_RUNNING}
        return self._listen_result(self._call_listener("stop"))

    def voice(self, action="open", mode="", topic="", level="", text=""):
        if action == "open":
            webbrowser.open(self.url)
            return {"ok": True, "answer": "Would you like to learn a topic or work on a problem you already have?"}
        if action == "listen":
            return self.listen()
        if action == "stop-listening":
            return self.stop_listening()
        if action == "start":
            s = self.dispatch({"action": "create", "mode": mode or "learn", "topic": topic, "level": level})
            if s["mode"] == "learn":
                s = self.dispatch({"action": "generate", "id": s["id"]})
            webbrowser.open(self.url + "#lesson/" + s["id"])
            return {"ok": True, "answer": s["problem"] or "Add your problem to the whiteboard, then show me your ideas."}
        if action == "status":
            s = self.store.get(self.active) if self.active else None
            return {"ok": True, "lesson": self.store.summary(s) if s else None,
                    "answer": s["events"][-1].get("feedback", "") if s and s["events"] else "Choose a lesson in the learning window."}
        if action == "ideas":
            s = self.dispatch({"action": "save", "work": {"ideas": text, "stuck": not text.strip()}})
            return {"ok": True, "answer": "I've saved your thinking. Would you like a hint or a check?"}
        s = self.dispatch({"action": action})
        event = s["events"][-1]
        return {"ok": True, "answer": " ".join(filter(None, [event.get("step"), event.get("feedback")]))}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, status, data, mime="application/json", nonce=""):
        encoded = json.dumps(data).encode() if mime == "application/json" else data
        self.send_response(status)
        self.send_header("Content-Type", mime)
        self.send_header("Content-Length", str(len(encoded)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Content-Security-Policy", content_policy(nonce) if nonce else CSP)
        self.end_headers()
        self.wfile.write(encoded)

    def allowed(self):
        expected = urlsplit(self.server.app.url).netloc
        return self.headers.get("Host") == expected and self.headers.get("Origin", self.server.app.url.rstrip("/")) == self.server.app.url.rstrip("/")

    def do_GET(self):
        if not self.allowed():
            return self.reply(403, {"error": "Local access only."})
        path = urlsplit(self.path).path
        app = self.server.app
        try:
            if path == "/api/config":
                settings = {"provider": "demo", "model": "offline examples", "ready": True, "key_name": ""} if app.demo else live_settings()
                return self.reply(200, {**settings, "token": self.server.token, "demo": app.demo, "active": app.active,
                                        "listen_available": app.listener is not None,
                                        # A tldraw licence key is shipped to the browser by design; it is not a secret.
                                        "tldraw_license_key": os.environ.get("EVE_TLDRAW_LICENSE_KEY", "").strip()})
            if path == "/api/listening":
                return self.reply(200, app.listen_status())
            if path == "/api/lessons":
                return self.reply(200, app.store.list())
            if path.startswith("/api/lessons/"):
                parts = path.split("/")
                key = parts[3]
                return self.reply(200, app.store.history(key) if len(parts) > 4 and parts[4] == "history" else app.store.get(key))
            if path.startswith("/fonts/"):
                # urlsplit does not percent-decode, so "..", "%2f" and friends fail the name checks.
                name = path[len("/fonts/"):]
                if name == "OFL.txt":
                    mime = "text/plain; charset=utf-8"
                elif FONT_FILE.fullmatch(name):
                    mime = "font/woff2"
                else:
                    return self.reply(404, {"error": "Not found"})
                target = FONTS / name
                if target.resolve().parent != FONTS.resolve() or not target.is_file():
                    return self.reply(404, {"error": "Not found"})
                return self.reply(200, target.read_bytes(), mime)
            if path.startswith("/canvas/"):
                name = path[len("/canvas/"):]
                mime = CANVAS_MIME.get(Path(name).suffix)
                if not mime or not CANVAS_FILE.fullmatch(name):
                    return self.reply(404, {"error": "Not found"})
                target = (CANVAS / name).resolve()
                if not target.is_relative_to(CANVAS.resolve()) or not target.is_file():
                    return self.reply(404, {"error": "Not found"})
                return self.reply(200, target.read_bytes(), mime)
            files = {"/":("index.html", "text/html; charset=utf-8"), "/app.js": ("app.js", "text/javascript; charset=utf-8"),
                     "/style.css": ("style.css", "text/css; charset=utf-8")}
            if path in files:
                name, mime = files[path]
                body = (ASSETS / name).read_bytes()
                if path == "/":
                    nonce = secrets.token_urlsafe(16)
                    return self.reply(200, body.replace(NONCE_SLOT, nonce.encode()), mime, nonce=nonce)
                return self.reply(200, body, mime)
            return self.reply(404, {"error": "Not found"})
        except KeyError:
            return self.reply(404, {"error": "Lesson not found"})

    def do_POST(self):
        if not self.allowed() or self.headers.get("X-Eve-Token") != self.server.token:
            return self.reply(403, {"error": "Reload this local page before saving."})
        if self.path != "/api/action":
            return self.reply(404, {"error": "Not found"})
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= MAX_BODY:
                return self.reply(413, {"error": "Request is too large."})
            data = json.loads(self.rfile.read(length))
            if not isinstance(data, dict):
                raise ValueError("Invalid request.")
            app = self.server.app
            if data.get("action") in LISTEN_ACTIONS:
                # The "Think out loud" toggle. The daemon owns the microphone; standalone, nobody does.
                key = data.get("id")
                result = app.listen(key if isinstance(key, str) else None) if data["action"] == "listen" else app.stop_listening()
                log.info("learning: %s from the browser -> ok=%s", data["action"], result.get("ok"))
                if not result.get("ok"):
                    status = 503 if not result.get("available") else 409
                    return self.reply(status, {**result, "error": result.get("reason") or "eve cannot listen now."})
                return self.reply(200, result)
            return self.reply(200, app.dispatch(data))
        except KeyError:
            return self.reply(404, {"error": "Lesson not found"})
        except (ValueError, TypeError) as exc:
            return self.reply(400, {"error": str(exc)})
        except Exception:
            log.exception("learning request failed")
            return self.reply(500, {"error": "Eve could not finish this request. Your previously saved work is safe."})


def start(directory=None, demo=False, port=DEFAULT_PORT, notify=None, listener=None):
    app = LearningApp(directory, demo, notify=notify, listener=listener)
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.app = app
    server.token = secrets.token_urlsafe(32)
    app.url = f"http://127.0.0.1:{server.server_port}/"
    thread = threading.Thread(target=server.serve_forever, name="eve-learning", daemon=True)
    thread.start()
    return server


def main(argv=None):
    parser = argparse.ArgumentParser(description="Eve's saved lessons and whiteboard")
    parser.add_argument("--demo", action="store_true", help="Offline examples; no API calls or handwriting recognition")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    parser.add_argument("--data-dir", type=Path)
    parser.add_argument("--env-file", type=Path, help="Explicit environment file (use a /mnt/c/... path from WSL)")
    parser.add_argument("--no-open", action="store_true")
    args = parser.parse_args(argv)
    from ..envfile import load_env_file
    load_env_file(args.env_file)
    directory = args.data_dir or (default_dir() / "demo" if args.demo else default_dir())
    try:
        server = start(directory, args.demo, args.port)
    except OSError as exc:
        if exc.errno != errno.EADDRINUSE and getattr(exc, "winerror", None) != 10048:
            raise
        print(f"Port {args.port} is already in use. If Eve is already running, open "
              f"http://127.0.0.1:{args.port}/ instead of starting another instance.\n"
              "To load changed settings, stop the existing Eve process first, then restart it. "
              "Or choose a different port with --port.", file=sys.stderr)
        return 2
    print(f"Eve learning: {server.app.url} ({'offline demo' if args.demo else 'live tutor'})", flush=True)
    if not args.no_open:
        webbrowser.open(server.app.url)
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        server.shutdown()
        server.server_close()
    return 0
