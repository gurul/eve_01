"""The owner's Fire TV: power, volume, the d-pad, play/pause, apps by name, typing, and a look at the screen —
over the LAN, with network ADB.

The owner asked on 2026-10-04 to control their Fire TV smart TV from eve "the most developer-friendly way". That
is ADB over the network: the TV's own debugging bridge, which Amazon documents for Fire TV and which needs no
cloud, no hub and no app on the TV. Eve shells out to Google's ``adb`` (Homebrew's ``android-platform-tools``),
which keeps the key this Mac was allowed with in ``~/.android/adbkey``. No Python ADB library is added: every
one on PyPI is years stale, and the slow part is on the TV, not here.

What each thing costs on the TV was measured on 2026-10-04, and the commands are chosen by it:

==================  =====================================================  ===========  ======================
What                Command on the TV                                      Time         Read back
==================  =====================================================  ===========  ======================
state               ``dumpsys power`` + ``dumpsys window`` + the            ~0.5 s       is the read
                    ``dumpsys audio`` STREAM_MUSIC block, one round trip
play / pause / ...  ``cmd media_session dispatch play-pause`` etc.          ~75 ms       no
volume              ``cmd media_session volume --stream 3 --set N``         ~75 ms       yes, from dumpsys
mute / unmute       ``input keyevent 164`` (a toggle: sent only when the    ~1.2 s       yes, from dumpsys
                    state differs)
d-pad, back, home   ``input keyevent 20 20 23`` — several keys, one call    ~1.2 s       no
power               ``input keyevent 224`` (WAKEUP) / ``223`` (SLEEP)       ~1.2 s       yes, polled
open an app         ``am start -n package/activity``                        ~120 ms      no
YouTube search      ``am start -a VIEW -n <YouTube> -d <results URL>``      ~120 ms      no
type text           ``input text``                                          ~1.2 s       no
screenshot          ``adb exec-out screencap -p``                           ~2.3 s       is the picture
==================  =====================================================  ===========  ======================

``input`` costs about 1.2 s because it starts a Java VM on the TV each time, so it is used only where nothing
faster exists, and a run of key presses ("down three times") goes in one call. Mute is ``input`` and not
``cmd media_session dispatch mute``: that one sends the *microphone* mute key (KEYCODE_MUTE, AOSP
MediaShellCommand), which never reaches the speaker. Power is WAKEUP and SLEEP, which do nothing when the TV is
already there; the POWER key is a toggle and is never sent. The volume is never written by a raw binder call:
one set this TV's volume to 0 on 2026-10-04.

The connection. ``~/.config/eve-bridge/firetv.json`` (mode 600) keeps the TV's address and its mDNS name
(``adb-<serial>``), written by ``eve-bridge firetv pair``. Before the first command, and after a failure,
eve runs ``adb connect`` (whose exit code is always 0, so its words are read) and then ``get-state``, which
must say ``device``. When the TV has gone (offline, not found, a timeout) eve disconnects, finds it again by
its mDNS name (a new DHCP address is saved), connects and retries once, then gives up with a plain line. The retry
is safe when adb said the TV was gone before the command ran; after a timeout the command may already have
landed, so only reads and commands that land the same twice (WAKEUP, SLEEP, a volume level, opening an app) are
sent again: a second ``input text`` would type the words twice. It restarts the adb server only in one case: adb
says the TV cannot be reached while a plain TCP connection to the TV's port opens at once. Then the TV is fine and
the adb server is stale (it lost its route after the Mac's network changed, 2026-10-08: "No route to host" for hours
with the TV on), so ``kill-server`` and ``start-server`` run once and eve connects again. At most once every
ADB_RESTART_EVERY seconds, and never otherwise: something else on the Mac may be using the server. Three outcomes are kept apart, the way the
Muse gadget taught: the TV answered, the TV allowed this Mac, the command was done.

Standby. The TV keeps answering for about 6 minutes after it is turned off; then its chip suspends, and a network
packet waking it from there is unproven. So "turn on the TV" works from the screensaver and from a fresh
standby, and from deep standby says so and suggests the remote or Alexa. HDMI inputs cannot be switched: the
TV's input switcher needs an Amazon-signed permission the shell does not have.

Four doors use one ``FireTV``, the way lights.py and spotify.py are wired:

* **Telegram** (and what rides its brain: calls, the Voice PE, the chat window). A message that is **only** a
  TV command and **names the TV** ("turn on the tv", "tv volume 30", "pause the tv", "open youtube on the tv",
  "what's on the tv") is done by code, no model turn (``FireTV.match``). Anything else goes to the model, which
  has ``firetv_status``, ``firetv_control`` and ``firetv_screenshot``. The screenshot goes to the owner's chat;
  the model does not see the picture, only the TV's state read at the same moment.
* **The robot's live voice** (voice_agent.py) gets status and control but not the screenshot: the picture goes
  to a chat, and a voice conversation has none.
* **The terminal**: ``eve-bridge firetv ...``.
* **The Mini App's remote card** (owner, 2026-10-04: "build tv remote into mini app"): power, the d-pad, back /
  home / menu, the media keys, volume and the app chips, through ``FireTV.panel`` behind ``/api/firetv`` (owner
  only). Each press answers with the TV's fresh state.

Everything the TV reports (app names, the model, errors) is cleaned of control characters and capped before it
reaches a model or a log; the log keeps the action, the outcome and the time, never typed text or a search.

``EVE_FIRETV=0`` turns it all off. With no pairing it is off too, and no tool is lent.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import os
import re
import shlex
import shutil
import time
import urllib.parse
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Awaitable, Callable, Optional, Sequence

log = logging.getLogger(__name__)

CONFIG_PATH = Path("~/.config/eve-bridge/firetv.json")
SHOTS_DIR = Path("~/.cache/eve-bridge/firetv")
OFF_REASON = "The Fire TV is not set up on this computer (eve-bridge firetv pair; EVE_FIRETV)"
# Said when a TV command arrives with no pairing: the code path still knows the words, so the model never gets
# a TV request it has no tools for (the same lesson as Spotify's, 2026-09-27).
NOT_SET_UP_LINE = ("The Fire TV isn't set up on this Mac yet. Turn on ADB debugging on the TV, run: "
                   "eve-bridge firetv pair, then restart eve.")
INSTALL_LINE = "adb isn't installed on this Mac. Run: brew install --cask android-platform-tools"
ALLOW_LINE = ("The TV hasn't allowed this Mac yet. On the TV, accept the Allow USB debugging prompt and tick "
              "Always allow from this computer, then try again.")
UNREACHABLE_LINE = "I can't reach the TV. Is it on, and on the home network?"
TIMEOUT_LINE = "The TV didn't answer in time, so I don't know if that was done. I didn't send it again."
DEEP_STANDBY_LINE = ("I can't reach the TV. It is probably in deep standby (it stops answering about 6 minutes "
                     "after it is turned off). Use the remote, or ask Alexa to turn it on.")
DEFAULT_PORT = 5555
ADB_PATHS = ("/opt/homebrew/bin/adb", "/usr/local/bin/adb")   # the daemon runs under launchd, with a thin PATH
CONNECT_SECS = 6.0
SHELL_SECS = 8.0
INPUT_SECS = 12.0                 # an ``input`` call starts a Java VM on the TV (~1.2 s), plus one per extra key
MDNS_SECS = 5.0
PROBE_SECS = 2.0                  # the plain TCP check of the TV's port before an adb server restart
ADB_RESTART_EVERY = 600.0         # at most one adb server restart per 10 minutes
SCREENCAP_SECS = 20.0
POWER_POLL_SECS = 5.0             # how long "turn on/off" watches the TV change after the key
POLL_STEP_SECS = 0.5
VOLUME_STEP = 10                  # "turn it up" in percent; "a bit" is half
MAX_REPEAT = 10                   # "down 40 times" is a typo or a loop, not a request
MAX_TEXT = 200
CLEAN_CAP = 120
RC_TIMEOUT = 124                  # the runner's code for "no answer in time" (as timeout(1))
RC_NO_ADB = 127                   # the runner's code for "adb would not start"
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"

# (rc, combined stdout and stderr). A timeout is an answer (RC_TIMEOUT), never an exception.
Runner = Callable[[list[str], float], Awaitable[tuple[int, str]]]
# (rc, stdout bytes): the screenshot, which is a PNG and must not be decoded.
Capture = Callable[[list[str], float], Awaitable[tuple[int, bytes]]]


# (host, port, timeout) -> whether a TCP connection opened: the TV answers, whatever adb says.
Probe = Callable[[str, int, float], Awaitable[bool]]


async def tcp_probe(host: str, port: int, timeout: float) -> bool:
    try:
        _, writer = await asyncio.wait_for(asyncio.open_connection(host, port), timeout)
    except (OSError, asyncio.TimeoutError):
        return False
    writer.close()
    with contextlib.suppress(Exception):
        await writer.wait_closed()
    return True


# ---- running adb ---------------------------------------------------------------------------------------------

def find_adb(environ: Any = None) -> str:
    """``EVE_ADB``, then the PATH, then Homebrew's two places. "" when there is none."""
    env = os.environ if environ is None else environ
    override = str(env.get("EVE_ADB", "") or "").strip()
    if override:
        return override
    found = shutil.which("adb")
    if found:
        return found
    return next((p for p in ADB_PATHS if os.access(p, os.X_OK)), "")


async def _spawn(argv: list[str], timeout: float, merge: bool) -> tuple[int, bytes, bytes]:
    try:
        proc = await asyncio.create_subprocess_exec(
            *argv, stdin=asyncio.subprocess.DEVNULL, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.STDOUT if merge else asyncio.subprocess.PIPE)
    except OSError as e:
        return RC_NO_ADB, b"", f"adb could not start ({e.strerror or e})".encode()
    try:
        out, err = await asyncio.wait_for(proc.communicate(), timeout)
    except asyncio.TimeoutError:
        await _kill(proc)
        return RC_TIMEOUT, b"", b""
    except BaseException:
        # cancelled (the owner talked over a slow voice tool, a Telegram STOP): an ``adb connect`` to a TV in deep
        # standby would run on for its whole TCP timeout, and a key press would still land after eve moved on
        await _kill(proc)
        raise
    return proc.returncode or 0, out or b"", err or b""


async def _kill(proc: asyncio.subprocess.Process) -> None:
    try:
        proc.kill()
    except ProcessLookupError:
        pass
    try:
        await asyncio.wait_for(asyncio.shield(proc.wait()), 2.0)
    except (asyncio.TimeoutError, asyncio.CancelledError, ProcessLookupError):
        pass


async def adb_runner(argv: list[str], timeout: float) -> tuple[int, str]:
    """The real runner: one adb process, an argv list (never a shell string), a timeout."""
    rc, out, err = await _spawn(argv, timeout, merge=True)
    return rc, (out + err).decode("utf-8", "replace")


async def adb_capture(argv: list[str], timeout: float) -> tuple[int, bytes]:
    """Like ``adb_runner`` but keeps stdout as bytes; on a failure the error words come back instead."""
    rc, out, err = await _spawn(argv, timeout, merge=False)
    return rc, out if rc == 0 else (err + out)


def _sh(*words: str) -> str:
    """One command line for the TV's shell, every word quoted for it."""
    return " ".join(shlex.quote(w) for w in words)


# ---- cleaning what the TV says -------------------------------------------------------------------------------

_CONTROL = re.compile(r"[\x00-\x1f\x7f-\x9f​-‏ -‮⁦-⁩]+")


def clean(text: Any, cap: int = CLEAN_CAP) -> str:
    """A string from the TV, safe for a model or a log: no control characters or newlines, one line, capped."""
    s = re.sub(r"\s+", " ", _CONTROL.sub(" ", str(text or ""))).strip()
    return s if len(s) <= cap else s[:cap - 1].rstrip() + "…"


# ---- the config ----------------------------------------------------------------------------------------------

def load(path: Optional[Path] = None) -> dict[str, Any]:
    p = (path or CONFIG_PATH).expanduser()
    try:
        d = json.loads(p.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    if not isinstance(d, dict) or not str(d.get("host") or "").strip():
        return {}
    try:
        port = int(d.get("port") or DEFAULT_PORT)
    except (TypeError, ValueError):
        port = DEFAULT_PORT
    return {**d, "host": str(d["host"]).strip(), "port": port, "mdns": str(d.get("mdns") or "")}


def save(cfg: dict[str, Any], path: Optional[Path] = None) -> Path:
    """Written private (600) from the first byte: the TV's address and serial are the owner's home."""
    p = (path or CONFIG_PATH).expanduser()
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(".tmp")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(cfg, f, indent=2)
    os.chmod(tmp, 0o600)
    os.replace(tmp, p)
    return p


# ---- the apps ------------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class App:
    """An app on the TV: the name said, other names for it, and the exact activity that opens it."""
    name: str
    component: str
    aliases: tuple[str, ...] = ()
    data: str = ""                      # opened by a VIEW link rather than as a launcher (Live TV's guide)

    @property
    def package(self) -> str:
        return self.component.split("/", 1)[0]


# The components were read off the owner's TV on 2026-10-04 (LEANBACK_LAUNCHER and LAUNCHER queries). Amazon
# renames activities now and then (Prime Video's did in 2024), so a failed start re-resolves the package.
APPS: tuple[App, ...] = (
    App("YouTube", "com.amazon.firetv.youtube/dev.cobalt.app.MainActivity", ("you tube", "yt")),
    App("Netflix", "com.netflix.ninja/.MainActivity"),
    App("Prime Video", "com.amazon.firebat/com.amazon.firebatcore.deeplink.DeepLinkRoutingActivity",
        ("prime", "amazon prime", "amazon prime video", "amazon video")),
    App("Disney+", "com.disney.disneyplus/com.bamtechmedia.dominguez.main.MainActivity",
        ("disney", "disney plus")),
    App("Hulu", "com.hulu.plus/.SplashActivity"),
    App("Peacock", "com.peacock.peacockfiretv/com.peacock.peacocktv.AmazonMainActivity"),
    App("Spotify", "com.spotify.tv.android/.SpotifyTVActivity"),
    App("ESPN", "com.espn.gtv/com.espn.startup.presentation.StartupActivity"),
    App("NBC Sports", "com.nbcuni.com.nbcsports.liveextra.firetv/com.nbc.nbctvapp.ui.main.view.MainActivity",
        ("nbc",)),
    App("NFL", "com.gotv.nflgamecenter.us.lite/com.nfl.connected.SplashActivity", ("nfl app",)),
    App("NBA", "com.nbaimd.gametime.nba2011.amazon/com.nba.tv.ui.splash.SplashActivity"),
    App("Fox Sports", "com.fox.fts.android/.MainActivity", ("fox",)),
    App("Amazon Music", "com.amazon.bueller.music/com.amazon.music.MainActivity"),
    App("Photos", "com.amazon.bueller.photos/.StubActivity", ("amazon photos",)),
    App("Silk", "com.amazon.cloud9/.browsing.BrowserActivity", ("silk browser", "browser", "web browser")),
    App("Appstore", "com.amazon.venezia/.grid.AppsGridLauncherActivity",
        ("app store", "apps", "my apps", "amazon appstore")),
    App("Settings", "com.amazon.tv.launcher/.ui.SettingsActivity", ("tv settings",)),
    App("Live TV", "com.amazon.tv.livetv/.GuideWrapperActivity", ("live", "guide", "tv guide", "channel guide",
                                                                  "channels", "antenna"),
        data="livetv://showGuideButtonContent?source=UNKNOWN"),
)
YOUTUBE = APPS[0]
LAUNCHER_PACKAGE = "com.amazon.tv.launcher"
PROJECTIVY_PACKAGE = "com.spocky.projengmenu"        # a third-party launcher the Home button may open instead
SCREENSAVER_PACKAGE = "com.amazon.ftv.screensaver"
PASSTHROUGH_ACTIVITY = "PassthroughPlayerActivity"   # the TV showing an HDMI input


def _app_key(text: str) -> str:
    t = (text or "").lower().replace("+", " plus ").replace("’", "'")
    t = re.sub(r"[^a-z0-9 ]+", " ", t)
    t = re.sub(r"\s+", " ", t).strip()
    t = re.sub(r"^(?:the|my)\s+", "", t)
    return re.sub(r"\s+app$", "", t)


_BY_ALIAS: dict[str, App] = {}
for _a in APPS:
    for _k in (_a.name, *_a.aliases):
        _BY_ALIAS.setdefault(_app_key(_k), _a)


def find_app(name: str) -> Optional[App]:
    return _BY_ALIAS.get(_app_key(name))


def app_name(package: str, activity: str = "") -> str:
    """The foreground app in words: the table's name, "home", "screensaver", "an HDMI input", or the package."""
    if not package:
        return ""
    if PASSTHROUGH_ACTIVITY in activity:
        return "an HDMI input"
    if package == SCREENSAVER_PACKAGE:
        return "screensaver"
    if package == LAUNCHER_PACKAGE:
        return "Settings" if "SettingsActivity" in activity else "home"
    if package == PROJECTIVY_PACKAGE:
        return "home" if ".ui.home." in activity else "Projectivy settings"
    for a in APPS:
        if a.package == package:
            return a.name
    return clean(package, 80)


# ---- the TV's state, in one round trip -----------------------------------------------------------------------

# Read-only, one ``adb shell``. Never ``grep -m1`` on the TV: closing the pipe early breaks dumpsys. The awk keeps
# reading to the end for the same reason; it prints only the STREAM_MUSIC block.
STATE_SCRIPT = (
    "echo @@POWER; dumpsys power | grep -E 'mWakefulness=|Display Power: state='; "
    "echo @@FOCUS; dumpsys window | grep mCurrentFocus || dumpsys activity activities | grep mResumedActivity; "
    "echo @@AUDIO; dumpsys audio | awk '/^- STREAM_/{f=($0 ~ /STREAM_MUSIC:/)} f'; "
    "echo @@END")


@dataclass(frozen=True)
class TvState:
    power: str = "unknown"              # on, screensaver, standby, unknown
    wakefulness: str = ""
    display: str = ""
    package: str = ""
    activity: str = ""
    app: str = ""
    index: Optional[int] = None         # STREAM_MUSIC's index, on the TV's own scale
    max_index: Optional[int] = None
    muted: Optional[bool] = None

    @property
    def volume(self) -> Optional[int]:
        """The volume in percent, or None when it could not be read."""
        if self.index is None or not self.max_index:
            return None
        return round(self.index * 100 / self.max_index)

    def public(self) -> dict[str, Any]:
        return {"power": self.power, "app": self.app or None, "volume": self.volume, "muted": self.muted}


def _section(text: str, name: str) -> str:
    m = re.search(rf"@@{name}\s*\n(.*?)(?=\n?@@[A-Z]+|\Z)", text, re.S)
    return m.group(1) if m else ""


def power_word(wakefulness: str, display: str) -> str:
    """Awake + screen on = on; Dreaming = the screensaver (still on); Asleep, Dozing or screen off = standby."""
    w, d = wakefulness.lower(), display.upper()
    if w == "dreaming":
        return "screensaver"
    if w in ("asleep", "dozing") or d == "OFF":
        return "standby"
    if w == "awake":
        return "on"
    return "unknown"


def parse_state(text: str) -> TvState:
    power = _section(text, "POWER")
    wake = re.search(r"mWakefulness=(\w+)", power)
    disp = re.search(r"Display Power: state=(\w+)", power)
    focus = _section(text, "FOCUS")
    pkg, act = "", ""
    m = re.search(r" u0 ([A-Za-z0-9_.]+)(?:/([A-Za-z0-9_.$]+))?", focus)
    if m:
        pkg, act = m.group(1), m.group(2) or ""
        if act.startswith("."):
            act = pkg + act
    audio = _section(text, "AUDIO")
    vol = re.search(r"streamVolume:\s*(\d+)", audio)
    mx = re.search(r"\bMax:\s*(\d+)", audio)
    muted = re.search(r"\bMuted:\s*(true|false)", audio)
    w, d = wake.group(1) if wake else "", disp.group(1) if disp else ""
    return TvState(power=power_word(w, d), wakefulness=w, display=d, package=pkg, activity=act,
                   app=app_name(pkg, act), index=int(vol.group(1)) if vol else None,
                   max_index=int(mx.group(1)) if mx and int(mx.group(1)) > 0 else None,
                   muted=(muted.group(1) == "true") if muted else None)


def said_state(st: TvState) -> str:
    """'TV on — YouTube, volume 44%'."""
    vol = "volume unknown" if st.volume is None else f"volume {st.volume}%" + (" (muted)" if st.muted else "")
    if st.power == "standby":
        return "TV in standby" + ("." if st.volume is None else f", {vol}.")
    if st.power == "unknown":
        return f"TV state unknown, {vol}."
    what = st.app or ("screensaver" if st.power == "screensaver" else "")
    return "TV on" + (f" — {what}" if what else "") + f", {vol}"


# ---- the keys ------------------------------------------------------------------------------------------------

NAV_KEYS: dict[str, int] = {"up": 19, "down": 20, "left": 21, "right": 22, "select": 23, "back": 4, "home": 3,
                            "menu": 82}
KEY_ALIASES = {"ok": "select", "enter": "select", "center": "select", "centre": "select"}
# ``cmd media_session dispatch`` names (AOSP MediaShellCommand.runDispatch). ``stop`` is for the terminal only.
MEDIA_KEYS: dict[str, str] = {"play_pause": "play-pause", "play": "play", "pause": "pause", "next": "next",
                              "previous": "previous", "rewind": "rewind", "fast_forward": "fast-forward",
                              "stop": "stop"}
KEYCODE_WAKEUP = 224
KEYCODE_SLEEP = 223
KEYCODE_VOLUME_MUTE = 164


# ---- the tools -----------------------------------------------------------------------------------------------

ACTIONS = ("power_on", "power_off", "up", "down", "left", "right", "select", "back", "home", "menu", "play_pause",
           "play", "pause", "next", "previous", "rewind", "fast_forward", "volume", "mute", "unmute", "open_app",
           "youtube_search", "type_text")
# What the Mini App's remote card sends (FireTV.panel): the buttons on a remote, the volume and the app chips.
# Typing and YouTube search stay with the chat.
PANEL_ACTIONS = ("power_on", "power_off", "up", "down", "left", "right", "select", "back", "home", "menu",
                 "play_pause", "rewind", "fast_forward", "volume", "volume_change", "mute", "unmute", "open_app")
TOOLS: list[dict[str, Any]] = [
    {"type": "function", "name": "firetv_status", "strict": True,
     "description": "The owner's Fire TV right now: power (on, screensaver, standby), the app in front, the "
                    "volume in percent and whether it is muted.",
     "parameters": {"type": "object", "additionalProperties": False, "required": [], "properties": {}}},
    {"type": "function", "name": "firetv_control", "strict": True,
     "description": "Control the owner's Fire TV: power_on / power_off; the remote's d-pad (up, down, left, "
                    "right, select, back, home, menu, with repeat); media keys (play_pause, play, pause, next, "
                    "previous, rewind, fast_forward); volume (a level or a change), mute, unmute; open_app by "
                    "name; youtube_search; type_text into the box the TV has focused. Pass null for what you "
                    "don't use.",
     "parameters": {"type": "object", "additionalProperties": False,
                    "required": ["action", "app", "query", "text", "volume", "volume_change", "repeat"],
                    "properties": {
                        "action": {"type": "string", "enum": list(ACTIONS)},
                        "app": {"type": ["string", "null"],
                                "description": "open_app: the app's name (\"YouTube\", \"Netflix\", \"Prime "
                                               "Video\"); or null."},
                        "query": {"type": ["string", "null"],
                                  "description": "youtube_search: what to search for; or null."},
                        "text": {"type": ["string", "null"],
                                 "description": "type_text: plain ASCII, up to 200 characters; or null."},
                        "volume": {"type": ["integer", "null"], "description": "volume: 0-100 percent; or null."},
                        "volume_change": {"type": ["integer", "null"],
                                          "description": "volume: a change such as 10 or -10, when no level "
                                                         "was asked; or null."},
                        "repeat": {"type": ["integer", "null"],
                                   "description": "d-pad keys: how many presses, 1-10; or null for one."}}}},
    {"type": "function", "name": "firetv_screenshot", "strict": True,
     "description": "Send the owner a picture of what their Fire TV shows now. You don't see the picture: you "
                    "get back the TV's state at the same moment (power, the app in front, the volume). Use it "
                    "when the owner wants to see the screen, or to check where navigating landed.",
     "parameters": {"type": "object", "additionalProperties": False, "required": [], "properties": {}}},
]
TOOL_NAMES = tuple(t["name"] for t in TOOLS)

# The model's guide. The screenshot sentences are left out where there is no screenshot tool (the live voice).
_SHOT_TOOL = (" firetv_screenshot sends the owner a picture of the screen; you don't see it, only the state it "
              "returns.")
_SHOT_CHECK = "; send a screenshot when the owner needs to see where it landed"
_INSTRUCTIONS = """The owner's Fire TV.
What you can do: firetv_control turns it on or off, presses the remote's keys, plays and pauses, sets the volume,
mutes, opens an app by name, searches YouTube and types into a focused search box. firetv_status reads power, the
app in front and the volume.{shot_tool} "Turn it up" is volume_change 10, "a bit" 5.
Verify: power, volume and mute are read back, so say the result you got. The d-pad, media keys, opening apps and
typing are not, so say what you pressed or opened{shot_check}. Don't guess what is on the screen.
When an app is already in front and the owner asks for another, go ahead, but say what you interrupted.
Limits: HDMI inputs can't be switched. In deep standby the TV can't be reached: say to use the remote or Alexa.
Search works in YouTube only."""
INSTRUCTIONS = _INSTRUCTIONS.format(shot_tool=_SHOT_TOOL, shot_check=_SHOT_CHECK)
INSTRUCTIONS_NO_SCREENSHOT = _INSTRUCTIONS.format(shot_tool="", shot_check="")


# ---- the code path -------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class Command:
    """A TV command the code understood."""
    action: str                         # status, power_on, power_off, a key, volume, mute, unmute, open_app, ...
    app: str = ""
    query: str = ""
    text: str = ""
    volume: Optional[int] = None
    change: Optional[int] = None
    repeat: int = 1


_LEAD = r"(?:(?:hey|ok|okay)\s+)?(?:eve\s+)?(?:(?:please|pls|can you|could you|would you)\s+)?"
_TAIL = r"(?:\s+(?:please|pls|thanks|thank you|eve|now))?"
_TV = r"(?:the\s+|my\s+)?(?:fire\s?tv|tv|television|telly)"
# "what's on tv" with no article asks about the listings, not this TV: that form needs "the", "my" or "fire tv"
_THIS_TV = r"(?:(?:the|my)\s+(?:fire\s?tv|tv|television|telly)|fire\s?tv)"
# Spotify owns "play the music on the tv" (a Connect transfer to the TV's Spotify): those words are left to it.
_MUSIC = r"(?:the\s+|my\s+)?(?:music|spotify|song|track|playback|tunes)\b"
_APP = r"([a-z0-9][a-z0-9' -]{0,38}?)"
_BIT = r"(\s+a\s+(?:bit|little|touch))?"
_COUNT_WORDS = {"once": 1, "one": 1, "two": 2, "twice": 2, "three": 3, "four": 4, "five": 5, "six": 6,
                "seven": 7, "eight": 8, "nine": 9, "ten": 10}
_KEY = r"(up|down|left|right|select|ok|enter|back|home|menu)"
_TIMES = r"(?:\s+(\d{1,2}|one|two|three|four|five|six|seven|eight|nine|ten)\s+times|\s+(once|twice))?"


def _count(*groups: Optional[str]) -> int:
    g = next((x for x in groups if x), None)
    if g is None:
        return 1
    return int(g) if g.isdigit() else _COUNT_WORDS.get(g, 1)


def _key(m: re.Match[str], key_group: int = 1) -> Command:
    k = m.group(key_group)
    return Command(KEY_ALIASES.get(k, k), repeat=_count(m.group(key_group + 1), m.group(key_group + 2)))


def _step(m: re.Match[str], sign: int) -> Command:
    bit = next((g for g in m.groups() if g and g.strip().startswith("a ")), None)
    return Command("volume", change=sign * (VOLUME_STEP // 2 if bit else VOLUME_STEP))


_UP = r"(?:up|louder)"
_DOWN = r"(?:down|quieter|softer)"
_PATTERNS: list[tuple[re.Pattern[str], Callable[[re.Match[str]], Optional[Command]]]] = [
    (re.compile(rf"(?:turn|switch|power)\s+on\s+{_TV}|(?:turn|switch|power)\s+{_TV}\s+on|{_TV}\s+on"
                rf"|wake\s+(?:up\s+)?{_TV}(?:\s+up)?"),
     lambda m: Command("power_on")),
    (re.compile(rf"(?:turn|switch|power)\s+off\s+{_TV}|(?:turn|switch|power)\s+{_TV}\s+off|{_TV}\s+off"
                rf"|put\s+{_TV}\s+to\s+sleep"),
     lambda m: Command("power_off")),
    (re.compile(rf"(?:what'?s|what\s+is)\s+(?:on|playing\s+on)\s+{_THIS_TV}|is\s+{_TV}\s+on|{_TV}\s+status"
                rf"|(?:what'?s|what\s+is)\s+{_TV}\s+(?:showing|doing|playing)"),
     lambda m: Command("status")),
    (re.compile(rf"(?:set\s+)?{_TV}\s+volume\s+(?:to\s+)?(\d{{1,3}})\s*%?"
                rf"|set\s+(?:the\s+)?volume\s+(?:on|of)\s+{_TV}\s+to\s+(\d{{1,3}})\s*%?"
                rf"|(?:set|turn)\s+{_TV}\s+(?:volume\s+)?to\s+(\d{{1,3}})\s*%?"),
     lambda m: Command("volume", volume=int(next(g for g in m.groups() if g)))),
    (re.compile(rf"{_TV}\s+volume\s+{_UP}{_BIT}|turn\s+{_TV}\s+{_UP}{_BIT}|turn\s+up\s+{_TV}{_BIT}"
                rf"|{_TV}\s+louder{_BIT}|turn\s+(?:the\s+)?volume\s+up\s+on\s+{_TV}{_BIT}"
                rf"|turn\s+up\s+the\s+volume\s+on\s+{_TV}{_BIT}|louder\s+on\s+{_TV}"),
     lambda m: _step(m, 1)),
    (re.compile(rf"{_TV}\s+volume\s+{_DOWN}{_BIT}|turn\s+{_TV}\s+{_DOWN}{_BIT}|turn\s+down\s+{_TV}{_BIT}"
                rf"|{_TV}\s+(?:quieter|softer){_BIT}|turn\s+(?:the\s+)?volume\s+down\s+on\s+{_TV}{_BIT}"
                rf"|turn\s+down\s+the\s+volume\s+on\s+{_TV}{_BIT}|quieter\s+on\s+{_TV}"),
     lambda m: _step(m, -1)),
    (re.compile(rf"mute\s+{_TV}|{_TV}\s+mute"), lambda m: Command("mute")),
    (re.compile(rf"unmute\s+{_TV}|{_TV}\s+unmute"), lambda m: Command("unmute")),
    (re.compile(rf"pause\s+{_TV}|{_TV}\s+pause|pause\s+(?:it\s+)?on\s+{_TV}"), lambda m: Command("pause")),
    (re.compile(rf"(?:play|resume|unpause)\s+{_TV}|{_TV}\s+(?:play|resume)|(?:resume|unpause)\s+on\s+{_TV}"),
     lambda m: Command("play")),
    (re.compile(rf"(?:go\s+(?:back\s+)?)?home\s+on\s+{_TV}|go\s+to\s+(?:the\s+)?home\s+screen\s+on\s+{_TV}"),
     lambda m: Command("home")),
    # "tv up" alone most likely means louder, so a bare up or down needs a count ("tv down 3 times") to be the
    # d-pad; without one the model hears it
    (re.compile(rf"{_TV}\s+{_KEY}{_TIMES}"),
     lambda m: None if m.group(1) in ("up", "down") and not (m.group(2) or m.group(3)) else _key(m)),
    (re.compile(rf"press\s+{_KEY}{_TIMES}\s+on\s+{_TV}"), lambda m: _key(m)),
    (re.compile(rf"(?:play|search(?:\s+for)?|find|look\s+up|put\s+on)\s+(.{{1,80}}?)\s+on\s+youtube\s+on\s+{_TV}"
                rf"|search\s+youtube\s+for\s+(.{{1,80}}?)\s+on\s+{_TV}"),
     lambda m: Command("youtube_search", query=next(g for g in m.groups() if g).strip())),
    # "open" names an app outright, so any name goes (an unknown one gets a plain "no such app")
    (re.compile(rf"(?:open|launch|start|load)\s+(?:up\s+)?(?!(?:the\s+|my\s+)?(?:music|song|track|playback|tunes)"
                rf"\b){_APP}(?:\s+app)?\s+on\s+{_TV}"),
     lambda m: Command("open_app", app=m.group(1).strip())),
    # "put/play/watch X on the tv" opens X only when X is an app eve knows; "play daft punk on the tv" is not
    (re.compile(rf"(?:put|play|watch|switch\s+to|go\s+to)\s+(?:on\s+)?(?!{_MUSIC}){_APP}(?:\s+app)?\s+on\s+{_TV}"),
     lambda m: Command("open_app", app=m.group(1).strip()) if find_app(m.group(1)) else None),
]


def _norm(text: str) -> str:
    t = (text or "").lower().replace("’", "'").replace("+", " plus ")
    return re.sub(r"\s+", " ", re.sub(r"[^\w%'\s-]", " ", t)).strip()


def match(text: str) -> Optional[Command]:
    """A Command when the whole message is a TV command that names the TV, else None (the model answers).
    Anchored at both ends: "turn on the tv in ten minutes" leaves words over, so the model gets it. Reads no state,
    so telegram can use it to say NOT_SET_UP_LINE when no TV is paired."""
    t = _norm(text)
    if not t or len(t) > 120:
        return None
    for pattern, make in _PATTERNS:
        # the lead and the tail only have non-capturing groups: the pattern's own groups come through as they are
        m = re.fullmatch(_LEAD + "(?:" + pattern.pattern + ")" + _TAIL, t)
        if m:
            cmd = make(m)
            if cmd is None:
                return None
            if cmd.volume is not None and cmd.volume > 100:
                return None
            return cmd
    return None


# ---- the whole thing -----------------------------------------------------------------------------------------

@dataclass(frozen=True)
class Reach:
    """Whether a command can be sent: the TV answered and allowed this Mac. ``why`` names what failed."""
    ok: bool
    why: str = ""                       # no_adb, unreachable, unauthorized, offline, timeout
    line: str = ""


# ``get-state`` and ``exec-out`` say "error: ...", but ``adb shell`` says "adb: device '...' not found" (adb 1.0.41,
# checked 2026-10-04): both prefixes are the TV gone, before the command ran
_GONE = re.compile(r"(?:error|adb): (?:device (?:offline|'[^']*' not found|not found|still connecting)|closed|"
                   r"no devices(?:/emulators)? found|no devices|protocol fault)", re.I)


def _gone(rc: int, out: str) -> bool:
    """adb said the TV is not there: the command never reached it, so sending it again is safe."""
    return rc != 0 and rc != RC_TIMEOUT and bool(_GONE.search(out or ""))


def _lost(rc: int, out: str) -> bool:
    """The TV went away under a command (as opposed to the command failing on the TV): gone, or no answer in time."""
    return rc == RC_TIMEOUT or _gone(rc, out)


def _am_failed(out: str) -> bool:
    return bool(re.search(r"^(?:Error|Exception|java\.lang\.\w+Exception)", out or "", re.M))


def _resolved_component(out: str) -> str:
    """The last ``package/activity`` line of ``cmd package resolve-activity --brief``."""
    comps = re.findall(r"^\s*([A-Za-z0-9_.]+/[A-Za-z0-9_.$]+)\s*$", out or "", re.M)
    return comps[-1] if comps else ""


_PACKAGE = re.compile(r"^[A-Za-z][A-Za-z0-9_]*(?:\.[A-Za-z0-9_]+)+$")


def type_text_arg(text: str) -> tuple[str, str]:
    """(the ``input text`` argument, "") or ("", why not). Spaces become %s, the way ``input text`` wants them;
    the word is shell-quoted when the command line is built."""
    if not text:
        return "", "Nothing to type."
    if len(text) > MAX_TEXT:
        return "", f"That's too long to type on the TV (over {MAX_TEXT} characters)."
    if not re.fullmatch(r"[ -~]+", text):
        return "", "I can only type plain English letters, digits and punctuation on the TV."
    return text.replace(" ", "%s"), ""


class FireTV:
    """The owner's Fire TV: the connection, the commands, the tools, the code path."""

    def __init__(self, cfg: dict[str, Any], *, runner: Optional[Runner] = None, capture: Optional[Capture] = None,
                 adb: str = "adb", path: Optional[Path] = None, shots_dir: Optional[Path] = None,
                 sleep: Callable[[float], Awaitable[Any]] = asyncio.sleep,
                 clock: Callable[[], float] = time.monotonic, probe: Optional[Probe] = None) -> None:
        self.cfg = dict(cfg)
        self.probe: Probe = probe or tcp_probe
        self._restarted_at: Optional[float] = None    # the clock at the last adb server restart
        self.runner: Runner = runner or adb_runner
        self.capture: Capture = capture or adb_capture
        self.adb = adb
        self.path = path                   # where a rediscovered address is saved; None keeps it in memory
        self.shots_dir = shots_dir
        self._sleep = sleep
        self._clock = clock
        self._ready = False
        self._lock = asyncio.Lock()

    @property
    def serial(self) -> str:
        return f"{self.cfg['host']}:{int(self.cfg.get('port') or DEFAULT_PORT)}"

    # -- the connection --
    async def _adb(self, *args: str, timeout: float = SHELL_SECS) -> tuple[int, str]:
        return await self.runner([self.adb, *args], timeout)

    async def _connect(self) -> Reach:
        if not self.adb:
            return Reach(False, "no_adb", INSTALL_LINE)
        rc, out = await self._adb("connect", self.serial, timeout=CONNECT_SECS)
        low = (out or "").lower()
        if rc == RC_NO_ADB:
            return Reach(False, "no_adb", INSTALL_LINE)
        if "unauthorized" in low or "failed to authenticate" in low:
            return Reach(False, "unauthorized", ALLOW_LINE)
        # adb connect exits 0 either way: its words are the answer
        if rc == RC_TIMEOUT or any(w in low for w in ("failed to connect", "unable to connect", "cannot connect")) \
                or "connected to" not in low:
            return Reach(False, "unreachable", UNREACHABLE_LINE)
        rc, out = await self._adb("-s", self.serial, "get-state", timeout=CONNECT_SECS)
        state = (out or "").strip().lower()
        if rc == 0 and state.splitlines()[-1:] == ["device"]:
            self._ready = True
            return Reach(True)
        if "unauthorized" in state:
            return Reach(False, "unauthorized", ALLOW_LINE)
        return Reach(False, "offline", UNREACHABLE_LINE)

    async def discover(self) -> list[tuple[str, str, int]]:
        """``adb mdns services``: (instance, host, port) of every plain-ADB service on the network."""
        rc, out = await self._adb("mdns", "services", timeout=MDNS_SECS)
        return parse_mdns(out) if rc == 0 else []

    async def _rediscover(self) -> None:
        name = str(self.cfg.get("mdns") or "")
        if not name:
            return
        for instance, host, port in await self.discover():
            if instance == name and (host, port) != (self.cfg["host"], int(self.cfg.get("port") or DEFAULT_PORT)):
                log.info("firetv: found the TV at a new address by its mDNS name")
                self.cfg["host"], self.cfg["port"] = host, port
                if self.path is not None:
                    try:
                        save(self.cfg, self.path)
                    except OSError as e:
                        log.warning("firetv: could not keep the new address (%s)", e)
                return

    async def _recover(self) -> Reach:
        """Once: drop the old connection, look the TV up by name, connect again; then, only if the TV's port opens
        while adb still cannot reach it, restart the stale adb server and connect once more."""
        self._ready = False
        await self._adb("disconnect", self.serial, timeout=CONNECT_SECS)
        await self._rediscover()
        r = await self._connect()
        if r.ok or r.why not in ("unreachable", "offline"):
            return r
        return await self._restart_server(r)

    async def _restart_server(self, failed: Reach) -> Reach:
        now = self._clock()
        if self._restarted_at is not None and now - self._restarted_at < ADB_RESTART_EVERY:
            return failed
        host, port = self.cfg["host"], int(self.cfg.get("port") or DEFAULT_PORT)
        if not await self.probe(host, port, PROBE_SECS):
            return failed                          # the TV itself is not answering: the server is not the problem
        self._restarted_at = now
        log.warning("firetv: the TV's port answers but adb cannot reach it; restarting the adb server")
        await self._adb("kill-server", timeout=CONNECT_SECS)
        await self._adb("start-server", timeout=CONNECT_SECS)
        r = await self._connect()
        log.info("firetv: after the adb server restart: %s", "connected" if r.ok else r.why)
        return r

    async def ensure(self) -> Reach:
        async with self._lock:
            if self._ready:
                return Reach(True)
            r = await self._connect()
            if r.ok or r.why in ("no_adb", "unauthorized"):
                return r
            return await self._recover()

    async def shell(self, script: str, timeout: float = SHELL_SECS, *, again: bool = True) -> tuple[Reach, int, str]:
        """One ``adb shell`` on the TV, reconnecting and retrying once if the TV went away under it.
        ``again=False`` is for a command that must not land twice (typing, a run of keys, a toggle): after a timeout
        it may already have landed, so it is not sent again; after "device not found" it never ran, so it is."""
        r = await self.ensure()
        if not r.ok:
            return r, 0, ""
        rc, out = await self._adb("-s", self.serial, "shell", script, timeout=timeout)
        if not _lost(rc, out):
            return r, rc, out
        if not again and not _gone(rc, out):
            self._ready = False             # connect afresh next time, but don't press it twice
            return Reach(False, "timeout", TIMEOUT_LINE), rc, out
        async with self._lock:
            r = await self._recover()
        if not r.ok:
            return r, 0, ""
        rc, out = await self._adb("-s", self.serial, "shell", script, timeout=timeout)
        if _lost(rc, out):
            self._ready = False
            return Reach(False, "offline", UNREACHABLE_LINE), rc, out
        return r, rc, out

    # -- reading --
    async def state(self) -> tuple[Reach, Optional[TvState]]:
        r, rc, out = await self.shell(STATE_SCRIPT)
        if not r.ok:
            return r, None
        if "@@POWER" not in out:
            self._ready = False                 # whatever answered, connect afresh next time
            return Reach(False, "offline", "The TV answered, but not with its state."), None
        return r, parse_state(out)

    async def status(self) -> dict[str, Any]:
        t0 = self._clock()
        r, st = await self.state()
        self._log("status", r.ok, t0)
        if st is None:
            return {"ok": False, "reachable": r.why not in ("unreachable", "offline", "no_adb"),
                    "authorized": r.why != "unauthorized", "reason": r.line, "line": r.line}
        return {"ok": True, **st.public(), "line": said_state(st)}

    # -- doing --
    async def control(self, action: str, *, app: str = "", query: str = "", text: str = "",
                      volume: Optional[int] = None, volume_change: Optional[int] = None,
                      repeat: Optional[int] = None) -> dict[str, Any]:
        """One command; ``{"ok", "line", ...}``. The line is what to tell the owner."""
        t0 = self._clock()
        try:
            out = await self._control(action, app=app.strip(), query=query.strip(), text=text, volume=volume,
                                      volume_change=volume_change, repeat=repeat)
        except OSError as e:                    # the runner could not run at all
            log.warning("firetv: %s failed: %s", action, clean(e))
            out = {"ok": False, "reason": UNREACHABLE_LINE, "line": UNREACHABLE_LINE}
        self._log(action, bool(out.get("ok")), t0)
        return out

    def _log(self, action: str, ok: bool, t0: float) -> None:
        # the action, the outcome and the time: never typed text, a search or an app's words
        log.info("firetv: %s -> %s in %.0f ms", action, "done" if ok else "failed", (self._clock() - t0) * 1000)

    @staticmethod
    def _fail(line: str, **extra: Any) -> dict[str, Any]:
        return {"ok": False, "reason": line, "line": line, **extra}

    async def _send(self, script: str, timeout: float = SHELL_SECS, *,
                    again: bool = True) -> tuple[Optional[dict[str, Any]], int, str]:
        """Run a command; (a failure dict or None, rc, output). ``again``: as in ``shell``."""
        r, rc, out = await self.shell(script, timeout, again=again)
        if not r.ok:
            return self._fail(r.line, reachable=r.why not in ("unreachable", "offline", "no_adb"),
                              authorized=r.why != "unauthorized"), rc, out
        return None, rc, out

    async def _control(self, action: str, *, app: str, query: str, text: str, volume: Optional[int],
                       volume_change: Optional[int], repeat: Optional[int]) -> dict[str, Any]:
        action = KEY_ALIASES.get(action, action)
        if action == "status":
            return await self.status()
        if action == "power_on":
            return await self._power(True)
        if action == "power_off":
            return await self._power(False)
        if action in NAV_KEYS:
            return await self.press(action, repeat or 1)
        if action in MEDIA_KEYS:
            # play-pause, next and the seeks move on each press: not sent twice after a timeout
            fail, rc, out = await self._send(_sh("cmd", "media_session", "dispatch", MEDIA_KEYS[action]),
                                             again=action in ("play", "pause", "stop"))
            if fail:
                return fail
            if rc != 0:
                return self._fail(f"The TV refused {action.replace('_', ' ')} ({clean(out, 80)}).")
            said = {"play_pause": "Pressed play/pause", "fast_forward": "Pressed fast-forward"}.get(
                action, f"Pressed {action}")
            return {"ok": True, "line": said + " on the TV."}
        if action == "volume":
            return await self._volume(volume, volume_change)
        if action in ("mute", "unmute"):
            return await self._mute(action == "mute")
        if action == "open_app":
            return await self.open_app(app)
        if action == "youtube_search":
            return await self.youtube_search(query)
        if action == "type_text":
            return await self.type_text(text)
        return self._fail(f"I can't do {clean(action, 40)!r} on the TV.")

    async def _power(self, on: bool) -> dict[str, Any]:
        r, st = await self.state()
        if st is None:
            if on and r.why in ("unreachable", "offline"):
                return self._fail(DEEP_STANDBY_LINE, reachable=False)
            if not on and r.why in ("unreachable", "offline"):
                return self._fail("I can't reach the TV. It may already be off.", reachable=False)
            return self._fail(r.line, reachable=r.why != "no_adb", authorized=r.why != "unauthorized")
        if on and st.power == "on":
            return {"ok": True, "line": "The TV is already on.", **st.public()}
        if not on and st.power == "standby":
            return {"ok": True, "line": "The TV is already off.", **st.public()}
        # WAKEUP and SLEEP do nothing when the TV is already there; POWER is a toggle and is never sent
        fail, _, _ = await self._send(_sh("input", "keyevent", str(KEYCODE_WAKEUP if on else KEYCODE_SLEEP)),
                                      INPUT_SECS)
        if fail:
            return fail
        waited, last = 0.0, st
        while True:
            _, now = await self.state()
            if now is not None:
                last = now
                if (now.power == "on") if on else (now.power == "standby"):
                    line = ("TV on" + (f" — {now.app}." if now.app else ".")) if on else "TV off."
                    return {"ok": True, "line": line, **now.public()}
            if waited >= POWER_POLL_SECS:
                break
            await self._sleep(POLL_STEP_SECS)
            waited += POLL_STEP_SECS
        word = "wake" if on else "sleep"
        return self._fail(f"I sent {word}, but the TV still reports {last.power}.", **last.public())

    async def press(self, key: str, repeat: int = 1) -> dict[str, Any]:
        """A d-pad key, pressed ``repeat`` times in ONE ``input`` call (each call costs a Java VM start)."""
        key = KEY_ALIASES.get(key, key)
        if key not in NAV_KEYS:
            return self._fail(f"The TV has no {clean(key, 20)!r} key here.")
        n = max(1, min(MAX_REPEAT, int(repeat or 1)))
        fail, rc, out = await self._send(_sh("input", "keyevent", *[str(NAV_KEYS[key])] * n),
                                         INPUT_SECS + 0.3 * n, again=False)
        if fail:
            return fail
        if rc != 0:
            return self._fail(f"The TV refused the key ({clean(out, 80)}).")
        return {"ok": True, "line": f"Pressed {key}" + (f" {n} times." if n > 1 else "."), "pressed": n}

    async def _volume(self, volume: Optional[int], change: Optional[int]) -> dict[str, Any]:
        if volume is None and change is None:
            return self._fail("What volume?")
        r, st = await self.state()
        if st is None:
            return self._fail(r.line)
        if st.volume is None or st.max_index is None:
            return self._fail("I couldn't read the TV's volume, so I didn't change it.")
        target = max(0, min(100, int(volume) if volume is not None else st.volume + int(change or 0)))
        index = round(target * st.max_index / 100)
        fail, rc, out = await self._send(_sh("cmd", "media_session", "volume", "--stream", "3", "--set", str(index)))
        if fail:
            return fail
        if rc != 0:
            return self._fail(f"The TV refused the volume ({clean(out, 80)}).")
        _, after = await self.state()
        if after is None or after.volume is None:
            return {"ok": True, "line": f"Set the TV volume to {target}%, but couldn't read it back.",
                    "volume": None}
        return {"ok": True, "line": f"TV volume {after.volume}%" + (" (muted)." if after.muted else "."),
                "volume": after.volume, "muted": after.muted}

    async def _mute(self, mute: bool) -> dict[str, Any]:
        r, st = await self.state()
        if st is None:
            return self._fail(r.line)
        if st.muted is None:
            return self._fail("I couldn't read whether the TV is muted, so I didn't touch it.")
        if st.muted == mute:
            return {"ok": True, "line": "The TV is already " + ("muted." if mute else "unmuted."), **st.public()}
        # KEYCODE_VOLUME_MUTE toggles the speaker; media_session's "mute" is the microphone's key
        # ok only when the key was taken AND the readback shows the target: a toggle can be refused, or answered and
        # still lost, so an unread one is not ok (verification/Eve/FireTv.lean, Fix.step, 2026-10-04)
        fail, rc, out = await self._send(_sh("input", "keyevent", str(KEYCODE_VOLUME_MUTE)), INPUT_SECS, again=False)
        if fail:
            return fail
        if rc != 0:
            return self._fail(f"The TV refused the mute key ({clean(out, 80)}).")
        _, after = await self.state()
        if after is None or after.muted is None:
            return self._fail("I pressed mute, but couldn't read the TV back, so I can't say it is "
                              + ("muted." if mute else "unmuted."))
        if after.muted != mute:
            return self._fail("I pressed mute, but the TV still reports " + ("unmuted." if mute else "muted."),
                              **after.public())
        return {"ok": True, "line": "TV muted." if mute else f"TV unmuted, volume {after.volume}%.",
                **after.public()}

    async def _before_opening(self) -> tuple[Optional[TvState], str]:
        """What is in front now, and the TV woken first when it is in standby (an app opened on a dark screen
        is no use)."""
        _, st = await self.state()
        if st is None:
            return None, ""
        woke = ""
        if st.power == "standby":
            fail, _, _ = await self._send(_sh("input", "keyevent", str(KEYCODE_WAKEUP)), INPUT_SECS)
            if not fail:
                woke = "Turned the TV on. "
        return st, woke

    @staticmethod
    def _interrupted(st: Optional[TvState], target: str) -> str:
        if st is None or st.power == "standby" or not st.app or st.app in ("home", "screensaver", target):
            return ""
        return st.app

    async def _start(self, component: str, data: str = "") -> tuple[Optional[dict[str, Any]], bool]:
        words = ["am", "start"] + (["-a", "android.intent.action.VIEW", "-d", data] if data else []) + ["-n", component]
        fail, rc, out = await self._send(_sh(*words))
        if fail:
            return fail, False
        return None, rc == 0 and not _am_failed(out)

    async def _resolve(self, package: str) -> str:
        """The package's launcher activity: LEANBACK_LAUNCHER first (the TV's own), then LAUNCHER."""
        if not _PACKAGE.match(package):
            return ""
        cat = "android.intent.category."
        script = (_sh("cmd", "package", "resolve-activity", "--brief", "-c", cat + "LEANBACK_LAUNCHER", package)
                  + "; echo @@; " + _sh("cmd", "package", "resolve-activity", "--brief", "-c", cat + "LAUNCHER",
                                        package))
        r, rc, out = await self.shell(script)
        if not r.ok:
            return ""
        lean, _, plain = out.partition("@@")
        return _resolved_component(lean) or _resolved_component(plain)

    async def packages(self) -> list[str]:
        r, rc, out = await self.shell("pm list packages")
        if not r.ok:
            return []
        return sorted(set(re.findall(r"^package:([A-Za-z0-9_.]+)\s*$", out or "", re.M)))

    async def open_app(self, name: str) -> dict[str, Any]:
        """By the table's name or alias; else any installed package whose name has the words in it."""
        key = _app_key(name)
        if not key:
            return self._fail("Which app?")
        st, woke = await self._before_opening()
        entry = find_app(name)
        label = entry.name if entry else clean(name, 40)
        component, data = (entry.component, entry.data) if entry else ("", "")
        started = False
        if component:
            fail, started = await self._start(component, data)
            if fail:
                return fail
        if not started:
            package = entry.package if entry else ""
            if not package:
                squashed = key.replace(" ", "")
                found = [p for p in await self.packages() if squashed in p.lower().replace("_", "")]
                if not found:
                    return self._fail(f"There's no app called {label} on the TV.")
                if len(found) > 1:
                    names = ", ".join(clean(p, 60) for p in found[:5])
                    return self._fail(f"More than one app on the TV matches {label}: {names}.")
                package = found[0]
            resolved = await self._resolve(package)
            if not resolved:
                return self._fail(f"{label} is on the TV but has nothing I can open.")
            fail, started = await self._start(resolved)
            if fail:
                return fail
            if not started:
                return self._fail(f"The TV wouldn't open {label}.")
            if not entry:
                label = app_name(package)
        was = self._interrupted(st, label)
        line = f"{woke}Opened {label}" + (f" — {was} was on." if was else ".")
        return {"ok": True, "line": line, "opened": label, **({"interrupted": was} if was else {})}

    async def youtube_search(self, query: str) -> dict[str, Any]:
        q = clean(query, MAX_TEXT)
        if not q:
            return self._fail("Search YouTube for what?")
        st, woke = await self._before_opening()
        url = "https://www.youtube.com/results?search_query=" + urllib.parse.quote_plus(q)
        # three apps take a youtube.com link on this TV (YouTube, Silk, Amazon's intent helper): -n pins YouTube
        fail, started = await self._start(YOUTUBE.component, url)
        if fail:
            return fail
        if not started:
            return self._fail("The TV wouldn't open YouTube's search.")
        was = self._interrupted(st, YOUTUBE.name)
        return {"ok": True, "line": f"{woke}Searching YouTube on the TV." + (f" {was} was on." if was else ""),
                **({"interrupted": was} if was else {})}

    async def type_text(self, text: str) -> dict[str, Any]:
        arg, why = type_text_arg(text or "")
        if why:
            return self._fail(why)
        fail, rc, out = await self._send(_sh("input", "text", arg), INPUT_SECS + len(arg) * 0.02, again=False)
        if fail:
            return fail
        if rc != 0:
            return self._fail(f"The TV refused the text ({clean(out, 80)}).")
        return {"ok": True, "line": f"Typed {len(text)} characters on the TV."}

    async def screenshot(self, out_dir: Optional[Path] = None, out_path: Optional[Path] = None) -> dict[str, Any]:
        """A PNG of the screen, saved under ``~/.cache/eve-bridge/firetv/``: ``{"ok", "path"}``."""
        t0 = self._clock()
        r = await self.ensure()
        if not r.ok:
            self._log("screenshot", False, t0)
            return self._fail(r.line)
        argv = [self.adb, "-s", self.serial, "exec-out", "screencap", "-p"]
        rc, data = await self.capture(argv, SCREENCAP_SECS)
        if rc != 0 and _lost(rc, data[:300].decode("utf-8", "replace")):
            async with self._lock:
                r = await self._recover()
            if not r.ok:
                self._log("screenshot", False, t0)
                return self._fail(r.line)
            argv[2] = self.serial
            rc, data = await self.capture(argv, SCREENCAP_SECS)
        if rc != 0 or not data.startswith(PNG_SIGNATURE):
            self._log("screenshot", False, t0)
            return self._fail("The TV didn't send a picture.")
        if out_path is not None:
            target = out_path.expanduser()
        else:
            folder = (out_dir or self.shots_dir or SHOTS_DIR).expanduser()
            target = folder / time.strftime("firetv-%Y%m%d-%H%M%S.png")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        self._log("screenshot", True, t0)
        return {"ok": True, "path": str(target)}

    # -- the model's tools --
    def tools(self) -> list[dict[str, Any]]:
        return json.loads(json.dumps(TOOLS))

    def instructions(self, screenshot: bool = True) -> str:
        """``screenshot=False`` where the screenshot tool is not lent (the live voice): no sentence names it."""
        return INSTRUCTIONS if screenshot else INSTRUCTIONS_NO_SCREENSHOT

    async def screenshot_for_model(self) -> dict[str, Any]:
        """The screenshot for a model that cannot see it: the picture's path for the owner, and in words what the
        TV reports at the same moment (``shows``), so the model is told something true about the screen."""
        shot = await self.screenshot()
        if not shot.get("ok") or not shot.get("path"):
            return {"ok": False, "reason": str(shot.get("reason") or "the TV sent no picture")}
        _, st = await self.state()
        shows = said_state(st) if st is not None else "the TV's state couldn't be read"
        return {"ok": True, "path": shot["path"], "shows": shows}

    async def handle(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        def num(v: Any) -> Optional[int]:
            return int(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else None

        if name == "firetv_status":
            return await self.status()
        if name == "firetv_screenshot":
            return await self.screenshot_for_model()
        if name == "firetv_control":
            return await self.control(str(args.get("action") or ""), app=str(args.get("app") or ""),
                                      query=str(args.get("query") or ""), text=str(args.get("text") or ""),
                                      volume=num(args.get("volume")), volume_change=num(args.get("volume_change")),
                                      repeat=num(args.get("repeat")))
        return {"ok": False, "reason": f"unknown tool {name}"}

    # -- the Mini App's remote --
    async def panel(self, body: dict[str, Any]) -> dict[str, Any]:
        """The Mini App's TV remote (miniapp.py ``/api/firetv``, owner only). ``{"action": "status"}`` reads the
        TV; every other action in ``PANEL_ACTIONS`` goes through ``control`` and answers with the fresh state, so
        the card always shows what the TV reports, not what was asked. ``volume`` takes ``volume`` (0-100, the
        slider), ``volume_change`` takes ``change`` (a signed percent), ``open_app`` takes ``app`` (a name from
        ``APPS``), a d-pad key takes an optional ``repeat``. The answer always carries ``apps`` (the picker's names) and ``now`` (the status line) when the
        TV could be read; ``line`` says what happened."""
        def num(v: Any) -> Optional[int]:
            return int(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else None

        apps = [a.name for a in APPS]
        action = str(body.get("action") or "status")
        if action == "status":
            st = await self.status()
            return {**st, "now": st["line"] if st.get("ok") else None, "apps": apps}
        if action not in PANEL_ACTIONS:
            return {"ok": False, "line": "Not something the TV remote does.", "apps": apps}
        if action == "volume":
            volume = num(body.get("volume"))
            if volume is None:
                return {"ok": False, "line": "What volume?", "apps": apps}
            out = await self.control("volume", volume=max(0, min(100, volume)))
        elif action == "volume_change":
            change = num(body.get("change"))
            if not change:
                return {"ok": False, "line": "Louder or quieter?", "apps": apps}
            out = await self.control("volume", volume_change=max(-100, min(100, change)))
        elif action == "open_app":
            entry = find_app(str(body.get("app") or ""))
            if entry is None:
                return {"ok": False, "line": "That app isn't on the remote.", "apps": apps}
            out = await self.control("open_app", app=entry.name)
        elif action in NAV_KEYS:
            # taps made while one was on its way arrive as one press with a count: one ``input`` call, not several
            out = await self.control(action, repeat=num(body.get("repeat")))
        else:
            out = await self.control(action)
        line =str(out.get("line") or out.get("reason") or "")
        if not out.get("ok") and out.get("reachable") is False:
            return {"ok": False, "line": line, "apps": apps}      # the TV is gone: don't wait on it twice
        _, st = await self.state()
        fresh = {**st.public(), "now": said_state(st)} if st is not None else {}
        return {**fresh, "ok": bool(out.get("ok")), "line": line, "apps": apps}

    # -- a message that is only a TV command --
    def match(self, text: str) -> Optional[Command]:
        return match(text)

    async def run(self, cmd: Command) -> str:
        out = await self.control(cmd.action, app=cmd.app, query=cmd.query, text=cmd.text, volume=cmd.volume,
                                 volume_change=cmd.change, repeat=cmd.repeat)
        log.info("firetv: by code, %s -> %s", cmd.action, "done" if out.get("ok") else "refused")
        return str(out.get("line") or out.get("reason") or "")

    async def close(self) -> None:
        return None


def parse_mdns(text: str) -> list[tuple[str, str, int]]:
    """Rows of ``adb mdns services``: ``adb-<serial>  _adb._tcp  host:port``. Only plain ADB, not the TLS one."""
    rows = []
    for line in (text or "").splitlines():
        parts = line.split()
        if len(parts) >= 3 and parts[1].rstrip(".") == "_adb._tcp":
            host, _, port = parts[2].rpartition(":")
            if host and port.isdigit():
                rows.append((clean(parts[0], 80), host, int(port)))
    return rows


# ---- construction --------------------------------------------------------------------------------------------

def enabled(environ: Any = None) -> bool:
    env = os.environ if environ is None else environ
    return str(env.get("EVE_FIRETV", "")).strip().lower() not in ("0", "false", "no", "off")


def make_firetv(path: Optional[Path] = None, environ: Any = None, runner: Optional[Runner] = None,
                capture: Optional[Capture] = None) -> Optional[FireTV]:
    """The owner's Fire TV, or None when switched off or never paired (then no tool is lent)."""
    if not enabled(environ):
        log.info("firetv: off (EVE_FIRETV)")
        return None
    p = (path or CONFIG_PATH).expanduser()
    cfg = load(p)
    if not cfg:
        log.info("firetv: not paired (eve-bridge firetv pair)")
        return None
    log.info("firetv: on")
    return FireTV(cfg, runner=runner, capture=capture, adb=find_adb(environ), path=p)


# ---- the CLI -------------------------------------------------------------------------------------------------

async def pair(host: str = "", port: int = DEFAULT_PORT, path: Optional[Path] = None,
               out: Callable[[str], Any] = print, runner: Optional[Runner] = None, environ: Any = None) -> int:
    """Find the TV (by mDNS unless a host is given), connect, check it allows this Mac, and save it."""
    adb = find_adb(environ)
    if not adb:
        out(INSTALL_LINE)
        return 1
    probe = FireTV({"host": host or "0.0.0.0", "port": port}, runner=runner, adb=adb)
    found = await probe.discover()
    mdns = ""
    if not host:
        if not found:
            out("No Fire TV found on the network. On the TV: Settings > My Fire TV (Device & Software on some "
                "TVs) > Developer Options > ADB Debugging on. Or give its address: eve-bridge firetv pair --host ADDRESS")
            return 1
        if len(found) > 1:
            out("More than one ADB device is on the network. Pick one with --host:")
            for name, h, pt in found:
                out(f"  {h}:{pt}  {name}")
            return 1
        mdns, host, port = found[0]
    else:
        mdns = next((name for name, h, pt in found if h == host and pt == port), "")
    tv = FireTV({"host": host, "port": port, "mdns": mdns}, runner=runner, adb=adb)
    r = await tv._connect()
    if not r.ok:
        out(r.line if r.why != "unauthorized" else ALLOW_LINE + " Then run this again.")
        return 1
    _, rc, text = await tv.shell(_sh("getprop", "ro.product.model") + "; echo @@; " + _sh("getprop", "ro.serialno"))
    model_raw, _, serial_raw = (text or "").partition("@@")
    model, serial = clean(model_raw, 60), clean(serial_raw, 60)
    if not mdns and re.fullmatch(r"[A-Za-z0-9]+", serial or ""):
        mdns = f"adb-{serial}"          # the name adbd advertises itself by: kept to find the TV again
    cfg = {"host": host, "port": port, "mdns": mdns, "model": model}
    p = save(cfg, path)
    out(f"Paired with {model or 'the TV'} at {host}:{port}. Saved to {p} (private). "
        "Restart the daemon to lend it to the chat and the voice.")
    return 0


def cli(argv: Sequence[str], path: Optional[Path] = None, out: Callable[[str], Any] = print,
        runner: Optional[Runner] = None, capture: Optional[Capture] = None, environ: Any = None) -> int:
    """``eve-bridge firetv pair [--host H] [--port P] | status | on | off | key KEY [--times N] | volume N|up|down
    | mute | unmute | open APP | apps | youtube QUERY | type TEXT | screenshot [--out PATH] | forget``."""
    import argparse

    p = argparse.ArgumentParser(prog="eve-bridge firetv")
    sub = p.add_subparsers(dest="act", required=True)
    pr = sub.add_parser("pair", help="Find the Fire TV on the network and save it (ADB debugging on, on the TV)")
    pr.add_argument("--host", default="", help="the TV's address, when mDNS doesn't find it")
    pr.add_argument("--port", type=int, default=DEFAULT_PORT)
    sub.add_parser("forget", help="Forget the paired TV")
    sub.add_parser("status", help="Power, the app in front, the volume")
    sub.add_parser("on", help="Wake the TV")
    sub.add_parser("off", help="Put the TV to sleep")
    k = sub.add_parser("key", help="Press a remote key: " + ", ".join([*NAV_KEYS, *MEDIA_KEYS]))
    k.add_argument("key")
    k.add_argument("--times", type=int, default=1)
    v = sub.add_parser("volume", help="Set the volume (0-100) or step it (up, down)")
    v.add_argument("level")
    sub.add_parser("mute")
    sub.add_parser("unmute")
    o = sub.add_parser("open", help="Open an app by name")
    o.add_argument("app", nargs="+")
    sub.add_parser("apps", help="The apps eve knows by name that are on the TV")
    y = sub.add_parser("youtube", help="Search YouTube on the TV")
    y.add_argument("query", nargs="+")
    t = sub.add_parser("type", help="Type into the TV's focused text box")
    t.add_argument("text", nargs="+")
    s = sub.add_parser("screenshot", help="Save a PNG of the screen")
    s.add_argument("--out", default="")
    a = p.parse_args(list(argv))
    cfg_path = (path or CONFIG_PATH).expanduser()

    if a.act == "pair":
        return asyncio.run(pair(a.host.strip(), a.port, cfg_path, out, runner, environ))
    if a.act == "forget":
        try:
            cfg_path.unlink()
            out(f"Forgot the TV ({cfg_path}).")
        except FileNotFoundError:
            out("No TV was paired.")
        return 0
    cfg = load(cfg_path)
    if not cfg:
        out(f"No TV paired ({cfg_path}). Run: eve-bridge firetv pair")
        return 1
    tv = FireTV(cfg, runner=runner, capture=capture, adb=find_adb(environ), path=cfg_path)

    async def go() -> dict[str, Any]:
        if a.act == "status":
            st = await tv.status()
            if st.get("ok") and cfg.get("model"):
                st["line"] = f"{st['line']}\n{clean(cfg['model'], 60)} at {tv.serial}"
            return st
        if a.act == "on":
            return await tv.control("power_on")
        if a.act == "off":
            return await tv.control("power_off")
        if a.act == "key":
            key = KEY_ALIASES.get(a.key.lower(), a.key.lower().replace("-", "_"))
            if key not in NAV_KEYS and key not in MEDIA_KEYS:
                return {"ok": False, "line": f"Unknown key {a.key!r}. Keys: " + ", ".join([*NAV_KEYS, *MEDIA_KEYS])}
            return await tv.control(key, repeat=a.times)
        if a.act == "volume":
            lv = a.level.lower()
            if lv in ("up", "down"):
                return await tv.control("volume", volume_change=VOLUME_STEP if lv == "up" else -VOLUME_STEP)
            if not lv.rstrip("%").isdigit():
                return {"ok": False, "line": "Give a level 0-100, up or down."}
            return await tv.control("volume", volume=int(lv.rstrip("%")))
        if a.act in ("mute", "unmute"):
            return await tv.control(a.act)
        if a.act == "open":
            return await tv.control("open_app", app=" ".join(a.app))
        if a.act == "youtube":
            return await tv.control("youtube_search", query=" ".join(a.query))
        if a.act == "type":
            return await tv.control("type_text", text=" ".join(a.text))
        if a.act == "screenshot":
            shot = await tv.screenshot(out_path=Path(a.out) if a.out else None)
            return {**shot, "line": f"Saved {shot['path']}" if shot.get("ok") else shot.get("line", "")}
        if a.act == "apps":
            have = set(await tv.packages())
            if not have:
                return {"ok": False, "line": UNREACHABLE_LINE}
            names = [x.name for x in APPS if x.package in have]
            return {"ok": True, "line": "On the TV: " + ", ".join(names) + "." if names else "None of the apps "
                    "eve knows by name is on the TV."}
        return {"ok": False, "line": "Unknown command."}

    result = asyncio.run(go())
    if result.get("line"):
        for line in str(result["line"]).splitlines():
            out(line)
    return 0 if result.get("ok") else 1
