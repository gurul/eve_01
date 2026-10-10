"""Open an app without Codex: task_router's launch reflex in front of the Codex computer agent.

Since computer use moved to Codex (codex_computer.py), every request — "open Spotify" included — starts
a Codex turn: a process, a thread, a model, tens of seconds. A quarter of the owner's requests are
nothing but a launch (task_router.py's own count), and those need no model at all: ``open -a Spotify``.

``ReflexFirstAgent`` wraps whichever agent the daemon builds and keeps its run/steer/cancel/status
contract, so the voice and the Telegram door both get it with no change of their own. The wrapped agent
is built only when the reflex declines (``make_inner``), so an app launch never spends the warm Codex
agent codex_warm.py keeps ready; ``on_done`` tells it to warm the next one after a Codex task. Before handing
the goal over it asks ``task_router.classify``:

* the rules first (code, microseconds): "open Spotify", "pull up Notes for me";
* Jev only for wording the rules do not know ("Notes, please."), with the same gates
  task_router ships (typed_ask.make_jev_request_asker), via ``EVE_ROUTER_MODEL=jev`` and the
  route in ``EVE_JEV_ROUTE`` (OpenRouter on this Mac). Jev can only name an installed app.

Quitting (owner, 2026-09-23) takes the same path: only the explicit word "quit", a graceful quit (the
app's own "save changes?" dialog still appears; never a force quit). "quit all" quits every regular app
except the ones in ``QUIT_ALL_KEEP`` — the terminals Claude Code runs in, Claude, ChatGPT/Codex (the
computer use eve hands off to), eve's own app, Finder — plus ``EVE_QUIT_ALL_KEEP``. Chosen by
tools/route_eval.py --quit on a blind holdout: rules then Jev, 45 of 45, 0 unsafe.

Only a request the plan says is COMPLETE — a bare launch of an installed app, nothing after it — is
opened here. Anything else, a failed ``open``, a Jev error, or ``EVE_REFLEXES=0``: the wrapped
agent runs the goal exactly as before.
"""

from __future__ import annotations

import asyncio
import logging
import os
import re
import time
from typing import Any, Awaitable, Callable, Iterable, Optional
from urllib.parse import urlsplit

from . import task_router
from .agent_contract import AgentEvent

log = logging.getLogger(__name__)


def reflexes_on(environ: Any = None) -> bool:
    env = os.environ if environ is None else environ
    raw = str(env.get("EVE_REFLEXES", "")).strip().lower()
    return task_router.REFLEX_DEFAULT if not raw else raw not in ("0", "false", "no", "off")


# A task's goal is the owner's words for THIS request ("Use Google search to open it up"), written by the door's
# model; a link the owner sent in an earlier message is not in it (production, 2026-09-24: a Google Maps link,
# then "open it up", and the Chrome lane got the words without the link). When the goal refers back and carries
# no link of its own, the links from the owner's recent messages go with it (``with_referenced_links``).
LINK = re.compile(r"https?://[^\s<>\"'`]+", re.I)
REFERS_BACK = re.compile(
    r"\b(that|this|the|same)\s+(link|url|page|site|website|place|address|location|map|pin|article|video|listing|post)\b"
    r"|\b(open|visit|load|show|check|search|look up|go to|read|click|pull up|bring up|navigate to)\s+"
    r"(it|that|this|them|those|these)\b", re.I)
RECENT_OWNER_MESSAGES = 4       # how far back a "that link" may reach
MAX_REFERENCED_LINKS = 3


def links_in(text: str) -> list[str]:
    """The http(s) links in a text, in order, without trailing punctuation, deduplicated."""
    out: list[str] = []
    for m in LINK.findall(text or ""):
        url = m.rstrip(".,;:!?)]}")
        if url not in out:
            out.append(url)
    return out


def refers_back(goal: str) -> bool:
    return bool(REFERS_BACK.search(goal or ""))


def with_referenced_links(goal: str, recent_owner_texts: Iterable[str]) -> str:
    """``goal`` plus the links from the owner's recent messages (oldest to newest; the last
    RECENT_OWNER_MESSAGES are read, newest link first) when the goal refers back to something ("open it",
    "that link") and has no link of its own. Otherwise ``goal`` unchanged."""
    if not goal or links_in(goal) or not refers_back(goal):
        return goal
    found: list[str] = []
    for text in reversed(list(recent_owner_texts)[-RECENT_OWNER_MESSAGES:]):
        for url in links_in(text):
            if url not in found:
                found.append(url)
    if not found:
        return goal
    found = found[:MAX_REFERENCED_LINKS]
    return (goal + "\n\n[note] The owner's recent messages included "
            + ("this link" if len(found) == 1 else "these links, newest first") + ", which the request refers to: "
            + " ".join(found))


def goal_shape(goal: str) -> str:
    """What a goal looks like, for the log, without the owner's words: its length, its links' hosts, whether
    it refers back to something."""
    hosts = [urlsplit(u).hostname or "?" for u in links_in(goal)]
    return (f"{len((goal or '').split())} words, {len(hosts)} link(s)" + (f" ({', '.join(hosts)})" if hosts else "")
            + (", refers back" if refers_back(goal) else ""))


QUIT_ALL_KEEP = frozenset({"finder", "warp", "stable", "terminal", "iterm2", "iterm", "ghostty", "cmux", "claude",
                           "chatgpt", "codex", "stackchannotes", "codex computer use", "skycomputeruseservice"})
QUIT_SETTLE_SECS = 2.0          # the longest wait for apps to go; checked every QUIT_POLL_SECS, done when they are
QUIT_POLL_SECS = 0.25


def quit_keep(environ: Any = None) -> frozenset[str]:
    env = os.environ if environ is None else environ
    extra = {p.strip().casefold() for p in str(env.get("EVE_QUIT_ALL_KEEP", "")).split(",") if p.strip()}
    return QUIT_ALL_KEEP | extra


def jev_quit_asker(environ: Any = None) -> Optional[Callable[[str, list[str]], str]]:
    """Jev for quit wording the rules do not know, under the same switch as jev_asker."""
    env = os.environ if environ is None else environ
    if str(env.get("EVE_ROUTER_MODEL", "")).strip().lower() != "jev":
        return None
    try:
        from . import jev, typed_ask

        url, key, model = jev.route_config(env)
        return typed_ask.make_jev_quit_asker(jev.make_predict(url, key, model, timeout_s=2.0), time.perf_counter)
    except Exception as e:  # noqa: BLE001
        log.warning("app-reflex: Jev quit is off (%s)", type(e).__name__)
        return None


async def _osascript(*lines: str) -> tuple[bool, str]:
    args: list[str] = []
    for line in lines:
        args += ["-e", line]
    proc = await asyncio.create_subprocess_exec("osascript", *args, stdout=asyncio.subprocess.PIPE,
                                                stderr=asyncio.subprocess.PIPE)
    out, err = await proc.communicate()
    return proc.returncode == 0, (out if proc.returncode == 0 else err).decode(errors="replace").strip()


def _quote(name: str) -> str:
    return '"' + name.replace("\\", "\\\\").replace('"', '\\"') + '"'


async def _running(name: str) -> bool:
    ok, out = await _osascript(f"return application {_quote(name)} is running")
    return ok and out == "true"


async def _gone(names: list[str], settle: float) -> list[str]:
    """Wait until every app in ``names`` has quit or ``settle`` passes; the ones still running."""
    loop = asyncio.get_running_loop()
    deadline = loop.time() + settle
    left = list(names)
    while left:
        left = [n for n in left if await _running(n)]
        if not left or loop.time() >= deadline:
            break
        await asyncio.sleep(QUIT_POLL_SECS)
    return left


async def quit_app(name: str, settle: float = QUIT_SETTLE_SECS) -> str:
    """Quit one app the way Cmd+Q does, without waiting on it. The sentence for the owner."""
    if not await _running(name):
        return f"{name} isn't open."
    ok, err = await _osascript("ignoring application responses", f"tell application {_quote(name)} to quit",
                               "end ignoring")
    if not ok:
        return f"I couldn't quit {name}: {err[:120]}"
    if await _gone([name], settle):
        return f"{name} didn't quit. It may be asking to save, or it declined; it's still open."
    return f"Quit {name}."


async def quit_all(keep: frozenset[str], settle: float = QUIT_SETTLE_SECS) -> str:
    """Quit every regular app except ``keep`` (process or app names, case-folded)."""
    ok, out = await _osascript('tell application "System Events" to get name of every application process '
                               'whose background only is false')
    if not ok:
        return f"I couldn't list the open apps: {out[:120]}"
    names = [n.strip() for n in out.split(",") if n.strip()]
    targets = [n for n in names if n.casefold() not in keep]
    kept = [("Warp" if n == "stable" else n) for n in names if n.casefold() in keep and n != "Finder"]
    if not targets:
        return "Nothing to quit." + (f" Kept {', '.join(kept)}." if kept else "")
    for n in targets:
        await _osascript("ignoring application responses", f"tell application {_quote(n)} to quit", "end ignoring")
    waiting = await _gone(targets, settle)
    done = [n for n in targets if n not in waiting]
    said = f"Quit {len(done)} app{'s' if len(done) != 1 else ''}" + (f": {', '.join(done)}." if done else ".")
    if waiting:
        said += (f" {', '.join(waiting)} didn't quit (asking to save, or declined); "
                 f"{'it is' if len(waiting) == 1 else 'they are'} still open.")
    if kept:
        said += f" Kept {', '.join(kept)}."
    return said


def jev_asker(environ: Any = None) -> Optional[Callable[[str, list[str]], str]]:
    """Jev for unfamiliar wording, or None when EVE_ROUTER_MODEL is not jev or no key is set."""
    env = os.environ if environ is None else environ
    if str(env.get("EVE_ROUTER_MODEL", "")).strip().lower() != "jev":
        return None
    try:
        from . import jev, typed_ask

        url, key, model = jev.route_config(env)
        return typed_ask.make_jev_request_asker(jev.make_predict(url, key, model, timeout_s=2.0), time.perf_counter)
    except Exception as e:  # noqa: BLE001 — no Jev is "rules only", never a failure
        log.warning("app-reflex: Jev is off (%s)", type(e).__name__)
        return None


async def open_app(app: str) -> tuple[bool, str]:
    proc = await asyncio.create_subprocess_exec("open", "-a", app, stdout=asyncio.subprocess.DEVNULL,
                                                stderr=asyncio.subprocess.PIPE)
    _, err = await proc.communicate()
    return proc.returncode == 0, err.decode(errors="replace").strip()[:200]


class ReflexFirstAgent:
    """The wrapped agent's contract, with a bare app launch answered by ``open -a``."""

    def __init__(self, make_inner: Callable[[], Any], on_event: Callable[[AgentEvent], None], *,
                 apps: Callable[[], Iterable[str]] = task_router.installed_apps,
                 asker: Optional[Callable[[str, list[str]], str]] = None,
                 opener: Callable[[str], Any] = open_app, enabled: bool = True,
                 on_done: Callable[[], None] = lambda: None, provider: str = "codex",
                 quit_asker: Optional[Callable[[str, list[str]], str]] = None,
                 quitter: Callable[[str], Any] = quit_app,
                 quit_everything: Optional[Callable[[], Any]] = None,
                 make_auto: Optional[Callable[[], Any]] = None,
                 route_body: Optional[Callable[[str], Awaitable[str]]] = None,
                 bodies: Optional[dict[str, Callable[[], Any]]] = None,
                 desk: Optional[Callable[[str], Awaitable[Optional[dict[str, Any]]]]] = None) -> None:
        self._make_inner, self._inner, self._on_event = make_inner, None, on_event
        self._apps, self._asker, self._opener, self._enabled = apps, asker, opener, enabled
        self._on_done, self.provider = on_done, provider
        self._quit_asker, self._quitter = quit_asker, quitter
        self._quit_everything = quit_everything or (lambda: quit_all(quit_keep()))
        # route_body names the body for a task the reflex declined: a name in ``bodies`` (e.g. "web",
        # browser_router.py), any other non-Codex name the one ``make_auto`` body (the Chrome lane), else Codex
        self._make_auto, self._route_body, self._bodies = make_auto, route_body, dict(bodies or {})
        # desk_lane.DeskLane.attempt: keyword-decided clicks on the frontmost window before any body (desk_lane.py)
        self._desk = desk
        self.body = "codex"
        self._reflex_running = False
        self._cancel_reason: Optional[str] = None
        self.reflexed: Optional[str] = None               # the app opened here, for the log and the tests

    @property
    def running(self) -> bool:
        return self._reflex_running or bool(getattr(self._inner, "running", False))

    async def _choose_body(self, goal: str) -> Any:
        """Codex, unless another body is wired in and the router sends this task there. Never raises."""
        if (self._make_auto is not None or self._bodies) and self._route_body is not None:
            try:
                body = await self._route_body(goal)
            except Exception as e:  # noqa: BLE001 — a router that fails keeps the task with Codex
                log.warning("app-reflex: the body router failed (%s); Codex takes it", type(e).__name__)
                body = "codex"
            make = self._bodies.get(body) or (self._make_auto if body and body != "codex" else None)
            if make is not None:
                agent = make()
                self.body = body
                self.provider = str(getattr(agent, "provider", "") or body)
                log.info("app-reflex: %s takes this task", self.provider)
                return agent
        return self._make_inner()

    def cancel(self, reason: str = "") -> None:
        if self._inner is None:
            self._cancel_reason = reason                  # before the handoff: the wrapped agent never starts
        else:
            self._inner.cancel(reason=reason)

    def steer(self, text: str) -> bool:
        return self._inner is not None and self._inner.steer(text)

    def status(self) -> dict[str, Any]:
        if self._inner is None:
            return {"running": self.running, "provider": self.provider, "goal": "", "reflex": self.reflexed}
        return self._inner.status()

    def __getattr__(self, name: str) -> Any:              # browser_used, browser_screenshot, final …
        if name.startswith("_") or self.__dict__.get("_inner") is None:
            raise AttributeError(name)
        return getattr(self._inner, name)

    async def _plan(self, goal: str) -> task_router.Plan:
        apps = tuple(await asyncio.to_thread(lambda: tuple(self._apps())))
        # classify is pure unless Jev is asked; Jev is a blocking HTTP call, so off the loop.
        return await asyncio.to_thread(task_router.classify, goal, apps=apps, model=self._asker,
                                       quit_model=self._quit_asker)

    async def run(self, goal: str, note: str = "") -> str:
        """``goal`` is the owner's request and the only text the reflex and the body router read. ``note`` is
        the door's own instruction to the body (telegram.TASK_FILE_HINT), added after the route is chosen:
        read with the goal, its "open it in a browser" sent every Telegram task to the Chrome lane and kept
        "open Spotify" from being a bare launch (production, 2026-09-26)."""
        if self._enabled:
            self._reflex_running = True
            try:
                plan = await self._plan(goal)
                if plan.complete and plan.kind in ("quit", "quit_all"):
                    t0 = time.perf_counter()
                    final = await (self._quitter(plan.app) if plan.kind == "quit" else self._quit_everything())
                    self.reflexed = plan.app or "*"
                    log.info("app-reflex: %s %s in %.2f s (%s)", plan.kind, plan.app or "all",
                             time.perf_counter() - t0, "; ".join(plan.reasons))
                    self._on_event(AgentEvent("started", goal))
                    self._on_event(AgentEvent("final", final))
                    return final
                if plan.kind == "launch" and plan.complete and plan.app:
                    t0 = time.perf_counter()
                    ok, err = await self._opener(plan.app)
                    if ok:
                        self.reflexed = plan.app
                        log.info("app-reflex: opened %s in %.2f s (%s)", plan.app, time.perf_counter() - t0,
                                 "; ".join(plan.reasons))
                        final = f"Opened {plan.app}."
                        self._on_event(AgentEvent("started", goal))
                        self._on_event(AgentEvent("final", final))
                        return final
                    log.warning("app-reflex: open -a %s failed (%s); Codex takes it", plan.app, err)
            except Exception as e:  # noqa: BLE001 — the reflex never costs the request
                log.warning("app-reflex: skipped (%s)", type(e).__name__)
            finally:
                self._reflex_running = False
        if self._cancel_reason is not None:
            self._on_event(AgentEvent("cancelled", "Stopped."))
            return "Stopped."
        if self._desk is not None:
            from .desk_lane import codex_note

            self._reflex_running = True
            try:
                lane = await self._desk(goal)
            except Exception as e:  # noqa: BLE001 — the lane never costs the request
                log.warning("app-reflex: the desk lane failed (%s); a body takes it", type(e).__name__)
                lane = None
            finally:
                self._reflex_running = False
            if lane is not None and lane.get("status") == "complete" and lane.get("sentence"):
                self.body, self.provider, self.reflexed = "desk", "desk lane", "desk"
                final = str(lane["sentence"])
                self._on_event(AgentEvent("started", goal))
                self._on_event(AgentEvent("final", final))
                return final
            if lane is not None and lane.get("status") == "partial":
                note = codex_note(lane) + note
            if self._cancel_reason is not None:
                self._on_event(AgentEvent("cancelled", "Stopped."))
                return "Stopped."
        self._inner = await self._choose_body(goal)
        # The goal's shape, never its words: enough to tell later whether a link reached the body.
        log.info("app-reflex: %s gets a goal of %s", self.provider, goal_shape(goal))
        try:
            return await self._inner.run(goal + note)
        finally:
            self._on_done()
