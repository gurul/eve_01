"""eve's two editions, full and lite, and the guard eve lite's daemon runs inside.

eve lite (docs/plans/eve-lite.md, docs/lite.md) is a second way to install and run eve, for people other than the owner:
one pasted line, two browser sign-ins (OpenRouter, Composio), and a web app on the same Wi-Fi. It is the same code with
other switches, chosen by one variable:

    EVE_PROFILE    unset or "full": today's eve, the owner's, unchanged
                   "lite": eve lite
                   anything else: ProfileError, and the CLI exits 78 (EX_CONFIG)

Only the lite installer writes it (the env file, and the LaunchAgent's own environment, which the env file cannot
override: envfile.py fills gaps only). Every lite branch in the code asks ``is_lite()``. With EVE_PROFILE unset nothing
here changes anything: ``apply`` returns [] and leaves the environment as it was, ``install_firewall`` installs nothing.

What eve lite is made of here:

1. ``apply(os.environ)``, run by cli.main right after the env file loads. It forces every switch in LITE_FORCED, so a
   door, lane, device or service eve lite does not have is off however the environment was set; it fills
   LITE_DEFAULTS where they are unset (lite's own IPC socket, never full eve's /tmp one); and it removes every
   secret-shaped variable but LITE_SECRET_ALLOW, so a key left in a shell can never switch a feature on. One log line,
   names only.
2. LITE_CLASSIFIED: every EVE_* name a module of the lite composition reads, and why lite forces it or leaves it alone.
   test_profile_classified.py works the module set out from the imports and fails on a name missing here, so a new
   switch in code eve lite runs is a decision someone made, not an accident.
3. LITE_TOOL_NAMES and LITE_TOOL_PREFIXES: the only tools the lite brain offers and runs (telegram.py filters with
   ``filter_tools`` and refuses with ``lite_tool_allowed`` before any handler).
4. ``install_firewall()``, the first thing the lite daemon does (lite.run_daemon): a process firewall made of a Python
   audit hook, which cannot be removed once added. Python reports every process start and file open to it:
   - A process may start only when its program is one of EXEC_ALLOW: pmset (the battery answer), ifconfig (the
     watcher's address guard), scutil (the LAN certificate's name). Everything else is refused, the developer-tool
     shims first (toolchain.py: on a Mac without Xcode's tools, git, clang, make or python3 opens their install
     dialog), then osascript, screencapture, codex, claude, cloudflared, adb and the shell itself.
   - A file inside the folders macOS guards with a privacy prompt (PROTECTED_HOME, PROTECTED_ROOTS: Desktop, Documents,
     Downloads, iCloud Drive, Mail, Messages, other apps' data, external volumes) is refused to open, list, create,
     move or delete, as is a path that reaches one through symlinks.
   A refusal is a PermissionError where the call was made. The hook logs each program or folder name once, never a
   path, an argument or a value. Mode "audit" refuses nothing and only counts and logs, to find what a new code path
   starts. It is Python's guard only: native code (PortAudio, ONNX, ctypes) can go around it (the spec's Risk 5).
"""

from __future__ import annotations

import contextlib
import errno
import logging
import os
import stat
import sys
import threading
import unicodedata
from typing import Any, Iterable, Mapping, MutableMapping, Optional
from urllib.parse import unquote

from . import app_registry

log = logging.getLogger(__name__)

PROFILE_ENV = "EVE_PROFILE"
FULL = "full"
LITE = "lite"
LITE_CHAT_ID = 1                  # the lite brain's one chat: the web app's (telegram.configured gives it as the owner)


class ProfileError(ValueError):
    """EVE_PROFILE holds something other than full or lite. The CLI exits 78 (EX_CONFIG) on it."""


def active(env: Optional[Mapping[str, str]] = None) -> str:
    """The edition for ``env`` (default: this process's environment): "full" or "lite". ProfileError otherwise."""
    raw = (os.environ if env is None else env).get(PROFILE_ENV)
    value = str(raw or "").strip().lower()
    if value in ("", FULL):
        return FULL
    if value == LITE:
        return LITE
    raise ProfileError(f"{PROFILE_ENV} must be {LITE!r} or {FULL!r}, or unset; not {str(raw)[:24]!r}")


def is_lite(env: Optional[Mapping[str, str]] = None) -> bool:
    """Whether ``env`` (default: this process's environment) is eve lite. Every lite branch asks here."""
    return active(env) == LITE


# ---- the switches eve lite forces (docs/plans/eve-lite.md M1) -------------------------------------------------------
#
# Each value reads as off (or as lite's one choice) through its own module's configured() or enabled()
# (test_profile_lite_gate.py checks every one, against an environment that turned it on). Groups as in the spec.

LITE_FORCED: dict[str, str] = {
    # doors: the brain is app-only and nothing reaches Telegram, the Mini App, the ntfy rendezvous or push
    "EVE_TELEGRAM": "app", "EVE_MINIAPP": "0", "EVE_LINK_RENDEZVOUS": "0", "EVE_NOTIFY": "0",
    # routing: the routers were fitted to full eve's split; Jev, for anything left that calls it, on OpenRouter
    "EVE_JEV_ROUTE": "openrouter", "EVE_TOOLSET_ROUTER": "off", "EVE_GRADE_ROUTER": "off",
    "EVE_SEARCH_ROUTER": "off", "EVE_MENU_TOOLS": "off",
    # memory: on, with OpenRouter's embedder; no second-brain vault, claude-mem or ROS bridge
    "EVE_MEMORY": "1", "EVE_MEM0_EMBEDDER": "openrouter", "EVE_SECOND_BRAIN": "0", "EVE_CLAUDE_MEM": "0",
    "EVE_ROSBRIDGE": "0",
    # apps: Composio Connect only (composio_connect.py); not direct Google, not Composio's Platform path, no sharing.
    # Gmail drafts run as routine writes; a send still asks with the whole email (composio_tools.decide)
    "EVE_APPS": "0", "EVE_COMPOSIO": "0", "EVE_COMPOSIO_POLICY": "gmail=auto", "EVE_SHARE": "0",
    # web: the plain reader and OpenRouter only (no TinyFish, Firecrawl, Monid, headless Chromium or curl_cffi)
    "EVE_WEB_READER": "0", "EVE_MONID": "0", "EVE_WATCH_HOSTED": "0", "EVE_WATCH_BROWSER": "0", "EVE_WATCH_TLS": "0",
    # the Mac: no microphone, keyboard, camera, screen, Codex, lanes, Chrome, coding sessions or learning server
    "EVE_VOICE": "0", "EVE_CALL_LIVE_STT": "0", "EVE_LISTEN_KEY": "off", "EVE_MAC_CAMERA": "off", "EVE_SCENE": "0",
    "EVE_EXPLORE": "0", "EVE_ROBOT": "0", "EVE_COMPUTER_CONTROL": "0", "EVE_CODEX_PREWARM": "0", "EVE_DESK_LANE": "0",
    "EVE_BROWSER_ATTACH": "0", "EVE_BROWSER_OWN": "0", "EVE_BROWSER_STEEL": "0", "EVE_WORK_SESSIONS": "0", "EVE_LEARNING": "0",
    # devices: the owner's apps (app_registry; any app added later is forced off below) and the phone's faces
    "EVE_LIGHTS": "0", "EVE_SPOTIFY": "0", "EVE_FIRETV": "0", "EVE_PIKA": "0", "EVE_OBSBOT": "0",
    "EVE_PHONE_EXPRESSIONS": "0",
    # other: Meet, America, OpenTable, AgentCard, Canvas, the chief of staff, Updates, phone location, the command-risk
    # gate on Claude Code's hooks, and the release check (lite updates by re-running its install line)
    "EVE_MEET": "0", "EVE_AMERICA": "0", "EVE_OPENTABLE": "0", "EVE_AGENTCARD": "0", "EVE_CANVAS": "0",
    "EVE_CHIEF": "off", "EVE_UPDATES": "0", "EVE_PHONE_LOCATION": "0", "EVE_COMMAND_RISK": "off",
    "EVE_BRIDGE_NO_UPDATE_CHECK": "1",
}
# An app added with `eve-bridge app new` brings its own switch (app_registry.APPS): forced off too, so a new app never
# reaches eve lite by default and its author edits nothing here.
LITE_FORCED.update({app.switch: "0" for app in app_registry.APPS if app.switch not in LITE_FORCED})

# Set only where unset (or empty): lite's IPC socket in its own folder, so it never shares full eve's /tmp/eve-bridge.sock.
# A leading "~/" is the environment's HOME.
LITE_DEFAULTS: dict[str, str] = {"EVE_BRIDGE_SOCK": "~/.eve-lite/ipc.sock"}

# The one secret eve lite keeps: the key its OpenRouter sign-in minted. Composio Connect's tokens live in a file
# (composio-connect.json), not the environment.
LITE_SECRET_ALLOW: frozenset[str] = frozenset({"OPENROUTER_API_KEY"})
SECRET_SUFFIXES = ("_KEY", "_TOKEN", "_SECRET", "_PASSWORD", "_CLIENT_ID")
SECRET_NAMES: frozenset[str] = frozenset({"EVE_TELEGRAM_OWNER"})    # not a secret, but it is what opens Telegram
# Names a lite module reads that mention a key, token, secret, password or sign-in and hold none: what each holds.
# test_profile_classified.py fails on any other such name until it is shaped like a secret, listed in SECRET_NAMES
# (apply removes it) or listed here.
NOT_SECRET_NAMES: dict[str, str] = {
    "EVE_KEY_METHOD": "how the desk's key presses are posted (full eve's desk)",
    "EVE_KEY_SOURCE": "which event source the desk's key presses come from (full eve's desk)",
    "EVE_CODEX_AUTH": "which sign-in Codex runs with: a mode, not a credential (Codex is off in lite)",
    "EVE_VAULT_KEYCHAIN": "where the password vault's keychain file is: a path (the vault is off in lite)",
}


def is_secret_name(name: str) -> bool:
    """Whether an environment variable's name looks like a secret's: ends in _KEY, _TOKEN, _SECRET, _PASSWORD or
    _CLIENT_ID (in any case), or is EVE_TELEGRAM_OWNER."""
    upper = str(name).upper()
    return upper.endswith(SECRET_SUFFIXES) or upper in SECRET_NAMES


def _expand(value: str, environ: Mapping[str, str]) -> str:
    if value.startswith("~/"):
        home = str(environ.get("HOME") or os.path.expanduser("~"))
        return os.path.join(home, value[2:])
    return value


def apply(environ: MutableMapping[str, str]) -> list[str]:
    """Make ``environ`` eve lite's when EVE_PROFILE says lite: the names it changed, sorted. [] and nothing touched
    otherwise. ProfileError on a value that is neither (the CLI exits 78).

    Secret-shaped variables go first (all but LITE_SECRET_ALLOW, and never a forced or default name: EVE_LISTEN_KEY
    ends in _KEY and is the keyboard key that opens the Mac's microphone), then every LITE_FORCED value is set, then
    LITE_DEFAULTS fill what is unset. One log line: counts and names, never a value. cli.main may run this before
    logging is set up: the names returned let it say them once it is."""
    if active(environ) != LITE:
        return []
    keep = LITE_SECRET_ALLOW | LITE_FORCED.keys() | LITE_DEFAULTS.keys()
    removed = sorted(name for name in list(environ) if is_secret_name(name) and name not in keep)
    for name in removed:
        del environ[name]
    forced = [name for name, value in LITE_FORCED.items() if environ.get(name) != value]
    for name in forced:
        environ[name] = LITE_FORCED[name]
    defaulted = [name for name in LITE_DEFAULTS if not str(environ.get(name) or "").strip()]
    for name in defaulted:
        environ[name] = _expand(LITE_DEFAULTS[name], environ)
    log.info("profile: eve lite: %d switches forced, %d already so; set %s; removed %s", len(forced),
             len(LITE_FORCED) - len(forced), ", ".join(defaulted) or "nothing", ", ".join(removed) or "nothing")
    return sorted({*removed, *forced, *defaulted})


# ---- every switch the lite composition reads, and why lite leaves it as it is -----------------------------------
#
# test_profile_classified.py finds every EVE_* name in the modules the lite daemon imports (its composition and
# everything their imports reach, function-level imports included) and fails on a name missing here. A reason is:
#   "forced to …" / "default when unset: …"     generated from LITE_FORCED and LITE_DEFAULTS below;
#   "secret-shaped: …"                          apply() removes it (is_secret_name);
#   "inert while EVE_X … forced …: …"           a setting of a feature whose switch lite forces (the test checks that
#                                               every EVE_X named there is in LITE_FORCED);
#   "kept: …"                                   a setting of a feature lite runs, whose default is right for lite;
#   "not run in lite: …"                        read only by code lite imports and never runs.
# A name built at run time is written with * for the part that varies (sound.py's EVE_<SOUND>_FILE).

def _why(reason: str, *names: str) -> dict[str, str]:
    return dict.fromkeys(names, reason)


_MAC_TASKS = ("inert while EVE_COMPUTER_CONTROL, EVE_DESK_LANE, EVE_BROWSER_ATTACH and EVE_BROWSER_OWN are forced off: "
              "a setting of the Mac's tasks (Codex, the desk and browser lanes, launches, the password vault's fills); "
              "lite's brain has no agent and none of their tools")
_DESK_VOICE = ("inert while EVE_VOICE is forced off: a setting of the desk voice and room notes, which use the Mac's own "
               "microphone; lite talks only through the web app's push-to-talk")
_FULL_DAEMON = ("not run in lite: full eve's daemon (daemon.py) and the robot, camera and serial link only it builds; "
                "lite's daemon (lite.py) never imports it")

LITE_CLASSIFIED: dict[str, str] = {
    PROFILE_ENV: "the edition switch itself: unset or full is the owner's eve, lite is eve lite",
    **{name: f"forced to {value}" for name, value in LITE_FORCED.items()},
    **{name: f"default when unset: {value}" for name, value in LITE_DEFAULTS.items()},
    **_why("secret-shaped: apply() removes it, so no key or owner id switches a feature on",
           "EVE_TELEGRAM_TOKEN", "EVE_TELEGRAM_OWNER", "EVE_GITHUB_MCP_TOKEN", "EVE_GOOGLE_MAPS_KEY",
           "EVE_OPENAI_ADMIN_KEY", "EVE_ANTHROPIC_ADMIN_KEY", "EVE_ENTER_KEY"),
    # what eve lite runs
    **_why("kept: the chat brain's models and efforts (luna; App Think on sol); the defaults are lite's",
           "EVE_TELEGRAM_MODEL", "EVE_TELEGRAM_EFFORT", "EVE_APP_THINK_MODEL", "EVE_APP_THINK_EFFORT"),
    **_why("kept: think_hard (astra), on in lite", "EVE_THINK", "EVE_THINK_MODEL", "EVE_THINK_EFFORT",
           "EVE_THINK_TIMEOUT_SECS"),
    **_why("kept: small talk, on in lite", "EVE_SMALL_TALK", "EVE_SMALL_TALK_MODEL"),
    **_why("kept: science search, on in lite (no key)", "EVE_SCIENCE_SEARCH"),
    **_why("kept: Muse on the owner's Raspberry Pi (muse_pi.py); off until set, in lite and full eve alike",
           "EVE_MUSE_PI"),
    **_why("kept: card pictures from Wikipedia (find_image), on in lite (no key)", "EVE_FIND_IMAGE"),
    **_why("kept: Horizontal History's topic titles, through OpenRouter", "EVE_TOPIC_TITLES",
           "EVE_TOPIC_TITLE_MODEL"),
    **_why("inert while EVE_BROWSER_STEEL is forced off: a setting of the local Steel browser lane (steel_local.py)",
           "EVE_STEEL_DIR", "EVE_STEEL_PROFILE", "EVE_STEEL_URL"),
    **_why("kept: web search through OpenRouter (Perplexity), on in lite", "EVE_WEB_SEARCH", "EVE_WEB_SEARCH_MODEL",
           "EVE_WEB_SEARCH_RESULTS", "EVE_WEB_SEARCH_MAX_USES"),
    **_why("kept: watches, on in lite, with the plain reader and OpenRouter", "EVE_WATCH", "EVE_WATCH_FILE",
           "EVE_WATCH_RATE", "EVE_WATCH_BURST", "EVE_WATCH_HOST_GAP", "EVE_WATCH_MODEL", "EVE_WATCH_MODEL_CALLS",
           "EVE_WATCH_VISION_MODEL"),
    **_why("kept: the chat log, on in lite", "EVE_CHAT_LOG", "EVE_CHAT_LOG_FILE", "EVE_CHAT_LOG_DAYS",
           "EVE_CHAT_LOG_LINES"),
    **_why("kept: memory, on in lite: its folders (under ~/.config/eve-bridge by default) and its experiments "
           "(off by default)", "EVE_MEMORY_DIR", "EVE_RECORDS", "EVE_MEM0", "EVE_MEMORY_HYBRID", "EVE_MEMORY_RETRY",
           "EVE_MEMORY_TIMEARGS", "EVE_MEMORY_SOURCES", "EVE_MEMORY_VALIDITY", "EVE_DEBRIEF_DIR", "EVE_NOTES_DIR"),
    **_why("kept: push-to-talk in the web app (speech to text and eve's voice, both through OpenRouter)",
           "EVE_CALL_STT_MODEL", "EVE_CALL_LANGUAGE", "EVE_CALL_OR_TTS_MODEL", "EVE_CALL_OR_VOICE"),
    **_why("kept: Jev's model and wait, for anything left that calls it (its route is forced to OpenRouter)",
           "EVE_JEV_MODEL", "EVE_JEV_TIMEOUT"),
    **_why("kept: where spend lines go", "EVE_SPEND_DIR"),
    **_why("kept: the clock's time zone and the weather's place, set by the user or not at all", "EVE_TIMEZONE",
           "EVE_LOCATION", "EVE_WEATHER_PLACE", "EVE_WEATHER_LAT", "EVE_WEATHER_LON"),
    **_why("kept: where the env file is (lite's installer writes the default one)", "EVE_ENV_FILE"),
    **_why("kept: where lite_web serves the web page from (default: the packaged web_lite)", "EVE_WEB_DIR"),
    # settings of features lite forces off
    **_why(_MAC_TASKS, "EVE_AGENT_EXEC_TIMEOUT", "EVE_AGENT_MAX_SECS", "EVE_AGENT_MAX_TURNS", "EVE_AGENT_MODEL",
           "EVE_AGENT_PLAN_REASONING", "EVE_AGENT_REASONING", "EVE_AGENT_RUNS_DIR", "EVE_AGENT_VERIFY",
           "EVE_AGENT_VERIFY_REASONING", "EVE_BROWSER_INTENT", "EVE_BROWSER_MODEL", "EVE_FAST_LANE", "EVE_FAST_LANE_DECIDE",
           "EVE_LANE_FIRST", "EVE_PLAN_EXEC", "EVE_PLAN_EXEC_REASONING", "EVE_REFLEXES", "EVE_ROUTER_MODEL",
           "EVE_QUIT_ALL_KEEP", "EVE_CODEX_AUTH", "EVE_CODEX_BIN", "EVE_CODEX_ROUTINE_ACCESS",
           "EVE_CODEX_SITE_ACCESS", "EVE_AX_PRESS", "EVE_DECISIONS", "EVE_JEV_GROUPS", "EVE_JEV_STEP",
           "EVE_CHROME_DEBUG_PORT", "EVE_CHROME_DIR", "EVE_CHROME_PROFILE", "EVE_OWN_CHROME_DIR", "EVE_VAULT_INDEX",
           "EVE_VAULT_KEYCHAIN"),
    **_why(_DESK_VOICE, "EVE_CAPTION_CPS", "EVE_LIVE_BACKEND_EFFORT", "EVE_LIVE_BACKEND_MODEL", "EVE_LIVE_MODEL",
           "EVE_LIVE_WEB_SEARCH", "EVE_VOICE_IDLE_SECS", "EVE_VOICE_NAME", "EVE_VOICE_OUTPUT", "EVE_INTENT",
           "EVE_INTENT_MODEL", "EVE_VOICE_GATE", "EVE_VOICE_GATE_MODEL", "EVE_KWS_MODEL_DIR", "EVE_LESSON_THRESHOLD",
           "EVE_LESSON_WORD", "EVE_MIC", "EVE_MIC_ALWAYS", "EVE_WAKE_THRESHOLD", "EVE_WAKE_WORD",
           "EVE_NOTES_DIARIZE", "EVE_NOTES_KEEP_TRANSCRIPT", "EVE_NOTES_MAX_MINUTES", "EVE_NOTES_MODEL_STT",
           "EVE_NOTES_OWNER_NAME", "EVE_NOTES_OWNER_VOICE", "EVE_NOTES_SEGMENT_SECS"),
    **_why("inert while EVE_MINIAPP is forced off: the Mini App, its tunnel and its app builder",
           "EVE_CLOUDFLARED", "EVE_MINIAPP_DAILY_USD", "EVE_MINIAPP_MAKE_EFFORT", "EVE_MINIAPP_MODEL",
           "EVE_APP_JOURNEYS"),
    **_why("inert while EVE_TELEGRAM is forced to app: Telegram's typing drafts; nothing reaches Telegram",
           "EVE_TELEGRAM_DRAFTS"),
    **_why("inert while EVE_LINK_RENDEZVOUS is forced off: the ntfy rendezvous server", "EVE_LINK_RENDEZVOUS_URL"),
    **_why("inert while EVE_NOTIFY is forced off: ntfy push", "EVE_NOTIFY_PREVIEW", "EVE_NOTIFY_URL"),
    **_why("inert while EVE_APPS is forced off: direct Google, GitHub and Maps", "EVE_APPS_DIR", "EVE_APPS_POLICY",
           "EVE_APPS_TIMEOUT_SECS"),
    **_why("inert while EVE_COMPOSIO is forced off: Composio's Platform path (lite's apps go through Composio "
           "Connect)", "EVE_COMPOSIO_STATE", "EVE_COMPOSIO_TIMEOUT_SECS", "EVE_COMPOSIO_TOOLKITS"),
    **_why("inert while EVE_CLAUDE_MEM is forced off: claude-mem", "EVE_CLAUDE_MEM_MIRROR", "EVE_CLAUDE_MEM_URL"),
    **_why("inert while EVE_ROSBRIDGE is forced off: the ROS bridge", "EVE_ROSBRIDGE_HOST", "EVE_ROSBRIDGE_PORT"),
    **_why("inert while EVE_SECOND_BRAIN is forced off: the second-brain vault", "EVE_VAULT"),
    **_why("inert while EVE_MONID is forced off: Monid", "EVE_MONID_CLI", "EVE_MONID_PER_DAY",
           "EVE_MONID_USD_PER_DAY"),
    **_why("inert while EVE_WEB_READER is forced off: the hosted page reader", "EVE_WEB_READER_TASKS"),
    **_why("inert while EVE_WATCH_HOSTED is forced off: the watcher's hosted fetch", "EVE_WATCH_HOSTED_CALLS"),
    **_why("inert while EVE_SEARCH_ROUTER is forced off: the routed search", "EVE_SEARCH_ROUTED_SECS"),
    **_why("inert while EVE_SEARCH_ROUTER and EVE_WEB_READER are forced off: Firecrawl (its key is removed too)",
           "EVE_FIRECRAWL_PER_DAY", "EVE_FIRECRAWL_USD"),
    **_why("inert while EVE_FIRETV is forced off: the TV's adb", "EVE_ADB"),
    **_why("inert while EVE_SCENE is forced off: the room camera", "EVE_SCENE_INTERVAL_SECS", "EVE_SCENE_MODEL",
           "EVE_SCENE_STALE_SECS"),
    **_why("inert while EVE_EXPLORE is forced off: the robot's exploring and its notes", "EVE_EXPLORE_AFTER_MIN",
           "EVE_EXPLORE_CYCLE_MIN", "EVE_NOTES_MODEL", "EVE_NOTES_PER_HOUR"),
    **_why("inert while EVE_LEARNING is forced off: the learning server", "EVE_LEARNING_PORT"),
    **_why("inert while EVE_PHONE_EXPRESSIONS is forced off: the phone's faces", "EVE_EXPRESSIONS_FILE",
           "EVE_EXPRESSION_BACKEND", "EVE_EXPRESSION_MODEL"),
    **_why("inert while EVE_PHONE_LOCATION is forced off: the phone's location", "EVE_PHONE_LOCATION_FILE"),
    **_why("inert while EVE_AGENTCARD is forced off: AgentCard", "EVE_AGENTCARD_ADDRESS_FILE",
           "EVE_AGENTCARD_TIMEOUT_SECS"),
    **_why("inert while EVE_AMERICA is forced off: America", "EVE_AMERICA_JEV", "EVE_AMERICA_TRANSPORT"),
    **_why("inert while EVE_CHIEF is forced off: the chief of staff", "EVE_CHIEF_ASSESS_EFFORT",
           "EVE_CHIEF_ASSESS_MODEL", "EVE_CHIEF_DIR", "EVE_CHIEF_PUSH", "EVE_CHIEF_REPLAN"),
    **_why("inert while EVE_MEET is forced off: the Meet notetaker", "EVE_MEET_GUEST_NAME", "EVE_MEET_LOBBY_MINUTES",
           "EVE_MEET_MAX_HOURS", "EVE_MEET_PROFILE", "EVE_MEET_QUIET", "EVE_MEET_SUMMARY_MODEL"),
    **_why("inert while EVE_UPDATES is forced off: the Updates pane", "EVE_UPDATES_EVERY_MIN", "EVE_UPDATES_MODEL"),
    # read only by code lite imports and never runs
    **_why("not run in lite: Claude Code's sessions, launches and permission questions; lite's brain lends none "
           "(no sessions, a launcher that refuses) and offers no coding tool", "EVE_CLAUDE_CONFIG_DIRS",
           "EVE_CLAUDE_TERMINAL", "EVE_CODE_AREAS", "EVE_CODE_ROOT", "EVE_TELEGRAM_ASK"),
    **_why("not run in lite: the Claude Code hooks' audit log (lite installs no hooks)", "EVE_BRIDGE_AUDIT"),
    **_why("not run in lite: full eve's daemon options (the robot's serial port, saved camera frames), read as the "
           "CLI's defaults; lite's daemon takes neither", "EVE_SERIAL_PORT", "EVE_SAVE_FRAMES"),
    **_why(_FULL_DAEMON, "EVE_CJK_TARGET", "EVE_COMPUTER", "EVE_HEAD_MODEL", "EVE_OWNER_PRINTS", "EVE_OWNER_THRESHOLD",
           "EVE_PHOTOS_MAX", "EVE_PHOTOS_MAX_MB", "EVE_FOLLOW_SPEAKER", "EVE_PRESENCE_FILE", "EVE_KEY_METHOD",
           "EVE_KEY_SOURCE", "EVE_*_FILE"),
}


# ---- the tools eve lite has (docs/plans/eve-lite.md M2) ---------------------------------------------------------

LITE_TOOL_NAMES: frozenset[str] = frozenset({
    "remember", "think_hard",
    "recent_messages", "web_search", "science_search",
    "memory_search", "memory_read", "forget_preview", "forget_apply", "recent_conversation",
    "watch_add", "watch_list", "watch_remove", "watch_pause", "watch_resume", "watch_set_end",
})
LITE_TOOL_PREFIXES: tuple[str, ...] = ("COMPOSIO_",)      # Composio Connect's meta tools, as its server names them


def tool_name(tool: Any) -> str:
    """A tool definition's name: Responses-style ``{"name"}``, chat-style ``{"function": {"name"}}``, or a hosted
    tool's ``type`` (``{"type": "web_search"}``). "" when it has none."""
    if not isinstance(tool, Mapping):
        return ""
    name = tool.get("name")
    if not name and isinstance(tool.get("function"), Mapping):
        name = tool["function"].get("name")
    if not name and tool.get("type") != "function":
        name = tool.get("type")
    return name if isinstance(name, str) else ""


def lite_tool_allowed(name: Any) -> bool:
    """Whether eve lite has the tool ``name``: one of LITE_TOOL_NAMES, or a name starting with a LITE_TOOL_PREFIXES
    entry (and longer than it)."""
    if not isinstance(name, str):
        return False
    return name in LITE_TOOL_NAMES or any(name.startswith(p) and len(name) > len(p) for p in LITE_TOOL_PREFIXES)


def filter_tools(tools: Iterable[dict[str, Any]]) -> list[dict[str, Any]]:
    """The tools eve lite has, in their order and as they are (the same objects): the rest are left out."""
    return [tool for tool in tools if lite_tool_allowed(tool_name(tool))]


# ---- the process firewall: the lite daemon's, nowhere else ------------------------------------------------------

EXEC_ALLOW: frozenset[str] = frozenset({
    "/usr/bin/pmset",            # the battery quick answer (quick_answers.py)
    "/sbin/ifconfig",            # the watcher's guard against pages that reach this Mac's own network (watch.py)
    "/usr/sbin/scutil",          # the Mac's LocalHostName, the LAN certificate's name (lan_net.py)
})
# Folders under the home that macOS guards with a privacy prompt (or Full Disk Access): the spec's list, then the same
# kind it did not name: cloud drives' File Provider folders (~/Dropbox and its like are links into it), Reminders,
# HomeKit, cookies, call history, iPhone backups, the privacy database itself and the Trash.
PROTECTED_HOME: tuple[str, ...] = (
    "Desktop", "Documents", "Downloads", "Pictures", "Movies", "Music",
    "Library/Mobile Documents", "Library/Mail", "Library/Messages", "Library/Calendars",
    "Library/Application Support/AddressBook", "Library/Safari", "Library/Containers", "Library/Group Containers",
    "Library/CloudStorage", "Library/Reminders", "Library/HomeKit", "Library/Cookies",
    "Library/Application Support/CallHistoryDB", "Library/Application Support/MobileSync",
    "Library/Application Support/com.apple.TCC", ".Trash",
)
PROTECTED_ROOTS: tuple[str, ...] = ("/Volumes",)          # external and network disks
FIREWALL_MODES = ("enforce", "audit")

_SHELL = "/bin/sh"                    # what os.system and shell=True run: it can run anything, so it is never allowed
_DATA_VOLUME = "/System/Volumes/Data"  # the home folders again under the data volume's own name (a firmlink, no symlink)
_MAX_LINKS = 40                       # symlinks one path may pass through before it is a loop (the kernel allows 32)
_FORK_EXEC = "_posixsubprocess.fork_exec"   # raised here (_wrap_fork_exec): the C function raises no audit event
_EXEC_EVENTS = frozenset({"subprocess.Popen", "os.posix_spawn", "os.exec", "os.spawn", "os.system", _FORK_EXEC})
# The file events, each with the positions of its path arguments (os.symlink: only where the link is made)
_PATH_EVENTS: dict[str, tuple[int, ...]] = {
    "open": (0,), "os.listdir": (0,), "os.scandir": (0,), "sqlite3.connect": (0,), "os.mkdir": (0,),
    "os.rmdir": (0,), "os.remove": (0,), "os.rename": (0, 1), "os.link": (0, 1), "os.symlink": (1,),
    "os.truncate": (0,), "os.chmod": (0,), "os.chown": (0,), "os.utime": (0,), "os.chflags": (0,),
}
_WATCHED = _EXEC_EVENTS | frozenset(_PATH_EVENTS)


def _fold(path: str) -> str:
    """A path as APFS compares names: case- and Unicode-normalization-insensitive."""
    return path.lower() if path.isascii() else unicodedata.normalize("NFC", path).casefold()


def _text(value: Any) -> str:
    return os.fsdecode(os.fspath(value))


def _absolute(path: str, base: Optional[str] = None) -> str:
    return path if os.path.isabs(path) else os.path.join(base or os.getcwd(), path)


def _runnable(paths: Iterable[str]) -> list[str]:
    """The paths the kernel would run: regular files with the execute bit (what exec skips over, it never runs)."""
    return [p for p in paths if os.path.isfile(p) and os.access(p, os.X_OK)]


def _on_path(name: str, dirs: Iterable[Any], base: Optional[str] = None) -> list[str]:
    return _runnable(_absolute(os.path.join(os.fsdecode(d), name), base) for d in dirs)[:1]


def _programs(event: str, args: tuple) -> list[str]:
    """The program files a process start would run, absolute; [] when it would run nothing (a missing program fails
    on its own). Each start is read the way its own call looks programs up."""
    if event == "os.system":
        return [_SHELL]
    if event == "subprocess.Popen":                # (executable, args, cwd, env); executable is args[0] when unset
        executable, _argv, cwd, env = args
        base = _absolute(_text(cwd)) if cwd is not None else None
        name = _text(executable)
        if os.path.dirname(name):
            return _runnable([_absolute(name, base)])
        return _on_path(name, os.get_exec_path(env), base)
    if event == "os.posix_spawn":                  # (path, argv, env), for posix_spawn and posix_spawnp alike
        name = _text(args[0])
        here = _runnable([_absolute(name)])
        if os.path.dirname(name):
            return here
        return here + _on_path(name, os.get_exec_path())   # ./name for posix_spawn, the PATH for posix_spawnp
    if event == "os.exec":                         # (path, args, env): os.execvp looked the PATH up already
        return _runnable([_absolute(_text(args[0]))])
    if event == "os.spawn":                        # (mode, path, args, env)
        return _runnable([_absolute(_text(args[1]))])
    if event == _FORK_EXEC:                        # (executable_list, cwd)
        executables, cwd = args
        base = _absolute(_text(cwd)) if cwd is not None else None
        return _runnable([_absolute(_text(p), base) for p in executables])[:1]
    return []


def _sqlite_paths(database: Any) -> list[Any]:
    """The files sqlite3.connect may open: the name as a file, and for a ``file:`` URI its path as well."""
    if not isinstance(database, (str, bytes, os.PathLike)):
        return []
    name = _text(database)
    if name in ("", ":memory:"):
        return []
    paths: list[Any] = [name]
    if name.startswith("file:"):
        rest = name[len("file:"):].split("?", 1)[0].split("#", 1)[0]
        if rest.startswith("//"):                  # file://host/path: the authority is not part of the path
            rest = "/" + rest[2:].partition("/")[2]
        paths.append(unquote(rest))
    return paths


class _Reached(Exception):
    def __init__(self, name: str) -> None:
        super().__init__(name)
        self.name = name


class _Guard:
    """The firewall itself: one per process (install_firewall), called by Python with every audit event."""

    def __init__(self, mode: str, roots: Iterable[tuple[str, str]], pid: int, exempt: Iterable[str] = ()) -> None:
        self.mode = mode
        self.pid = pid                                       # an exec refused in a forked child exits it (__call__)
        # each allowed program by its path and by the file it resolves to (a system path may become a link)
        self.allowed = EXEC_ALLOW | {os.path.realpath(p) for p in EXEC_ALLOW}
        self.roots = {_fold(path): name for path, name in roots}
        self.prefixes = tuple(root + "/" for root in self.roots)
        self.exempt = tuple(_fold(path).rstrip("/") + "/" for path in exempt)
        self.counts: dict[str, int] = {}
        self.logged: set[str] = set()
        self.lock = threading.Lock()
        self.local = threading.local()

    def __call__(self, event: str, args: tuple) -> None:
        if event not in _WATCHED:
            return
        local = self.local
        if getattr(local, "busy", False):
            return                       # the guard's own work (a log line opening its file) is not checked again
        # a forked child about to exit 126 (below) logs nothing: the line would be lost with its buffers
        child = event == "os.exec" and os.getpid() != self.pid
        local.busy = True
        refusal = None
        try:
            refusal = self.check(event, args)
            if refusal is not None and not (child and self.mode == "enforce"):
                self.note(refusal[0], refusal[1])
        finally:
            local.busy = False
        if refusal is not None and self.mode == "enforce":
            if child:
                os._exit(126)            # a forked child whose exec is refused must not run on as a copy of the daemon
            raise PermissionError(errno.EPERM, refusal[2])

    def check(self, event: str, args: tuple) -> Optional[tuple[str, str, str]]:
        """(count key, what the log line names, the error's words) for a refused event; None when it may go."""
        positions = _PATH_EVENTS.get(event)
        if positions is not None:
            try:
                for i in positions:
                    if i >= len(args):
                        continue
                    for value in (_sqlite_paths(args[i]) if event == "sqlite3.connect" else (args[i],)):
                        name = self.reaches(value)
                        if name:
                            return f"path:{name}", f"a file in {name}", f"eve lite stays out of {name}"
            except Exception as e:  # noqa: BLE001 — a fault in this check must not stop every file eve opens
                self.fault(type(e).__name__)
            return None
        try:
            programs = _programs(event, args)
            reals = [os.path.realpath(p) for p in programs]
        except Exception:  # noqa: BLE001 — a start this guard cannot read is refused, never let through
            return "exec:?", "a program it could not read", "eve lite does not run this program"
        for real in reals:
            if real not in self.allowed:
                name = os.path.basename(real) or "?"
                return f"exec:{name}", f"the program {name}", f"eve lite does not run {name}"
        return None

    def note(self, key: str, what: str) -> None:
        with self.lock:
            self.counts[key] = self.counts.get(key, 0) + 1
            first = key not in self.logged
            self.logged.add(key)
        if first:
            verb = "would refuse" if self.mode == "audit" else "refused"
            log.warning("firewall: %s %s (said once; firewall_counts has the count)", verb, what)

    def fault(self, kind: str) -> None:
        """A path this guard failed to check (a fault of its own, let through): said once per kind of error."""
        with self.lock:
            first = f"fault:{kind}" not in self.logged
            self.logged.add(f"fault:{kind}")
        if first:
            log.warning("firewall: could not check a path (%s); it was let through", kind)

    def reaches(self, value: Any) -> Optional[str]:
        """The protected folder a path argument is inside of, or reaches through symlinks; None for any other path,
        and for a file descriptor (its path was checked when it was opened)."""
        if value is None:
            value = "."                                   # os.listdir() and os.scandir() with no argument
        if isinstance(value, int):
            return None
        try:
            path = _text(value)
        except TypeError:
            return None
        if not path or "\x00" in path:
            return None                                   # the call refuses these itself
        try:
            path = _absolute(path)
        except OSError:
            return None                                   # no working folder: a relative path reaches nothing
        normal = os.path.normpath(path)
        name = self.lexical(normal)
        if name:
            # this process's own code, when it lives in a protected folder (a checkout under ~/Documents): only by
            # its words, so a link from a code folder into a protected one is still followed and refused below
            return None if self.exempt and (_fold(normal) + "/").startswith(self.exempt) else name
        return self.walk(path)

    def lexical(self, path: str) -> Optional[str]:
        if path.startswith("//"):
            path = "/" + path.lstrip("/")                 # normpath keeps a leading "//", which is "/" here
        folded = _fold(path)
        name = self.roots.get(folded)
        if name is None and folded.startswith(self.prefixes):
            name = next(n for root, n in self.roots.items() if folded.startswith(root + "/"))
        return name

    def walk(self, path: str) -> Optional[str]:
        """Follow ``path``'s symlinks the way the kernel does (posixpath.realpath's walk), stopping short of a protected
        folder: nothing at or inside one is looked at, since an lstat there is itself an access macOS may ask about."""
        seen: dict[str, Optional[str]] = {}
        links = [_MAX_LINKS]

        def follow(base: str, rest: str) -> str:
            if rest.startswith("/"):
                base, rest = "/", rest[1:]
            for part in rest.split("/"):
                if not part or part == ".":
                    continue
                if part == "..":
                    base = os.path.dirname(base)
                    continue
                new = os.path.join(base, part)
                name = self.lexical(new)
                if name:
                    raise _Reached(name)
                try:
                    is_link = stat.S_ISLNK(os.lstat(new).st_mode)
                except OSError:
                    is_link = False
                if not is_link:
                    base = new
                    continue
                if new in seen:
                    resolved = seen[new]
                    if resolved is None:
                        raise RecursionError("symlink loop")      # the kernel's ELOOP: the path reaches nothing
                    base = resolved
                    continue
                links[0] -= 1
                if links[0] < 0:
                    raise RecursionError("too many symlinks")
                seen[new] = None
                base = follow(base, os.readlink(new))     # a relative target is relative to the link's folder
                seen[new] = base
            return base

        try:
            return self.lexical(follow("/", path))
        except _Reached as reached:
            return reached.name
        except (OSError, RecursionError):
            return None


_GUARD: Optional[_Guard] = None
_GUARD_LOCK = threading.Lock()


def _account_home() -> str:
    try:
        import pwd

        return pwd.getpwuid(os.getuid()).pw_dir
    except (ImportError, KeyError, OSError):
        return ""


def protected_roots(environ: Optional[Mapping[str, str]] = None) -> list[tuple[str, str]]:
    """(folder, its name in a log line) for every protected folder: each of PROTECTED_HOME under $HOME, as given and
    resolved, and again under the data volume's name; then PROTECTED_ROOTS."""
    env = os.environ if environ is None else environ
    home = str(env.get("HOME") or "")
    if not os.path.isabs(home):
        home = _account_home()                            # HOME unset or relative: the account's own
    homes = [os.path.normpath(home), os.path.realpath(home)] if os.path.isabs(home) else []
    roots: list[tuple[str, str]] = []
    for base in dict.fromkeys(homes):
        for sub in PROTECTED_HOME:
            folder = os.path.join(base, sub)
            roots += [(folder, f"~/{sub}"), (_DATA_VOLUME + folder, f"~/{sub}")]
    roots += [(root, root) for root in PROTECTED_ROOTS]
    return list(dict.fromkeys(roots))


def code_roots() -> list[str]:
    """The folders this process runs its code from: Python's own, the environment's, and the one eve_bridge is
    imported from. Never refused, even inside a protected folder: macOS has already been asked about a folder Python
    started from, and a developer's checkout under ~/Documents must still import what it runs (eve lite's own install
    lives in ~/.eve-lite, outside every protected folder)."""
    package_parent = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    found = [sys.prefix, sys.exec_prefix, sys.base_prefix, sys.base_exec_prefix, package_parent]
    paths = (p for root in found if root and os.path.isabs(root) for p in (os.path.normpath(root), os.path.realpath(root)))
    return list(dict.fromkeys(p for p in paths if p != "/"))


def _prime() -> None:
    """What the standard library answers by starting a program, asked once before the hook is in, so it is cached for
    the process: platform's processor is ``uname -p`` (mem0's telemetry asks for it), and platform.platform() also runs
    ``file`` on the interpreter (the openai SDK puts it in every request's headers). Asked first under the firewall,
    each would be refused, and logged, on the first model call. Neither is a developer tool or asks for anything."""
    import platform

    with contextlib.suppress(Exception):
        _ = platform.uname().processor
        _ = platform.platform()


def _wrap_fork_exec() -> None:
    """multiprocessing's spawn and forkserver start Python through _posixsubprocess.fork_exec, which raises no audit
    event: wrapped here to raise one. subprocess bound the function when it was imported (it is imported first, here),
    so its starts are checked once, by their own subprocess.Popen event."""
    import subprocess  # noqa: F401 — before the wrap, so subprocess keeps the function it always had

    try:
        import _posixsubprocess
    except ImportError:
        return
    real = _posixsubprocess.fork_exec

    def fork_exec(*args: Any, **kwargs: Any) -> Any:
        sys.audit(_FORK_EXEC, args[1] if len(args) > 1 else (), args[4] if len(args) > 4 else None)
        return real(*args, **kwargs)

    _posixsubprocess.fork_exec = fork_exec


def install_firewall(mode: str = "enforce") -> None:
    """Turn the process firewall on, for good: an audit hook cannot be removed. Only in eve lite (EVE_PROFILE=lite):
    anywhere else this logs one line and installs nothing. "audit" lets everything through and only counts and logs;
    a later "enforce" tightens it, and nothing loosens it. A test that runs code which calls this does it in a
    subprocess (or stubs it): installed in a test runner, the hook would stay for every test after it."""
    global _GUARD
    if mode not in FIREWALL_MODES:
        raise ValueError(f"firewall mode is one of {FIREWALL_MODES}, not {mode!r}")
    if not is_lite():
        log.warning("firewall: not installed: it is eve lite's (EVE_PROFILE=lite), and this is full eve")
        return
    with _GUARD_LOCK:
        if _GUARD is not None:
            if mode == "enforce" and _GUARD.mode != "enforce":
                _GUARD.mode = "enforce"
                log.info("firewall: enforcing now (was auditing)")
            return
        _prime()
        guard = _Guard(mode, protected_roots(), os.getpid(), code_roots())
        sys.addaudithook(guard)
        _wrap_fork_exec()
        _GUARD = guard
    log.info("firewall: on (%s): %d programs may start; %d folders are refused", mode, len(EXEC_ALLOW),
             len(PROTECTED_HOME) + len(PROTECTED_ROOTS))


def firewall_counts() -> dict[str, int]:
    """What the firewall refused (or, auditing, would have), by "exec:<program>" and "path:<folder>". {} when it is not
    on or refused nothing."""
    guard = _GUARD
    if guard is None:
        return {}
    with guard.lock:
        return dict(guard.counts)
