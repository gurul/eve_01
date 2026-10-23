"""eve lite's daemon: the parts behind eve's web app, and nothing of full eve's Mac (docs/plans/eve-lite.md M1, WP-9).

The owner, 2026-10-13: "create a variant of eve that can be one click onboarding … use composio and openrouter as the
only setup things", and "this is so that people dont need to have xcode and stuff setup". Under EVE_PROFILE=lite,
``eve-bridge daemon`` runs ``run_daemon`` (cli.py; the lite LaunchAgent pins the profile). Full eve's daemon.py is never
built or imported: its run starts BLE, the ears, vision, learning, Codex and the Mini App. ``LiteDaemon`` builds:

* the IPC server, on a 0600 Unix socket in ~/.eve-lite (profile.LITE_DEFAULTS), for ``eve-bridge web`` and ``status``;
* memory, by the steps of full eve's ``Daemon._build_memory`` and ``_start_dream``, copied here (``build_memory``,
  ``start_dream``) because daemon.py is not imported; test_lite_memory_parity.py holds the copies to the originals;
* the chat log, and the watcher, whose alerts land in the chat (``watcher.notify = inlet.tell_owner``);
* a ConnectBridge (composio_connect.py) once Composio is signed in, that is once composio-connect.json exists;
* the brain: telegram.py's inlet, app-only (telegram.configured gives eve lite its own), lent memory, the chat log, the
  watcher, the apps and think_hard, and none of the Mac: no agent, no screen, a launcher that refuses, no recent
  folders, no Claude Code sessions, no Codex folders;
* exactly one LinkChat, whose uploads are the inlet's; PhoneCalls on the same brain, a call proving itself with its
  WebRTC peer's own secret (lite_rtc.py), never with Telegram's initData; the RtcHub over both;
* the Mac's page (lite_web.LocalServer), always; the LAN door once a phone was added (LocalServer opens it again when
  web.json says it was on, and the page or ``eve-bridge web lan`` turns it on and off);
* the providers' spend figures (spend_sync.loop) and the nightly dream.

States. NEEDS_OPENROUTER while there is no model key: the setup page and the IPC, and nothing else. READY once there
is one: everything above. The state is decided once, at start; a sign-in moves it on by restarting.

Restarts. A sign-in that writes a credential (the OpenRouter key into the env file, Composio's composio-connect.json)
makes lite_web call ``restart``: the daemon stops gracefully (the page's open streams end, the WebRTC peers close, the
brain says goodbye, the memory closes its conversation and waits for the dream) and exits 75 (EXIT_RESTART). launchd
starts it again (the lite plist: KeepAlive {SuccessfulExit: false}, ThrottleInterval 5), now with the credential. A
stop (SIGTERM, SIGINT) exits 0, which launchd leaves alone.

Full eve on this Mac. While full eve's LaunchAgent (FULL_LABEL) is installed for this user, eve lite logs one line and
exits 0 before anything starts, so launchd leaves it down: one edition per macOS user (both keep their data in
~/.config/eve-bridge).

The firewall. ``run_daemon`` installs profile.py's process firewall before it builds anything (only logging is set up
first, so the firewall's own line is kept), and it stays for the life of the process: no developer tool, no other
program but pmset, ifconfig and scutil, and no private folder.

IPC: one JSON line each way (M11), on the 0600 socket only. A TCP address is not taken (``ipc_path``): any account on
the Mac could reach a port, and ``web_login`` signs a browser in.

    {"evt": "web_login"}                 -> {"ok": true, "code": <one-time code, 2 minutes>, "port": <the page's>}
    {"evt": "web_login_used", "code"}    -> {"ok": true, "used": <whether a browser signed in with it>}
    {"evt": "status"}                    -> {"ok": true, "state", "openrouter", "chat", "composio", "apps", "voice",
                                             "page", "port", "lan", "lan_port", "local_network", "restarting",
                                             "firewall": {<program or folder>: <times refused>}}
    {"evt": "devices"}                   -> {"ok": true, "devices": [{"id", "label", "host", "created", "last_seen"}]}
    {"evt": "revoke", "id": <id or all>} -> {"ok": true, "revoked": [<ids>]}
    {"evt": "lan", "on": <bool>}         -> {"ok": true, "lan": <whether the LAN door is open now>}

A refusal is {"ok": false, "error": <one plain line>}. No request, code, key or token reaches a log: lines carry
states, counts, ports and exception types (AGENTS rule 9). The model key is handed to telegram.hide_token at start,
and httpx, httpcore, aioice and aiortc log at WARNING (QUIET_LOGGERS): their INFO lines name every URL and every
address and candidate.
"""

from __future__ import annotations

import asyncio
import contextlib
import itertools
import logging
import os
import signal
import threading
from pathlib import Path
from typing import Any, Awaitable, Callable, Mapping, Optional

from . import chat_log as chat_log_mod
from . import (
    composio_connect,
    ipc,
    link_chat,
    lite_rtc,
    lite_web,
    mem0_memory,
    onboarding,
    openrouter,
    phone_call,
    profile,
    spend_sync,
    telegram,
    think,
    web_auth,
)
from . import dream as dream_mod
from . import memory as memory_mod
from . import recall as recall_mod
from . import records as records_mod
from . import transcripts as transcripts_mod
from . import watch as watch_mod
from .lite_tls import LocalTls

log = logging.getLogger(__name__)

# ---- exit codes (launchd reads them: the lite plist restarts on any but 0) ------------------------------------------

EXIT_OK = 0              # stopped, or full eve is installed for this user: launchd leaves eve lite down
EXIT_IN_USE = 2          # another daemon holds eve lite's IPC socket (as cli._run_daemon)
EXIT_RESTART = 75        # EX_TEMPFAIL: a sign-in saved a credential; launchd starts eve lite again into it
EXIT_CONFIG = 78         # EX_CONFIG: EVE_PROFILE is not lite (the CLI exits 78 on profile.ProfileError too)

READY, NEEDS_OPENROUTER = lite_web.READY, lite_web.NEEDS_OPENROUTER

# ---- where ----------------------------------------------------------------------------------------------------------

FULL_LABEL = "com.github.eve-bridge.daemon"                      # full eve's LaunchAgent (_service_launchd.LABEL)
FULL_PLIST = Path("~/Library/LaunchAgents") / f"{FULL_LABEL}.plist"
SOCK_ENV = "EVE_BRIDGE_SOCK"                                     # the IPC address (profile.LITE_DEFAULTS)
DEVICES_FILE = Path("~/.config/eve-bridge/web-devices.json")     # web_auth.Devices: token hashes, 0600
TLS_DIR = Path("~/.config/eve-bridge/web-tls")                   # lite_tls.LocalTls: the CA and the LAN certificate
SOCKET_MAX_BYTES = 103                                           # macOS keeps a socket's path in 104 bytes, NUL included

# ---- how long, and the log --------------------------------------------------------------------------------------

CLOSE_MARKERS_SECS = 3.0           # shutdown: the conversation the brain left open is closed within this (daemon.py)
DRAIN_SECS = 12.0                  # and background work (the dream) is given this before it is cut off (daemon.py)
PAGE_RETRY_SECS = (10.0, 30.0, 60.0)   # every port of the Mac's page taken: tried again after these, the last repeating
PAGE_STOP_SECS = 5.0               # shutdown: a page start already under way is let finish this long, then stopped
QUIET_LOGGERS = ("httpx", "httpcore", "aioice", "aiortc")
LOG_FORMAT = "%(asctime)s %(levelname)s %(name)s: %(message)s"   # full eve's daemon's (cli._run_daemon)

# What `eve-bridge web` and `status` show: one plain line each.
PAGE_DOWN = "eve's page is not up on this Mac yet. Try again in a minute."
STOPPING = "eve is stopping on this Mac right now. Try again in a few seconds."
UNKNOWN_EVENT = "eve lite does not take that request."


# ---- what the brain is lent instead of the Mac -------------------------------------------------------------------

def _no_screen() -> None:
    """The inlet's screen in eve lite: none (eve lite never asks for Screen Recording)."""
    return None


def _nothing() -> list[Any]:
    """No recent coding folders, no Claude Code sessions, no Codex folders: eve lite has none of them."""
    return []


def _no_work() -> None:
    """LinkChat's Work sessions in eve lite: none (lite_web answers 404 for their routes before LinkChat would)."""
    return None


def _no_owner(init_data: str) -> None:
    """PhoneCalls' Telegram check in eve lite: no initData belongs to anyone. A call proves itself with its WebRTC
    peer's own secret instead (lite_rtc.RtcCallSocket's hello, PhoneCalls.serve_socket's token_check)."""
    return None


async def _refuse_launch(folder: Path, harness: str) -> str:
    """The inlet's launcher in eve lite: a coding session is not part of eve lite (no Claude Code, no Codex), so nothing
    opens, and the line says what eve lite does instead. Its code words and tool are refused before this anyway."""
    return telegram.LITE_NOT_HERE


# ---- memory: full eve's steps, copied ------------------------------------------------------------------------------

def build_memory(cfg: recall_mod.RecallConfig) -> Optional[memory_mod.Memory]:
    """The memory stack, or None while memory is off: the steps of full eve's ``Daemon._build_memory`` (daemon.py),
    copied, since eve lite never imports daemon.py. test_lite_memory_parity.py holds the copy to the original, call
    for call and file for file. Synchronous (renames and a few git calls): run it on a worker thread. In eve lite
    records.py starts no git (toolchain.git_usable is False there), so ensure_repo says in one line that the records
    keep no history."""
    if not memory_mod.enabled():
        log.info("memory: off (EVE_MEMORY) — nothing said is kept, and prompts carry no memory")
        return None
    try:
        memory_mod.migrate(cfg, recall_mod.legacy_store(), recall_mod.legacy_mem0())
    except Exception as e:  # noqa: BLE001 - a failed move leaves the old store where it was
        log.warning("memory: the move to the memory folder failed (%s) — the old store is untouched",
                    type(e).__name__)
    records_mod.ensure_repo(cfg.records_dir)
    transcripts = transcripts_mod.Transcripts(transcripts_mod.configured(), archive=cfg.archive_dir)
    index = mem0_memory.shared(cfg)
    log.info("memory: on — transcripts, records%s", ", mem0 index" if index is not None else " (no mem0 index)")
    if index is not None and mem0_memory.embedder_of() == "gemma":
        # the local embedder loads in ~3 s; loaded now, the first search is not the one that waits
        threading.Thread(target=index.warm, name="mem0-warm", daemon=True).start()
    return memory_mod.Memory(cfg, transcripts, index, background=True)   # only the daemon runs the index worker


def start_dream(memory: Optional[memory_mod.Memory], cfg: recall_mod.RecallConfig, shutdown: asyncio.Event,
                background: set[asyncio.Task[Any]]) -> Optional[asyncio.Task[Any]]:
    """The nightly dream, held in ``background`` so shutdown drains it instead of cancelling it: full eve's
    ``Daemon._start_dream`` (daemon.py), copied. No memory or no model client (records.make_client logs why): no
    dream."""
    if memory is None:
        return None
    client = records_mod.make_client()
    if client is None:
        return None
    dreamer = dream_mod.Dreamer(cfg, memory.transcripts, client, memory.index)
    task = asyncio.create_task(dreamer.loop(shutdown), name="memory-dream")
    background.add(task)
    task.add_done_callback(background.discard)
    return task


# ---- the parts ------------------------------------------------------------------------------------------------------

def make_bridge(token_path: Path) -> Optional[composio_connect.ConnectBridge]:
    """The person's apps through Composio Connect once they signed in (``token_path``, composio-connect.json, exists:
    onboarding writes it only once a token arrived), else None. Not started here: ``start`` lists the tools on a thread
    of its own, trying again until Composio answers."""
    try:
        signed_in = token_path.is_file()
    except OSError:
        signed_in = False
    if not signed_in:
        log.info("apps: Composio is not signed in on this Mac; the web app's Settings, Apps signs in")
        return None
    return composio_connect.ConnectBridge(composio_connect.ConnectTokenSource(token_path))


def full_eve_installed() -> bool:
    """Whether full eve's LaunchAgent is installed for this user (its plist is in ~/Library/LaunchAgents): eve lite
    then stays down (module docstring)."""
    try:
        return FULL_PLIST.expanduser().exists()
    except OSError:
        return False


def ipc_path(spec: Optional[str] = None) -> Path:
    """The IPC socket's path: ``spec`` (the CLI's --socket), else EVE_BRIDGE_SOCK, else eve lite's default in
    ~/.eve-lite. Only a socket file is taken: a TCP address (``host:port``) is replaced by the default, with one line,
    because any account on the Mac could connect to a port and ``web_login`` signs a browser in."""
    default = Path(profile.LITE_DEFAULTS[SOCK_ENV]).expanduser()
    chosen = str(spec or os.environ.get(SOCK_ENV) or "").strip()
    if not chosen:
        return default
    try:
        socket_file = isinstance(ipc.parse_spec(chosen), ipc.UnixTransport)
    except (ValueError, RuntimeError):
        socket_file = False
    if not socket_file:
        log.warning("lite: the IPC address must be a socket file in eve lite, not a TCP port; using eve lite's own")
        return default
    return Path(chosen).expanduser()


def setup_logging(level: Any = "INFO") -> None:
    """The daemon's log, written as full eve's daemon writes it (launchd sends it to ~/Library/Logs/eve-lite.log),
    with QUIET_LOGGERS at WARNING: httpx and httpcore name every URL at INFO, and aioice every address and candidate."""
    named = logging.getLevelName(str(level or "INFO").strip().upper())
    logging.basicConfig(level=named if isinstance(named, int) else logging.INFO, format=LOG_FORMAT)
    for name in QUIET_LOGGERS:
        logging.getLogger(name).setLevel(logging.WARNING)


def _private_folder(folder: Path) -> None:
    """The socket's folder, made 0700 when it is missing (~/.eve-lite: the installer makes it so). A folder that is
    there is left as it is: it may be one this daemon does not own."""
    if not folder.is_dir():
        folder.mkdir(mode=0o700, parents=True, exist_ok=True)


# ---- the daemon -----------------------------------------------------------------------------------------------------

class LiteDaemon:
    """eve lite's daemon (module docstring): ``run`` builds it, serves until ``shutdown`` or ``restart``, stops it in
    order and returns the exit code. Every part is read from this process's environment and home, as full eve's
    daemon reads them. The keywords are for tests and their stand-ins: the IPC socket's path, WebContext's own keywords
    (ports 0, fakes for macOS and the network), the two sign-ins, and ``on_ready``, called with the daemon once it
    serves."""

    def __init__(self, *, socket_path: Optional[str] = None, web_options: Optional[Mapping[str, Any]] = None,
                 openrouter_login: Any = None, composio_login: Any = None,
                 on_ready: Optional[Callable[["LiteDaemon"], Any]] = None) -> None:
        self.socket_path = ipc_path(socket_path)
        self.web_options = dict(web_options or {})
        self.on_ready = on_ready
        self.state = NEEDS_OPENROUTER
        self.exit_code = EXIT_OK
        self.recall_cfg = recall_mod.configured()
        self.connect_path = Path(onboarding.CONNECT_TOKEN_FILE).expanduser()
        # who may use the web app, and how a browser proves it (web_auth.py, lite_tls.py)
        self.devices = web_auth.Devices(DEVICES_FILE.expanduser())
        self.pairing = web_auth.PairingWindow()
        self.mac_login = web_auth.MacLogin()
        self.tls = LocalTls(TLS_DIR.expanduser())
        # the two sign-ins (onboarding.py): the setup page's, in either state
        self.openrouter_login = openrouter_login if openrouter_login is not None else onboarding.OpenRouterLogin()
        self.composio_login = (composio_login if composio_login is not None
                               else onboarding.ComposioConnectLogin(self.connect_path))
        # the composition (READY only; None until run builds it, and for good in NEEDS_OPENROUTER)
        self.memory: Optional[memory_mod.Memory] = None
        self.chat_log: Optional[chat_log_mod.ChatLog] = None
        self.watcher: Optional[watch_mod.Watcher] = None
        self.apps: Optional[composio_connect.ConnectBridge] = None
        self.inlet: Optional[telegram.TelegramInlet] = None
        self.link: Optional[link_chat.LinkChat] = None
        self.calls: Optional[phone_call.PhoneCalls] = None
        self.rtc: Optional[lite_rtc.RtcHub] = None
        # serving
        self.web: Optional[lite_web.WebContext] = None
        self.local: Optional[lite_web.LocalServer] = None
        self.ipc: Optional[ipc.IPCServer] = None
        self._shutdown = asyncio.Event()
        self._loop: Optional[asyncio.AbstractEventLoop] = None    # run's, so a stop from another thread lands on it
        self._tasks: list[asyncio.Task[Any]] = []
        self._page_task: Optional[asyncio.Task[Any]] = None       # _keep_page_up, while every page port is taken
        self._ipc_task: Optional[asyncio.Task[Any]] = None
        # work that must outlive the call that started it (the dream): drained at shutdown, not cancelled
        self._background: set[asyncio.Task[Any]] = set()

    def __repr__(self) -> str:
        return f"LiteDaemon({self.state})"

    # -- the lifecycle --
    async def run(self) -> int:
        """Build (``_build``), serve (``_serve``) until ``shutdown`` or ``restart``, then stop in order (``_stop``),
        whatever happened before. The exit code (``exit_code``)."""
        self._loop = asyncio.get_running_loop()
        try:
            await self._build()
            if not self.stopping:                # a stop asked for while it was building: nothing more starts
                await self._serve()
                self._ready()
            await self._shutdown.wait()
        finally:
            await self._stop()
        return self.exit_code

    def shutdown(self) -> None:
        """Stop (SIGTERM, SIGINT): exit 0, unless a restart was asked for first. From any thread."""
        self._wake()

    def restart(self) -> None:
        """WebContext.restart: a sign-in saved its credential. Stop gracefully and exit 75, so launchd starts eve lite
        again into it (module docstring). Asked twice, it is one restart. From any thread."""
        if self.exit_code != EXIT_RESTART:
            self.exit_code = EXIT_RESTART
            log.info("lite: a sign-in was saved; eve lite restarts into it (exit %d)", EXIT_RESTART)
        self._wake()

    def _wake(self) -> None:
        """Set the shutdown event on the daemon's own loop: an asyncio.Event set from another thread would not wake
        the loop's waiter."""
        loop = self._loop
        if loop is not None and not loop.is_closed():
            try:
                here: Optional[asyncio.AbstractEventLoop] = asyncio.get_running_loop()
            except RuntimeError:
                here = None
            if here is not loop:
                loop.call_soon_threadsafe(self._shutdown.set)
                return
        self._shutdown.set()

    @property
    def stopping(self) -> bool:
        return self._shutdown.is_set()

    # -- building --
    async def _build(self) -> None:
        """READY with a model key: everything in the module docstring. NEEDS_OPENROUTER without one: nothing here,
        the setup page alone (``_serve``)."""
        key = openrouter.key()
        if not key:
            self.state = NEEDS_OPENROUTER
            log.info("lite: no OpenRouter key yet, so the setup page only, until the sign-in")
            return
        telegram.hide_token(key)                 # the key never reaches a log line, whoever writes it
        try:
            self.memory = await asyncio.to_thread(build_memory, self.recall_cfg)
        except Exception as e:  # noqa: BLE001 - memory is a nicety: eve lite runs without it, as full eve does
            log.warning("memory: could not open (%s) — running without memory", type(e).__name__)
            self.memory = None
        memory = self.memory
        try:
            self.watcher = watch_mod.make_watcher()
        except Exception as e:  # noqa: BLE001 — a watcher that cannot start costs the watching, never the daemon
            log.warning("watch: could not start (%s); watching is off this run", type(e).__name__)
            self.watcher = None
        self.chat_log = chat_log_mod.open_configured()
        if self.chat_log is not None and memory is not None:
            memory.chat_log = self.chat_log      # forget reaches the chat log through the memory
        self.apps = make_bridge(self.connect_path)
        recall_cfg = self.recall_cfg
        # A brain or a voice that cannot be built costs the chat or the calls, never the setup page: a daemon that
        # failed here would be started again by launchd every 5 s, and nobody could reach its page.
        try:
            self.inlet = telegram.make_inlet(
                telegram.configured(), memory=memory, chat_log=self.chat_log, watcher=self.watcher, apps=self.apps,
                thinker=think.make_thinker(think.configured()),
                brief=lambda: recall_mod.opening_brief(recall_cfg, memory.transcripts if memory is not None else None),
                agent_factory=None, agent_enabled=False, screen=_no_screen, launcher=_refuse_launch,
                launch_recent=_nothing, claude_sessions=_nothing, codex_folders=_nothing)
        except Exception as e:  # noqa: BLE001 — the type only
            log.warning("lite: the chat brain could not be built (%s)", type(e).__name__)
            self.inlet = None
        # the one chat door: the Mac's page and every phone reach this LinkChat, over HTTP or the WebRTC link channel
        self.link = link_chat.LinkChat(lambda: self.inlet, work=_no_work)
        if self.inlet is not None:
            self.inlet.link_uploads = self.link.uploads   # eve's own pictures and files land in the door's store
            if self.watcher is not None:
                self.watcher.notify = self.inlet.tell_owner
        else:
            log.warning("lite: the chat brain is off this run, so the chat answers that it is not running")
        try:
            voice = phone_call.make_voice()
        except Exception as e:  # noqa: BLE001 — the type only
            log.warning("call: the voice could not be built (%s); calls are off", type(e).__name__)
            voice = None
        self.calls = phone_call.PhoneCalls(_no_owner, self.inlet, voice)
        self.rtc = lite_rtc.RtcHub(self.link, self.calls)
        self.state = READY

    # -- serving --
    async def _serve(self) -> None:
        """The Mac's page (and the LAN door when web.json says it was on), then the IPC socket, then the work that
        runs beside them: the brain, the watcher, the spend figures, the dream and Composio's start."""
        self.web = lite_web.WebContext(
            self.link, self.rtc, self.devices, self.pairing, self.mac_login, self.tls, self.openrouter_login,
            self.composio_login, self.apps, self.web_status, self.restart, None, lite_web.WEB_FILE,
            **self.web_options)
        self.local = lite_web.LocalServer(self.web)
        try:
            await self.local.start()
        except OSError as e:                        # PortsTaken: every port the page may use is taken now
            log.warning("lite: the Mac's page did not start (%s); trying again in %.0f s", type(e).__name__,
                        PAGE_RETRY_SECS[0])
            self._page_task = self._spawn(self._keep_page_up(), "page")
        await self._start_ipc()
        if self.inlet is not None:
            self._spawn(self.inlet.run(), "brain")
            if self.watcher is not None:
                self._spawn(self.watcher.run(), "watch")
        if self.state == READY:
            # the providers' own spend figures, at start and hourly; Settings shows them beside eve's own meter
            self._spawn(spend_sync.loop(self._shutdown), "spend-sync")
            # the nightly dream: the one automatic writer of the records and the mem0 index, drained at shutdown
            start_dream(self.memory, self.recall_cfg, self._shutdown, self._background)
        if self.apps is not None:
            self.apps.start()

    async def _start_ipc(self) -> None:
        """The IPC server on its 0600 socket (ipc.UnixTransport chmods it; its folder is made 0700). One that cannot
        open is a warning, not a stop: the page still serves the browsers already signed in."""
        path = self.socket_path
        if len(os.fsencode(str(path))) > SOCKET_MAX_BYTES:
            log.warning("lite: the IPC socket's path is longer than macOS takes (%d bytes); `eve-bridge web` cannot "
                        "reach eve", len(os.fsencode(str(path))))
            return
        server = ipc.IPCServer(self.handle_ipc, socket_path=str(path))
        try:
            _private_folder(path.parent)
            await server.start()
        except OSError as e:
            log.warning("lite: the IPC socket did not open (%s); `eve-bridge web` cannot reach eve", type(e).__name__)
            return
        self.ipc = server
        self._ipc_task = asyncio.create_task(server.serve_forever(), name="lite-ipc")
        self._ipc_task.add_done_callback(self._ended)

    async def _keep_page_up(self) -> None:
        """Every port the Mac's page may use was taken at start: try again after PAGE_RETRY_SECS, until one is free or
        eve stops."""
        assert self.local is not None
        for attempt, wait in enumerate(itertools.chain(PAGE_RETRY_SECS, itertools.repeat(PAGE_RETRY_SECS[-1]))):
            with contextlib.suppress(TimeoutError):
                await asyncio.wait_for(self._shutdown.wait(), wait)
                return
            try:
                await self.local.start()
            except OSError as e:
                log.log(logging.INFO if attempt else logging.WARNING,
                        "lite: the Mac's page did not start yet (%s)", type(e).__name__)
                continue
            return

    def _ready(self) -> None:
        local = self.local
        log.info("lite: %s; the Mac's page %s; memory %s, apps %s, voice %s", self.state,
                 f"on port {local.port}" if local is not None and local.running else "not up yet",
                 "on" if self.memory is not None else "off",
                 "signed in" if self.apps is not None else "not signed in",
                 "ready" if self.rtc is not None and self.rtc.voice_ready() else "off")
        if self.on_ready is not None:
            try:
                self.on_ready(self)
            except Exception as e:  # noqa: BLE001 — the type only
                log.warning("lite: on_ready failed (%s)", type(e).__name__)

    def _spawn(self, coro: Awaitable[Any], name: str) -> asyncio.Task[Any]:
        task = asyncio.ensure_future(coro)
        task.set_name(f"lite-{name}")
        task.add_done_callback(self._ended)
        self._tasks.append(task)
        return task

    @staticmethod
    def _ended(task: asyncio.Task[Any]) -> None:
        """A part's task that stopped on its own with an error: said by type (its own lines say why)."""
        if task.cancelled():
            return
        e = task.exception()
        if e is not None:
            log.warning("lite: %s stopped (%s)", task.get_name(), type(e).__name__)

    # -- stopping --
    async def _stop(self) -> None:
        """Everything that started, stopped in order, each step on its own, so one that fails (logged by its type)
        leaves the rest to run: the Mac's page first (LinkChat's open streams end with their [DONE], the LAN door
        closes), then the WebRTC peers and their calls, then the brain (its own goodbye: TelegramInlet's shutdown), the
        watcher and the spend figures, then Composio's thread; then memory (the conversation the brain left open is
        closed, and the dream is given DRAIN_SECS); the IPC socket last."""
        page_task = self._page_task
        if page_task is not None and not page_task.done():
            # it ends by itself once it sees the stop; a start it is in the middle of finishes first, so the page's
            # sockets are already open when the page is stopped below, and none opens after it
            await asyncio.wait({page_task}, timeout=PAGE_STOP_SECS)
        if self.local is not None:
            await self._step("the page", self.local.stop())
        if self.rtc is not None:
            await self._step("the WebRTC peers", self.rtc.close())
        tasks = [task for task in self._tasks if not task.done()]
        for task in tasks:
            task.cancel()
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        if self.apps is not None:
            await self._step("Composio", asyncio.to_thread(self.apps.close))
        await self._step("the open conversation", self._close_open_conversation())
        await self._step("the dream", self._drain_background())
        if self._ipc_task is not None and not self._ipc_task.done():
            self._ipc_task.cancel()
            await asyncio.gather(self._ipc_task, return_exceptions=True)
        if self.ipc is not None:
            await self._step("the IPC socket", self.ipc.stop())
        log.info("lite: stopped%s", " for a restart" if self.exit_code == EXIT_RESTART else "")

    @staticmethod
    async def _step(what: str, step: Awaitable[Any]) -> None:
        try:
            await step
        except Exception as e:  # noqa: BLE001 — shutting down: best effort, the type only
            log.warning("lite: %s did not stop cleanly (%s)", what, type(e).__name__)

    async def _close_open_conversation(self) -> None:
        """A restart must not leave the chat's conversation open in the transcripts: the brain writes its own close
        marker on the way out, and this is the backstop for one that did not (full eve's
        ``Daemon._close_open_conversations``, for the one brain eve lite has). A conversation with nothing in it gets
        nothing."""
        memory, inlet = self.memory, self.inlet
        conv = str(getattr(inlet, "_conv", None) or "") if inlet is not None else ""
        if memory is None or not conv:
            return
        store = memory.transcripts

        def close() -> bool:
            lines = store.conv_lines(conv)
            if not lines or any(ln.get("kind") == "close" for ln in lines):
                return False
            return bool(store.close(telegram.CHANNEL, conv))

        try:
            closed = await asyncio.wait_for(asyncio.to_thread(close), timeout=CLOSE_MARKERS_SECS)
        except Exception as e:  # noqa: BLE001 - shutting down: best effort
            log.warning("memory: close marker not written at shutdown (%s)", type(e).__name__)
            return
        if getattr(inlet, "_conv", None) is not None:
            inlet._conv = None                   # closed above: a late _close_chat must not close it twice
        if closed:
            log.info("memory: the open conversation was closed at shutdown")

    async def _drain_background(self) -> None:
        """Wait up to DRAIN_SECS for background work (the nightly dream) to finish, then cancel whatever is left
        (full eve's ``Daemon._drain_background``)."""
        pending = [t for t in list(self._background) if not t.done()]
        if not pending:
            return
        _, still = await asyncio.wait(pending, timeout=DRAIN_SECS)
        for task in still:
            task.cancel()
        if still:
            await asyncio.gather(*still, return_exceptions=True)
            log.warning("shutdown: %d background task(s) cut off after %.0f s", len(still), DRAIN_SECS)

    # -- what the doors and the CLI read --
    def web_status(self) -> dict[str, Any]:
        """WebContext.status: the state and whether a model key is saved; lite_web reads nothing else from it."""
        return {"state": self.state, "openrouter": openrouter.available()}

    def _composio_live(self) -> bool:
        """Signed in to Composio, and not a sign-in only a new one can fix (as the web app's Settings says it)."""
        try:
            signed_in = self.connect_path.is_file()
        except OSError:
            signed_in = False
        bridge = self.apps
        return signed_in and not (bridge is not None and bool(getattr(bridge, "needs_sign_in", False)))

    def _openrouter_rejected(self) -> bool:
        """Whether the last credit read found the OpenRouter key turned down (a 401). Read from the web's cached
        read, which ``_ipc_status`` warms before it answers, so ``doctor`` sees a fresh answer."""
        web = self.web
        kept = web.shared.credit if web is not None else None
        return bool(kept is not None and lite_web._key_rejected(kept[2]))

    def status(self) -> dict[str, Any]:
        """``{"evt": "status"}``, for ``eve-bridge status`` and ``doctor``: words, booleans, ports and the firewall's
        counts by program or folder name. Never a key, a code, a path or an address."""
        local, web = self.local, self.web
        lan = web.shared.lan if web is not None else None
        page = local is not None and local.running
        lan_on = lan is not None and lan.running
        return {"state": self.state, "openrouter": openrouter.available(),
                "openrouter_rejected": self._openrouter_rejected(), "chat": self.inlet is not None,
                "composio": self._composio_live(), "apps": bool(self.apps is not None and self.apps.started),
                "voice": bool(self.rtc is not None and self.rtc.voice_ready()),
                "page": page, "port": local.port if page and local is not None else 0,
                "lan": lan_on, "lan_port": lan.port if lan_on and lan is not None else 0,
                "local_network": web.shared.local_network if web is not None else "unknown",
                "restarting": self.exit_code == EXIT_RESTART or bool(web is not None and web.shared.restarting),
                "firewall": profile.firewall_counts()}

    # -- IPC (module docstring) --
    async def handle_ipc(self, req: Any) -> dict[str, Any]:
        """One request from ``eve-bridge web``, ``status`` or ``doctor``. Never raises: ipc.IPCServer would log a
        failed request whole, and a login code may be in it."""
        try:
            evt = req.get("evt") if isinstance(req, Mapping) else None
            handler = _IPC.get(evt) if isinstance(evt, str) else None
            if handler is None:
                return {"ok": False, "error": UNKNOWN_EVENT}
            return await handler(self, req)
        except Exception as e:  # noqa: BLE001 — the type only
            log.warning("lite: an IPC request failed (%s)", type(e).__name__)
            return {"ok": False, "error": lite_web.FAILED}

    async def _ipc_web_login(self, req: Mapping[str, Any]) -> dict[str, Any]:
        """A one-time code for ``http://localhost:<port>/#login=<code>``: ``eve-bridge web open`` hands it to the
        browser in a 0600 file, never on the screen."""
        local = self.local
        if self.stopping:
            return {"ok": False, "error": STOPPING}
        if local is None or not local.running:
            return {"ok": False, "error": PAGE_DOWN}
        return {"ok": True, "code": self.mac_login.issue(), "port": local.port}

    async def _ipc_web_login_used(self, req: Mapping[str, Any]) -> dict[str, Any]:
        return {"ok": True, "used": self.mac_login.used(req.get("code"))}

    async def _ipc_status(self, req: Mapping[str, Any]) -> dict[str, Any]:
        # Read the OpenRouter credit now, so ``openrouter_rejected`` is fresh for ``doctor`` even when no browser
        # has polled (it is bounded by CREDIT_WAIT_SECS and never raises). Skipped while eve restarts into a key.
        if (self.local is not None and openrouter.available()
                and not (self.web is not None and self.web.shared.restarting)):
            with contextlib.suppress(Exception):
                await self.local._credit()
        return {"ok": True, **self.status()}

    async def _ipc_devices(self, req: Mapping[str, Any]) -> dict[str, Any]:
        return {"ok": True, "devices": await asyncio.to_thread(self.devices.list)}

    async def _ipc_revoke(self, req: Mapping[str, Any]) -> dict[str, Any]:
        """Forget one device by its id, or every device with "all"; their WebRTC peers close (LocalServer.revoke)."""
        raw = req.get("id")
        target = raw.strip().lower() if isinstance(raw, str) else ""
        if not (target == "all" or web_auth.DEVICE_ID.fullmatch(target)):
            return {"ok": False, "error": lite_web.DEVICE_ID}
        if self.local is None:
            return {"ok": False, "error": PAGE_DOWN}
        try:
            gone = await self.local.revoke(target)
        except OSError as e:
            log.warning("lite: a revoke was not saved (%s)", type(e).__name__)
            return {"ok": False, "error": lite_web.NOT_SAVED}
        return {"ok": True, "revoked": gone}

    async def _ipc_lan(self, req: Mapping[str, Any]) -> dict[str, Any]:
        """The LAN door on or off, remembered in web.json (LocalServer.set_lan: off also closes a pairing window and
        the peers that came through the door)."""
        on = req.get("on")
        if not isinstance(on, bool):
            return {"ok": False, "error": lite_web.LAN_FLAG}
        if self.local is None:
            return {"ok": False, "error": PAGE_DOWN}
        try:
            await self.local.set_lan(on)
        except lite_web.PortsTaken as e:
            log.warning("lite: the LAN door did not open (%s)", type(e).__name__)
            return {"ok": False, "error": lite_web.LAN_FAILED}
        except (OSError, ValueError) as e:          # the certificate could not be made or loaded
            log.warning("lite: the LAN certificate is not usable (%s)", type(e).__name__)
            return {"ok": False, "error": lite_web.TLS_OFF}
        return {"ok": True, "lan": self.local.lan_on()}


_IPC: dict[str, Callable[[LiteDaemon, Mapping[str, Any]], Awaitable[dict[str, Any]]]] = {
    "web_login": LiteDaemon._ipc_web_login,
    "web_login_used": LiteDaemon._ipc_web_login_used,
    "status": LiteDaemon._ipc_status,
    "devices": LiteDaemon._ipc_devices,
    "revoke": LiteDaemon._ipc_revoke,
    "lan": LiteDaemon._ipc_lan,
}


# ---- the process ----------------------------------------------------------------------------------------------------

def run_daemon(args: Any = None, **options: Any) -> int:
    """``eve-bridge daemon`` in eve lite (cli.py calls it under EVE_PROFILE=lite): logging, then the process firewall
    (profile.install_firewall, for the life of the process), then LiteDaemon until it stops. The exit code: EXIT_OK
    when it was stopped or full eve is installed for this user, EXIT_RESTART after a sign-in, EXIT_CONFIG outside eve
    lite, EXIT_IN_USE when another daemon has the socket. ``args`` is the CLI's parsed command line (its ``socket`` and
    ``log_level`` are read); ``options`` go to LiteDaemon (tests)."""
    setup_logging(getattr(args, "log_level", None) or "INFO")
    try:
        profile.install_firewall()              # first: before any part of eve lite is built
        lite = profile.is_lite()
    except profile.ProfileError as e:
        log.error("lite: %s", e)
        return EXIT_CONFIG
    if not lite:
        log.error("lite: EVE_PROFILE is not lite here, so eve lite's daemon does not start")
        return EXIT_CONFIG
    if full_eve_installed():
        log.warning("lite: full eve's LaunchAgent (%s) is installed for this user, so eve lite stays down: one edition "
                    "per Mac account. Uninstall full eve's service to run eve lite.", FULL_LABEL)
        return EXIT_OK
    # cli.main applied the profile right after the env file; applied again here, so the daemon runs on eve lite's
    # switches however it was started (idempotent: a second pass changes nothing)
    profile.apply(os.environ)
    path = ipc_path(getattr(args, "socket", None))
    if ipc.UnixTransport(str(path)).is_in_use():
        log.error("lite: another eve daemon is already listening on eve lite's socket; this one stops")
        return EXIT_IN_USE
    daemon = LiteDaemon(socket_path=str(path), **options)
    loop = asyncio.new_event_loop()
    asyncio.set_event_loop(loop)
    for sig in (signal.SIGINT, signal.SIGTERM):
        with contextlib.suppress(NotImplementedError, RuntimeError, ValueError):
            loop.add_signal_handler(sig, daemon.shutdown)
    # a stalled loop cannot log why it stalled; this thread does it from outside, and SIGUSR1 dumps every thread
    from .loop_watchdog import LoopWatchdog, register_dump_signal

    watchdog = LoopWatchdog(loop).start()
    register_dump_signal()
    try:
        return loop.run_until_complete(daemon.run())
    except KeyboardInterrupt:
        return daemon.exit_code
    finally:
        watchdog.stop()
        loop.close()
        asyncio.set_event_loop(None)
