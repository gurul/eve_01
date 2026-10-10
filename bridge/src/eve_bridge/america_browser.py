"""A reusable normal-Chrome America.gov chat flow, with Jev control recovery.

Only UI fill/click submits a question. The answer is read from that UI request's
completed SSE, not guessed from a pause in text growth. Browser connection and
semantic locator recipes persist; every question starts a fresh document.
"""

from __future__ import annotations

import asyncio
import threading
import time
from collections import deque
from dataclasses import dataclass, replace
from typing import Any, Mapping
from urllib.parse import urlsplit

from . import browser_lane
from .america import AMERICA_URL, API_URL, MAX_STREAM_BYTES, _ProtocolError, parse_stream
from .typed_ask import JEV_STEP_GATES

FORM = 'form[data-slot="chat-input-shell"][aria-label="Describe what you need"]'
FIELD = 'textarea[name="message"]'
POLL_MS = 100
CONTROL_MS = 3000
# Read accessible labels without the generic collector's current-value fallback.
_NAME_JS = """el => {
  const by = el.getAttribute('aria-labelledby');
  if (by) { const name = by.split(/\\s+/).map(id => document.getElementById(id)?.textContent || '').join(' ').trim();
    if (name) return name; }
  const aria = el.getAttribute('aria-label'); if (aria) return aria.trim();
  const labels = Array.from(el.labels || []).map(label => label.textContent || '').join(' ').trim();
  if (labels) return labels;
  if (el.tagName === 'TEXTAREA') return (el.title || el.getAttribute('placeholder') || '').trim();
  return (el.textContent || el.title || '').trim();
}"""


class BrowserFailure(RuntimeError):
    """Static owner-safe failure text; never an upstream body or private question."""


@dataclass(frozen=True)
class Recipe:
    role: str = ""
    name: str = ""
    css: str = ""

    def locate(self, form: Any) -> Any:
        return form.locator(self.css) if self.css else form.get_by_role(self.role, name=self.name, exact=True)


class AmericaBrowser:
    """Dedicated lane/tab; repeated calls reuse Chrome without sharing another task's page."""

    def __init__(self, lane: Any, *, step_asker: Any = None, timeout_secs: float = 60.0,
                 clock: Any = time.monotonic) -> None:
        self.lane, self._asker, self.timeout_secs, self._clock = lane, step_asker, timeout_secs, clock
        self._field = Recipe(css=FIELD)
        self._send = Recipe(role="button", name="Send message")
        self._lock = asyncio.Lock()
        self._inflight: asyncio.Task | None = None
        self._cancel: threading.Event | None = None
        self._closed = False
        self._recent_ids: deque[str] = deque(maxlen=128)
        self.last_metrics: dict[str, Any] = {}

    @staticmethod
    def _unique_visible(locator: Any, *, enabled: bool = False) -> bool:
        return locator.count() == 1 and locator.is_visible() and (not enabled or locator.is_enabled())

    def _recover(self, wrapper: Any, form: Any, kind: str) -> tuple[Any, Recipe]:
        if self._asker is None:
            raise BrowserFailure("America.gov's chat controls changed and Jev is unavailable. Open its browser chat.")
        snap = wrapper.snapshot()
        candidates = []
        for candidate in snap.elements:
            wanted = candidate.editable if kind == "field" else candidate.role == "button"
            if not wanted:
                continue
            loc = wrapper._locator(candidate)
            if loc.count() == 1 and loc.evaluate("(el, selector) => !!el.closest(selector)", FORM):
                tag = loc.evaluate("el => el.tagName.toLowerCase()")
                if tag != ("textarea" if kind == "field" else "button"):
                    continue
                if kind == "send" and loc.get_attribute("type") != "submit":
                    continue
                name = loc.evaluate(_NAME_JS)
                if isinstance(name, str):
                    name = " ".join(name.split())
                if isinstance(name, str) and 0 < len(name) <= 120:
                    candidates.append((candidate, name))
        # The static objective, chat URL and labels are enough. Never send field
        # values, page prose, legal question or answer to the grounding provider.
        options = {str(i): f"{c.role}: {name}" for i, (c, name) in enumerate(candidates[:12])}
        answer = self._asker(
            "Find the chat question textbox" if kind == "field" else "Find the Send button for the chat question",
            app="America.gov", context=AMERICA_URL, options=options,
        )
        self.last_metrics["jev_calls"] += 1
        # The owner already authorized sending this America question. Only this
        # verified chat-submit operation can use that authorization; confidence,
        # presence, margin and already-done gates still apply unchanged.
        selected = JEV_STEP_GATES.decide(replace(answer, risky=0.0) if kind == "send" else answer)
        if selected not in options:
            raise BrowserFailure("Jev could not safely identify America.gov's chat control. Open its browser chat.")
        candidate, name = candidates[int(selected)]
        # Cache a semantic recipe, never a per-snapshot data-eve-id or a screen point.
        recipe = Recipe(role="textbox" if kind == "field" else "button", name=name)
        locator = recipe.locate(form)
        if not self._unique_visible(locator, enabled=kind == "field"):
            raise BrowserFailure("America.gov's selected chat control changed before use. Open its browser chat.")
        if kind == "field" and locator.evaluate("el => el.tagName.toLowerCase()") != "textarea":
            raise BrowserFailure("America.gov did not expose a supported chat textarea. Open its browser chat.")
        if kind == "send" and (locator.evaluate("el => el.tagName.toLowerCase()") != "button"
                               or locator.get_attribute("type") != "submit"):
            raise BrowserFailure("America.gov did not expose a supported chat submit button. Open its browser chat.")
        return locator, recipe

    def _controls(self, wrapper: Any) -> tuple[Any, Any]:
        page = wrapper.page
        form = page.locator(FORM)
        if not self._unique_visible(form):
            raise BrowserFailure("America.gov's chat form is unavailable. Open its browser chat.")
        field, send = self._field.locate(form), self._send.locate(form)
        if not self._unique_visible(field, enabled=True):
            field, self._field = self._recover(wrapper, form, "field")
        if not self._unique_visible(send):
            # Older observed accessible name was Send; still an exact semantic match.
            alternative = Recipe(role="button", name="Send")
            send = alternative.locate(form)
            if self._unique_visible(send):
                self._send = alternative
            else:
                send, self._send = self._recover(wrapper, form, "send")
        if field.evaluate("el => el.tagName.toLowerCase()") != "textarea" or send.get_attribute("type") != "submit":
            raise BrowserFailure("America.gov's chat controls are incompatible. Open its browser chat.")
        return field, send

    @staticmethod
    def _target(request: Any) -> bool:
        return request.method == "POST" and request.url == API_URL

    @staticmethod
    def _correlate(request: Any, text: str) -> str:
        try:
            body = request.post_data_json
            messages = body["messages"]
            message = messages[0]
            if (set(body) != {"language", "id", "messages", "trigger"}
                    or not isinstance(body["id"], str) or not 1 <= len(body["id"]) <= 256
                    or not isinstance(messages, list)
                    or set(message) != {"id", "role", "parts"}
                    or not isinstance(message["id"], str) or not 1 <= len(message["id"]) <= 256
                    or body.get("language") != "en" or body.get("trigger") != "submit-message"
                    or len(messages) != 1 or message.get("role") != "user"
                    or message.get("parts") != [{"type": "text", "text": text}]):
                raise ValueError("Unexpected question/history.")
            return body["id"]
        except (KeyError, IndexError, TypeError, ValueError) as exc:
            raise BrowserFailure("America.gov's browser request did not match this fresh question. It will not be resent.") from exc

    def _reset(self) -> None:
        # Runs only on the lane worker. Closing our own page aborts an abandoned
        # stream and ensures another call cannot read the previous question.
        wrapper = self.lane._page
        if wrapper is not None:
            try:
                wrapper.page.close()
            except Exception:  # noqa: BLE001 — cleanup cannot replace the safe failure
                pass
            finally:
                self.lane._page = None

    def _ask(self, question: str, jurisdiction: str, cancelled: threading.Event) -> tuple[str, list[dict[str, str]]]:
        started = self._clock()
        self.last_metrics = {"jev_calls": 0, "submitted": False}
        text = f"Jurisdiction: {jurisdiction}\nQuestion: {question}"
        requests: list[Any] = []
        finished: list[Any] = []
        failed: list[Any] = []
        page = None
        listeners: list[tuple[str, Any]] = []
        guard = None
        refused = False
        accepted = False

        def remaining() -> int:
            left = self.timeout_secs - (self._clock() - started)
            if cancelled.is_set() or left <= 0:
                raise BrowserFailure("America.gov's browser question was stopped or timed out. It will not be resent.")
            return max(1, int(left * 1000))

        try:
            if not self.lane.config.attach:
                raise BrowserFailure("America.gov needs Eve's normal Chrome profile. Configure EVE_BROWSER_OWN=1.")
            wrapper = self.lane._ensure()
            page = wrapper.page
            remaining()
            # Unmount the prior app before loading /chat: its history is browser
            # memory. Do not reuse an owner's tab or replay previous messages.
            page.goto("about:blank", wait_until="domcontentloaded", timeout=min(3000, remaining()))
            page.goto(AMERICA_URL, wait_until="domcontentloaded", timeout=min(15000, remaining()))
            parsed = urlsplit(page.url)
            if parsed.scheme != "https" or parsed.netloc != "america.gov" or parsed.path not in ("/", "/chat"):
                raise BrowserFailure("America.gov did not open its ordinary chat page. Open it in your browser.")
            if page.title() == "Just a moment...":
                raise BrowserFailure("America.gov is showing a browser security check. Open its chat in your browser.")
            page.locator(FORM).wait_for(state="visible", timeout=min(10000, remaining()))
            field, send = self._controls(wrapper)
            remaining()
            field.fill(text, timeout=min(CONTROL_MS, remaining()))
            if field.input_value(timeout=min(CONTROL_MS, remaining())) != text:
                raise BrowserFailure("America.gov's question field did not contain the complete question.")
            if not self._unique_visible(send, enabled=True):
                raise BrowserFailure("America.gov's Send button is unavailable. Open its browser chat.")

            def preflight(route: Any) -> None:
                nonlocal refused, accepted
                try:
                    chat_id = self._correlate(route.request, text)
                    if accepted or chat_id in self._recent_ids:
                        raise BrowserFailure("Repeated chat request.")
                except BrowserFailure:
                    refused = True
                    route.abort()
                    return
                accepted = True
                self._recent_ids.append(chat_id)
                route.continue_()  # pass the browser-generated body through unchanged

            # Reject restored history/metadata before any question leaves Chrome.
            # This does not build or replay an HTTP request.
            guard = preflight
            page.route(API_URL, guard)

            def observe(bucket: list[Any]):
                def callback(request: Any) -> None:
                    if self._target(request):
                        bucket.append(request)
                return callback

            # Listen before the one submit attempt; even a click exception can
            # follow a successful send. Never infer that such a send is safe to repeat.
            for event, bucket in (("request", requests), ("requestfinished", finished), ("requestfailed", failed)):
                callback = observe(bucket)
                page.on(event, callback)
                listeners.append((event, callback))
            self.last_metrics["submitted"] = True
            send.click(timeout=min(CONTROL_MS, remaining()))
            while not finished:
                remaining()
                if refused:
                    raise BrowserFailure("America.gov's browser request did not match this fresh question. It will not be resent.")
                if failed or len(requests) > 1:
                    raise BrowserFailure("America.gov's chat request failed or repeated. It will not be resent.")
                page.wait_for_timeout(min(POLL_MS, remaining()))
            if refused or not accepted or len(requests) != 1 or len(finished) != 1 or finished[0] is not requests[0] or failed:
                raise BrowserFailure("America.gov's answer could not be tied to this one question. It will not be resent.")
            self._correlate(requests[0], text)
            response = finished[0].response()
            if response is None or response.status != 200:
                raise BrowserFailure("America.gov did not answer this browser question. It will not be resent.")
            headers = response.headers
            if (headers.get("content-type", "").split(";", 1)[0].strip().lower() != "text/event-stream"
                    or headers.get("x-vercel-ai-ui-message-stream") != "v1"):
                raise BrowserFailure("America.gov changed its browser answer protocol. It will not be resent.")
            raw = response.body()
            remaining()
            if len(raw) > MAX_STREAM_BYTES:
                raise _ProtocolError("Stream limit exceeded.")
            answer, sources = parse_stream(raw)
            self.last_metrics["seconds"] = round(self._clock() - started, 3)
            return answer, sources
        except BrowserFailure:
            self._reset()
            raise
        except Exception as exc:  # noqa: BLE001 — no raw question, body, browser path or Jev error in output
            self._reset()
            suffix = " The question may have been sent; it will not be resent." if self.last_metrics["submitted"] else ""
            raise BrowserFailure("America.gov's browser flow could not complete." + suffix + " Open its browser chat.") from exc
        finally:
            if page is not None:
                if guard is not None:
                    try:
                        page.unroute(API_URL, guard)
                    except Exception:  # noqa: BLE001 — page may already be closed
                        pass
                for event, callback in listeners:
                    try:
                        page.remove_listener(event, callback)
                    except Exception:  # noqa: BLE001 — the owned page may already be closed
                        pass

    async def ask(self, question: str, jurisdiction: str) -> tuple[str, list[dict[str, str]]]:
        async with self._lock:
            if self._closed:
                raise BrowserFailure("America.gov's browser is closed.")
            if self._inflight is not None and not self._inflight.done():
                raise BrowserFailure("The previous America.gov browser question is still stopping. It will not be resent.")
            self._cancel = threading.Event()
            job = self._inflight = asyncio.create_task(self.lane._run(self._ask, question, jurisdiction, self._cancel))
            try:
                async with asyncio.timeout(self.timeout_secs):
                    return await asyncio.shield(job)
            except (asyncio.CancelledError, TimeoutError):
                # A sync worker cannot be killed by cancelling its asyncio future.
                # Keep it tracked and quarantine the page until its finite waits end.
                self._cancel.set()
                job.add_done_callback(lambda task: task.exception() if not task.cancelled() else None)
                raise

    async def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        if self._cancel is not None:
            self._cancel.set()
        if self._inflight is not None:
            await asyncio.gather(self._inflight, return_exceptions=True)
        await self.lane.close()


def make_browser(environ: Mapping[str, str]) -> AmericaBrowser | None:
    config = browser_lane.configured(environ)
    if not config.attach:
        return None
    try:
        import playwright  # noqa: F401 — optional existing browser extra
    except ImportError:
        return None
    # America has its own opt-out; enabling this fixed flow does not enable Jev
    # on unrelated desktop tasks. Existing route/key/timeout/spend tracking apply.
    env = {**environ, "EVE_JEV_STEP": environ.get("EVE_AMERICA_JEV", "1")}
    asker = browser_lane.make_step_asker(env)
    return AmericaBrowser(browser_lane.BrowserLane(config), step_asker=asker)
