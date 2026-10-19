"""Entry point. `eve-bridge [daemon|install|uninstall|status|notes-widget|...]`.

Under EVE_PROFILE=lite (profile.py) the command line is eve lite's own, smaller one (`_lite_main`): `eve-bridge
[daemon|install|uninstall|status|update|web|doctor|lite|memory]`."""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import logging
import os
import plistlib
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Callable, Iterator, Optional

from . import __version__, app_registry, profile
from .envfile import load_env_file
from .ipc import make_transport
from .learning import LESSON_ACTIONS


def main(argv: list[str] | None = None) -> int:
    rest = sys.argv[1:] if argv is None else list(argv)
    if rest[:1] == ["mcp"]:
        # eve's tools for a Work session (eve_mcp.py): before the env file, so the server holds none of its keys
        from . import eve_mcp
        return eve_mcp.main(rest[1:])
    if rest[:1] == ["vault-mcp"]:
        # the vault's fill tool for Codex tasks (vault_mcp.py): no env file either, it needs none of its keys
        from . import vault_mcp
        return vault_mcp.main(rest[1:])
    # Secrets and knobs the service cannot inherit from a shell
    # (OPENAI_API_KEY, EVE_*) — read before any subcommand looks at
    # os.environ. Existing variables win; the file only fills gaps.
    load_env_file()
    # eve lite (EVE_PROFILE=lite, profile.py): its switches forced, its defaults filled and every other secret removed
    # before any command reads the environment, then its own commands. Unset or "full" changes nothing here.
    try:
        applied = profile.apply(os.environ)
    except profile.ProfileError as e:
        print(f"eve-bridge: {e}", file=sys.stderr)
        return EX_CONFIG
    if profile.is_lite():
        return _lite_main(rest, applied)
    # eve's apps' own CLIs (app_registry.APPS: lights, spotify, firetv, obsbot/cam, ...): everything after the word
    # is the app's own (argparse's REMAINDER drops a leading --help)
    for app in app_registry.APPS:
        if rest[:1] and rest[0] in app.cli:
            return app.cli_mod().cli(rest[1:] or ["--help"])
    if rest[:1] == ["app"]:
        from . import app_template
        return app_template.cli(rest[1:])
    if rest[:1] == ["vault"]:
        from . import vault
        return vault.cli(rest[1:] or ["--help"])
    parser = argparse.ArgumentParser(prog="eve-bridge")
    parser.add_argument("--version", action="version", version=f"eve-bridge {__version__}")
    sub = parser.add_subparsers(dest="cmd")
    sub.add_parser("codex-home", help="Prepare Eve's separate Codex API-key home without changing your own sign-in")

    p_daemon = sub.add_parser("daemon", help="Run the bridge daemon (connects to BLE device, serves hooks)")
    p_daemon.add_argument("--socket", default=None, help="IPC path or host:port override")
    p_daemon.add_argument("--device-name", default="Claude", help="BLE name prefix to match (default: Claude)")
    p_daemon.add_argument("--device-address", default=None, help="BLE address to connect to (skips scan)")
    p_daemon.add_argument(
        "--serial-port",
        default=os.environ.get("EVE_SERIAL_PORT") or None,
        help="USB serial port of the eve (e.g. /dev/cu.usbmodem*). Uses serial instead of BLE.",
    )
    p_daemon.add_argument("--log-level", default="INFO")
    p_daemon.add_argument(
        "--save-frames",
        default=os.environ.get("EVE_SAVE_FRAMES") or None,
        metavar="DIR",
        help="Bench debugging: write received camera frames here (at most one per second)",
    )

    CONFIG_DIR_HELP = (
        "Claude Code config home to operate on (default: $CLAUDE_CONFIG_DIR, else ~/.claude). "
        "Wrappers such as era-code run sessions against a private home — hooks installed into "
        "the wrong one never fire and never warn."
    )

    p_install = sub.add_parser(
        "install", help="Register hooks in Claude Code's settings.json")
    p_install.add_argument("--config-dir", default=None, help=CONFIG_DIR_HELP)
    p_install.add_argument(
        "--service", action="store_true",
        help="Install a user-level service so the daemon auto-starts on login "
             "(macOS: launchd agent; Linux: systemd user unit) instead of registering hooks",
    )
    p_install.add_argument(
        "--serial-port",
        default=os.environ.get("EVE_SERIAL_PORT") or None,
        help="With --service: bake this USB serial port (glob ok, e.g. '/dev/cu.usbmodem*') "
             "into the service so the daemon uses serial instead of BLE.",
    )
    p_install.add_argument(
        "--notes-widget", action="store_true",
        help="macOS: also install a second launchd agent that shows the robot's idle-explorer "
             "notes as a desktop widget at login (com.github.eve-bridge.notes-widget)",
    )
    p_uninstall = sub.add_parser(
        "uninstall", help="Remove eve-bridge hooks from Claude Code's settings.json")
    p_uninstall.add_argument("--config-dir", default=None, help=CONFIG_DIR_HELP)
    p_uninstall.add_argument(
        "--service", action="store_true",
        help="Remove the user-level service (launchd agent / systemd unit) instead of removing hooks",
    )
    p_uninstall.add_argument(
        "--notes-widget", action="store_true",
        help="macOS: remove the notes-widget launchd agent",
    )
    p_status = sub.add_parser("status", help="Show install status")
    p_status.add_argument("--config-dir", default=None, help=CONFIG_DIR_HELP)

    p_celebrate = sub.add_parser(
        "celebrate",
        help="Make the pet celebrate — useful to signal a green CI run or a finished job",
    )
    p_celebrate.add_argument("--secs", type=float, default=5.0,
                             help="How long to celebrate (default 5)")
    p_celebrate.add_argument("--socket", default=None, help="IPC path or host:port override")

    p_species = sub.add_parser(
        "species",
        help="Set the pet's character (replaces the retired on-device menu)",
    )
    p_species.add_argument(
        "name",
        help="Species name (e.g. capybara, bongo, cat) or index; 'gif' selects "
             "an installed GIF character pack",
    )
    p_species.add_argument("--socket", default=None, help="IPC path or host:port override")

    p_widget = sub.add_parser(
        "notes-widget",
        help="macOS: show the robot's idle-explorer notes in a desktop widget (foreground)",
    )
    p_widget.add_argument("--once", action="store_true",
                          help="Build the window, print what it rendered, and exit (smoke test)")

    sub.add_parser(
        "key-check",
        help="Diagnose swipe-to-key (Accessibility grant, signing, pyobjc)",
    )
    p_ears = sub.add_parser(
        "ears-check",
        help="Diagnose the \"hey eve\" wake word (mic, model, then listen for the phrase)",
    )
    p_ears.add_argument("--seconds", type=float, default=8.0, help="How long to listen (default 8)")

    p_diag = sub.add_parser(
        "diag",
        help="Ask the board why it last reset and what it was doing (crash/hang report)",
    )
    p_diag.add_argument("--socket", default=None, help="IPC path or host:port override")
    p_diag.add_argument("--watch", action="store_true",
                        help="Poll every 2s — leave running to catch the next freeze")

    p_hud = sub.add_parser(
        "hud",
        help="Print a one-line stick status summary (stdout; designed for Claude Code's statusLine)",
    )
    p_hud.add_argument("--ascii", action="store_true", help="ASCII-only output (no emoji)")
    p_hud.add_argument("--socket", default=None, help="IPC path or host:port override")

    sub.add_parser(
        "unpair",
        help="Clear the stick's stored BLE bond (you must also Forget on the macOS side afterwards)",
    )

    p_vision = sub.add_parser(
        "vision-test",
        help="Run the host face detector on an image file and print the face cmd it would send",
    )
    p_vision.add_argument("image", help="Path to a JPEG/PNG (anything macOS ImageIO decodes)")

    p_scene = sub.add_parser(
        "scene-test",
        help="Describe one image with the model eve's voice sees through (one real API call)",
    )
    p_scene.add_argument("image", help="Path to a JPEG or PNG")
    p_scene.add_argument("--find", default=None, help="Also look for this thing in the image")

    p_intent = sub.add_parser(
        "intent-test",
        help="Check the goodbye / mute classifier on phrases (one real API call each)",
    )
    p_intent.add_argument("--expect", nargs=2, action="append", required=True, metavar=("LABEL", "TEXT"),
                          help="leave | mute | unmute | look | stay, then a phrase; repeat for more")

    # "notes" is already the diary's own listing (what eve SAW). This records
    # the room, so it takes the name the owner says out loud.
    p_notes = sub.add_parser(
        "take-notes",
        help="Take notes on the room: eve transcribes what is said until you stop it",
    )
    p_notes.add_argument("action", choices=("start", "stop", "status", "list"), nargs="?",
                         default="status")
    p_notes.add_argument("--socket", default=None, help="IPC path or host:port override")

    p_move = sub.add_parser(
        "move",
        help="Move the head: a named rhythm or gesture the board runs by itself",
    )
    p_move.add_argument("preset", nargs="?", default="dance",
                        help="sway, dance, nod, shake, bounce, wiggle, fig8, doubletake, "
                             "shrug, tilt, perk, droop, lean_peek, home")
    p_move.add_argument("--speed", type=float, default=1.0,
                        help="0.5-2.0; above 1 is a quicker beat, and the board narrows the swing to match")
    p_move.add_argument("--stop", action="store_true", help="stop whatever is running, where it is")
    p_move.add_argument("--keys", default=None,
                        help="a raw keyframe list instead of a preset, e.g. "
                             "'[[0,127,35,400],[900,127,45,400]]' (127 leaves an axis alone)")
    p_move.add_argument("--yaw-amp", type=int, default=None)
    p_move.add_argument("--pitch-amp", type=int, default=None)
    p_move.add_argument("--period-ms", type=int, default=None)
    p_move.add_argument("--cycles", type=int, default=None)
    p_move.add_argument("--socket", default=None, help="IPC path or host:port override")

    p_headcal = sub.add_parser(
        "head-cal",
        help="Open the head calibration page: tap an extreme, confirm the axis or flip it (saved on the robot)",
    )
    p_headcal.add_argument("--port", type=int, default=8766)
    p_headcal.add_argument("--no-open", action="store_true", help="do not open a browser")
    p_headcal.add_argument("--socket", default=None, help="IPC path or host:port override")

    for app in app_registry.APPS:                         # help only: main() hands these words to the app first
        if app.cli:
            p_app = sub.add_parser(app.cli[0], aliases=list(app.cli[1:]), help=app.cli_help, add_help=False)
            p_app.add_argument(f"{app.slot}_args", nargs=argparse.REMAINDER)
    p_new = sub.add_parser("app", help="Your own apps: new <name> [--dry-run] writes one (a module, its test, its doc "
                                       "and its registry line); list shows them. See docs/adding-an-app.md",
                           add_help=False)
    p_new.add_argument("app_args", nargs=argparse.REMAINDER)

    p_chrome = sub.add_parser(
        "chrome-profile",
        help="Sign in to eve's own Chrome (EVE_BROWSER_OWN): opens it with nothing connected, waits "
             "for you to quit it, then checks the sign-in. "
             "See docs/stackchan/routing.md",
    )
    p_chrome.add_argument("url", nargs="?", default="https://accounts.google.com/",
                          help="the page to open in it (default: Google sign-in)")

    p_steel = sub.add_parser(
        "steel-profile",
        help="Sign in to sites in eve's Steel browser (EVE_BROWSER_STEEL): opens a Steel session in a window "
             "on eve's Steel profile, waits for Enter, then closes it. See docs/steel.md",
    )
    p_steel.add_argument("url", nargs="?", default="https://accounts.google.com/",
                         help="the page to open in it (default: Google sign-in)")

    p_sound = sub.add_parser(
        "sound",
        help="Mute or unmute eve (the head and lights keep moving), or show which it is",
    )
    p_sound.add_argument("action", choices=("on", "off", "status"), nargs="?", default="status")
    p_sound.add_argument("--socket", default=None, help="IPC path or host:port override")

    p_mic = sub.add_parser(
        "mic",
        help="Turn the Mac microphone (the wake word) off or on for good, or show whether eve is listening",
    )
    p_mic.add_argument("action", choices=("on", "off", "status"), nargs="?", default="status")
    p_mic.add_argument("--socket", default=None, help="IPC path or host:port override")

    p_identity = sub.add_parser(
        "identity",
        help="Owner face prints: show what the robot knows, or forget it (enrol by holding Option)",
    )
    p_identity.add_argument("action", choices=("status", "reset"), nargs="?", default="status")
    p_identity.add_argument("--socket", default=None, help="IPC path or host:port override")

    p_explore = sub.add_parser(
        "explore",
        help="Send the robot off to look around the room now (or call it back / ask what it is doing)",
    )
    p_explore.add_argument("action", choices=("start", "stop", "status"), nargs="?", default="start",
                           help="start (default): explore now, ignoring the idle timer; "
                                "stop: come back; status: what the explorer is doing")
    p_explore.add_argument("--socket", default=None, help="IPC path or host:port override")

    p_notes = sub.add_parser(
        "notes",
        help="Print the idle explorer's recent notes (what the robot saw while Claude was quiet)",
    )
    p_notes.add_argument("-n", "--last", type=int, default=20,
                         help="Show the last N notes (default 20; 0 = all)")

    p_photos = sub.add_parser(
        "photos",
        help="The pictures eve kept of views it found cool: list them, or open the newest",
    )
    p_photos.add_argument("action", choices=("list", "open"), nargs="?", default="list")
    p_photos.add_argument("-n", "--last", type=int, default=10,
                          help="How many to list, newest first (default 10; 0 = all)")

    p_notes_test = sub.add_parser(
        "notes-test",
        help="Send one image to the notes model and print the sentence (one real API call)",
    )
    p_notes_test.add_argument("image", help="Path to a JPEG or PNG")

    p_push = sub.add_parser(
        "push-character",
        help="Upload a GIF character pack folder to the stick (manifest.json + *.gif)",
    )
    p_push.add_argument("path", help="Path to the character folder")

    sub.add_parser(
        "check-update",
        help="Check GitHub for a newer eve-bridge release (forces refresh)",
    )

    p_upgrade = sub.add_parser(
        "update",
        help="Pull latest release, reinstall the package, and restart the daemon",
    )
    p_upgrade.add_argument("-y", "--yes", action="store_true",
                           help="Skip the confirmation prompt")

    p_audit = sub.add_parser(
        "audit",
        help="Show the PreToolUse decision audit log (tail + filter + follow)",
    )
    p_audit.add_argument("-n", "--last", type=int, default=20, help="Show the last N entries (default 20; 0 = all)")
    p_audit.add_argument("-f", "--follow", action="store_true", help="Stream new entries as they're written")
    p_audit.add_argument("--decision", choices=["allow", "deny"], help="Filter by final decision")
    p_audit.add_argument("--source", choices=["auto_allow", "stick", "timeout", "defer", "ble_disconnected"],
                         help="Filter by decision source")
    p_audit.add_argument("--tool", help="Filter by tool name (Bash, Edit, ...)")
    p_audit.add_argument("--ascii", action="store_true", help="ASCII-only output (no colour)")
    p_audit.add_argument("--path", action="store_true", help="Print the audit log path and exit")

    p_learning = sub.add_parser("learning", help="Open the saved lessons and whiteboard")
    p_learning.add_argument("--demo", action="store_true")
    p_learning.add_argument("--port", type=int, default=48766)
    p_learning.add_argument("--data-dir")
    p_learning.add_argument("--no-open", action="store_true")
    p_learning.add_argument("--env-file", default=None,
                            help="Explicit environment file (default ~/.config/eve-bridge/env)")

    p_lesson = sub.add_parser(
        "lesson",
        help="Drive eve's lesson from the terminal (same path as the voice tool)",
        description="Drive eve's lesson on the running daemon, in any subject. "
                    "Actions: open, start, ideas, hint, check, step, status, recap, end, "
                    "listen, stop-listening.",
    )
    p_lesson.add_argument("action", choices=LESSON_ACTIONS, metavar="action",
                          help="open: show the whiteboard; start: new lesson (--mode, --topic, --level); "
                               "ideas: save your thinking (--text; empty means stuck); hint: a nudge; "
                               "check: review your work; step: exactly one next step; status: latest "
                               "feedback; recap: summary and a question; end: save and finish; "
                               "listen: eve listens while you think out loud about the open lesson; "
                               "stop-listening: eve stops listening and the microphone closes")
    p_lesson.add_argument("--mode", choices=("learn", "help"), default=None,
                          help="start: learn a topic (default) or get help with your own problem")
    p_lesson.add_argument("--topic", default=None, help="start: the topic, for example Fractions or Rust ownership")
    p_lesson.add_argument("--level", default=None, help="start: the level, free text, for example \"Grade 4\" or \"second-year physics\"")
    p_lesson.add_argument("--text", default=None, help="ideas: what you are thinking")
    p_lesson.add_argument("--socket", default=None, help="IPC path or host:port override")

    p_memory = sub.add_parser(
        "memory",
        help="eve's memory: forget, dream, reindex; or watch the memory bus (EVE_ROSBRIDGE=1)",
        description="forget: remove words from every memory store, after a preview and a yes. dream: run the "
                    "nightly dream now. reindex: rebuild the mem0 index from the transcripts. tail: print every "
                    "event on the memory bus. recall: search the owner's claude-mem for eve's memories "
                    "(EVE_CLAUDE_MEM=1). forget, dream and reindex need EVE_MEMORY=1; run dream and "
                    "reindex with the daemon stopped (the mem0 index takes one opener at a time).")
    memory_sub = p_memory.add_subparsers(dest="memory_cmd")
    p_tail = memory_sub.add_parser("tail", help="Print one line per event until Ctrl-C")
    p_tail.add_argument("--url", default="ws://127.0.0.1:9090", help="rosbridge WebSocket URL")
    p_tail.add_argument("--topic", default="/eve/*", help="Topic, or a prefix ending in * (default /eve/*)")
    p_recall = memory_sub.add_parser("recall", help="Call /eve/memory/recall and print the results")
    p_recall.add_argument("query")
    p_recall.add_argument("--limit", type=int, default=5)
    p_recall.add_argument("--url", default="ws://127.0.0.1:9090", help="rosbridge WebSocket URL")
    p_forget = memory_sub.add_parser("forget", help="Count what matches in every store, then forget it on a yes")
    p_forget.add_argument("query", help="The words to forget; a phrase in double quotes matches as written")
    p_dream = memory_sub.add_parser("dream", help="Dream the due nights now, or one day with --day")
    p_dream.add_argument("--day", default=None, help="A transcript day, YYYY-MM-DD, dreamt again even if done")
    memory_sub.add_parser("reindex", help="Rebuild the mem0 index from every transcript day")

    p_apps = sub.add_parser(
        "apps",
        help="The owner's direct apps (Gmail, Calendar, Drive, GitHub, Maps): sign in, or show what is up",
        description="login google: browser sign-in with the Desktop OAuth client in "
                    "~/.config/eve-bridge/google-oauth-client.json. GitHub and Maps read "
                    "EVE_GITHUB_MCP_TOKEN and EVE_GOOGLE_MAPS_KEY from the env file. status: each app's "
                    "sign-in, path (mcp or rest) and tool count. See docs/apps.md.")
    apps_sub = p_apps.add_subparsers(dest="apps_cmd")
    p_apps_login = apps_sub.add_parser("login", help="One-time browser sign-in for Google")
    p_apps_login.add_argument("service", choices=("google",))
    apps_sub.add_parser("status", help="Which apps are signed in and answering")

    p_link = sub.add_parser(
        "link",
        help="Eve Link: the pairing link for a phone, without Telegram",
        description="pair: print the link that pairs Eve Link with this Mac (eve's current address, read from "
                    "the rendezvous, plus the link token in the fragment). Open it on the iPhone. The link holds "
                    "the token: anyone with it can use eve, so send it only to your own phone.")
    link_sub = p_link.add_subparsers(dest="link_cmd")
    link_sub.add_parser("pair", help="Print the pairing link (and a QR code when qrencode is installed)")

    p_notify = sub.add_parser(
        "notify",
        help="Notifications on the phone through ntfy: the topic to subscribe to, or one test notification",
        description="topic: the ntfy topic (derived from Eve Link's link token) and how to subscribe on the "
                    "iPhone. test: post one notification on it. See docs/notifications.md.")
    notify_sub = p_notify.add_subparsers(dest="notify_cmd")
    notify_sub.add_parser("topic", help="Print the ntfy topic and how to subscribe")
    notify_sub.add_parser("test", help="Post one test notification")

    p_places = sub.add_parser(
        "places",
        help="The places history: a day's digest, the week, habits, or forget it all",
        description="day [YYYY-MM-DD]: that day's places and times (today by default). week [YYYY-MM-DD]: the week "
                    "holding it. habits: the usual places, days and times. summary: what is kept. forget: wipe the "
                    "history, the places, the things and the latest fix, after a yes. See docs/places.md.")
    p_places.add_argument("places_cmd", choices=("day", "week", "habits", "summary", "forget"))
    p_places.add_argument("day", nargs="?", default="")
    p_places.add_argument("--yes", action="store_true", help="forget without asking")

    p_refresh = sub.add_parser(
        "refresh",
        help="The weekly refresh reminder: when Eve Link's signing and the Google sign-in lapse",
        description="status: print both dates and the steps. remind: post them on the phone and the Mac. "
                    "schedule / unschedule: run remind every Friday at 09:00. See docs/refresh.md.")
    refresh_sub = p_refresh.add_subparsers(dest="refresh_cmd")
    for name, text in (("status", "Print when each lapses and what to run"),
                       ("remind", "Post the reminder on the phone (ntfy) and the Mac"),
                       ("schedule", "Run remind every Friday at 09:00 (a LaunchAgent)"),
                       ("unschedule", "Stop the weekly reminder")):
        refresh_sub.add_parser(name, help=text)

    sub.add_parser(
        "telegram-check",
        help="Text door: check EVE_TELEGRAM_TOKEN and print the numeric id of whoever messaged the bot",
        description="Run with the daemon's text door off (one poller per bot). Prints ids and first names "
                    "only, never message text. See docs/stackchan/telegram.md.")

    args = parser.parse_args(argv)
    if args.cmd is None:
        parser.print_help()
        return 1

    if args.cmd == "codex-home":
        from .codex_auth import SetupError, setup
        from .codex_computer import executable
        try:
            home = setup(binary=executable())
        except (SetupError, OSError, ValueError) as exc:
            # Never echo config/subprocess exceptions: they may contain secrets.
            print(str(exc) if isinstance(exc, SetupError) else "Could not prepare Eve's Codex home.", file=sys.stderr)
            return 1
        print(f"Eve Codex API home ready: {home}")
        print("Your personal Codex sign-in is unchanged. Enable with EVE_CODEX_AUTH=api.")
        return 0

    if args.cmd == "learning":
        from .learning.server import main as learning_main
        options = ["--port", str(args.port)]
        if args.demo:
            options.append("--demo")
        if args.no_open:
            options.append("--no-open")
        if args.data_dir:
            options.extend(["--data-dir", args.data_dir])
        if args.env_file:
            options.extend(["--env-file", args.env_file])
        return learning_main(options)
    if args.cmd == "lesson":
        return _run_lesson(args)
    if args.cmd == "memory":
        if args.memory_cmd is None:
            p_memory.print_help()
            return 1
        if args.memory_cmd in ("forget", "dream", "reindex"):
            return _run_memory_store(args)
        return _run_memory(args)
    if args.cmd == "apps":
        if args.apps_cmd is None:
            p_apps.print_help()
            return 1
        return _run_apps(args)
    if args.cmd == "link":
        if args.link_cmd is None:
            p_link.print_help()
            return 1
        return _run_link(args)
    if args.cmd == "notify":
        if args.notify_cmd is None:
            p_notify.print_help()
            return 1
        return _run_notify(args)
    if args.cmd == "places":
        return _places(args)
    if args.cmd == "refresh":
        if args.refresh_cmd is None:
            p_refresh.print_help()
            return 1
        return _run_refresh(args)
    if args.cmd == "daemon":
        return _run_daemon(args)
    if args.cmd == "install":
        # --notes-widget is additive: on its own it installs only the widget
        # unit; combined with --service it installs both. Without it the
        # install path is exactly what it was.
        if getattr(args, "notes_widget", False):
            from .service import install_notes_widget
            rc = install_notes_widget()
            if rc or not getattr(args, "service", False):
                return rc
        if getattr(args, "service", False):
            from .service import install_service
            return install_service(
                serial_port=getattr(args, "serial_port", None),
            )
        from .installer import install_hooks
        return install_hooks(config_dir=getattr(args, "config_dir", None))
    if args.cmd == "uninstall":
        if getattr(args, "notes_widget", False):
            from .service import uninstall_notes_widget
            rc = uninstall_notes_widget()
            if rc or not getattr(args, "service", False):
                return rc
        if getattr(args, "service", False):
            from .service import uninstall_service
            return uninstall_service()
        from .installer import uninstall_hooks
        return uninstall_hooks(config_dir=getattr(args, "config_dir", None))
    if args.cmd == "notes-widget":
        logging.basicConfig(level=logging.INFO,
                            format="%(asctime)s %(levelname)s %(name)s: %(message)s")
        from .notes_widget import run as widget_run
        return widget_run(once=args.once)
    if args.cmd == "steel-profile":
        return _run_steel_profile(args.url)
    if args.cmd == "chrome-profile":
        return _run_chrome_profile(args.url)
    if args.cmd == "status":
        from .installer import show_status
        return show_status(config_dir=getattr(args, "config_dir", None))
    if args.cmd == "celebrate":
        from .hooks._client import post
        resp = post({"evt": "celebrate", "secs": args.secs},
                    socket_path=args.socket, timeout=3.0)
        if resp is None:
            print("eve-bridge: daemon not reachable", file=sys.stderr)
            return 2
        if not resp.get("connected"):
            print("board not connected — nothing to celebrate on", file=sys.stderr)
            return 2
        print(f"pet celebrating for {args.secs:.0f}s")
        return 0
    if args.cmd == "species":
        return _run_species(args.name, args.socket)
    if args.cmd == "diag":
        return _run_diag(args.socket, args.watch)
    if args.cmd == "key-check":
        from .key_tap import KeyTapper
        return KeyTapper().diagnose()
    if args.cmd == "telegram-check":
        from .telegram import diagnose as telegram_diagnose
        return telegram_diagnose()
    if args.cmd == "ears-check":
        from .ears import diagnose as ears_diagnose
        return ears_diagnose(args.seconds)
    if args.cmd == "hud":
        from .hud import run as hud_run
        return hud_run(ascii_only=args.ascii, socket_path=args.socket)
    if args.cmd == "unpair":
        return _run_unpair()
    if args.cmd == "vision-test":
        from .vision import run_vision_test
        return run_vision_test(args.image)
    if args.cmd == "scene-test":
        from .scene import run_scene_test
        return run_scene_test(args.image, find=args.find)
    if args.cmd == "intent-test":
        pairs = [(label, text) for label, text in args.expect]
        bad = [label for label, _ in pairs if label not in ("leave", "mute", "unmute", "look", "stay")]
        if bad:
            print(f"intent-test: unknown label(s) {bad}; use leave, mute, unmute, look or stay", file=sys.stderr)
            return 2
        from .intent import run_intent_test
        return run_intent_test(pairs)
    if args.cmd == "take-notes":
        return _run_notes(args)
    if args.cmd == "move":
        return _run_move(args)
    if args.cmd == "head-cal":
        from . import head_cal
        return head_cal.serve(args.port, args.socket, open_browser=not args.no_open)
    if args.cmd == "sound":
        return _run_sound(args.action, args.socket)
    if args.cmd == "mic":
        return _run_mic(args.action, args.socket)
    if args.cmd == "identity":
        from .identity import run_identity
        return run_identity(args.action, args.socket)

    if args.cmd == "explore":
        return _run_explore(args.action, args.socket)
    if args.cmd == "notes":
        from .explore import run_notes_cli
        return run_notes_cli(args.last)
    if args.cmd == "photos":
        return _run_photos(args.action, args.last)
    if args.cmd == "notes-test":
        from .explore import run_notes_test
        return run_notes_test(args.image)
    if args.cmd == "push-character":
        return _run_push_character(args.path)
    if args.cmd == "audit":
        from .audit import default_path, render
        if args.path:
            print(default_path())
            return 0
        return render(
            last=args.last,
            decision=args.decision,
            source=args.source,
            tool=args.tool,
            ascii_only=args.ascii,
            follow=args.follow,
        )
    if args.cmd == "check-update":
        from .version_check import check, render_cli
        info = check(force=True)
        print(render_cli(info))
        return 1 if info.has_update else 0
    if args.cmd == "update":
        from .update import run_update
        return run_update(yes=args.yes)

    return 1


def _run_daemon(args: argparse.Namespace) -> int:
    # Here, not at the top: daemon.py brings BLE (bleak) and the rest of full eve's Mac with it, which no other command
    # needs, and which eve lite's command line never loads (its daemon is lite.py).
    from .daemon import Daemon

    logging.basicConfig(
        level=getattr(logging, args.log_level.upper(), logging.INFO),
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    # Refuse to start if another daemon is already listening on this IPC
    # address. A stale Unix socket is safe to remove and proceed.
    try:
        transport = make_transport(args.socket)
    except ValueError as e:
        print(f"eve-bridge: invalid IPC address: {e}", file=sys.stderr)
        return 2
    if transport.is_in_use():
        print(
            f"eve-bridge: another daemon is already listening at {transport.address}.\n"
            f"  Stop it first, or pass --socket to use a different path.",
            file=sys.stderr,
        )
        return 2

    daemon = Daemon(
        socket_path=args.socket,
        device_name_prefix=args.device_name,
        device_address=args.device_address,
        serial_port=args.serial_port,
        save_frames=args.save_frames,
    )

    loop = asyncio.new_event_loop()
    asyncio.set_event_loop(loop)

    def _sigterm(*_: object) -> None:
        asyncio.ensure_future(daemon.shutdown(), loop=loop)

    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            loop.add_signal_handler(sig, _sigterm)
        except NotImplementedError:
            pass

    # A stalled loop cannot log why it stalled; this thread does it from outside
    # (loop_watchdog.py), and SIGUSR1 dumps every thread for the cases it cannot see.
    from .loop_watchdog import LoopWatchdog, register_dump_signal

    watchdog = LoopWatchdog(loop).start()
    register_dump_signal()

    try:
        loop.run_until_complete(daemon.run())
    except KeyboardInterrupt:
        pass
    finally:
        watchdog.stop()
        loop.close()
    return 0


def _run_push_character(path: str) -> int:
    from .hooks._client import post

    # Pushing a full 1.8 MB pack at BLE speeds can take 1-2 minutes with the
    # per-chunk ack requirement. Give the IPC call plenty of headroom.
    resp = post({"evt": "push_character", "path": path}, timeout=600.0)
    if resp is None:
        print(
            "eve-bridge: daemon not reachable. Start it first.",
            file=sys.stderr,
        )
        return 2
    if not resp.get("ok"):
        print(f"push failed: {resp.get('error', 'unknown')}", file=sys.stderr)
        return 2

    name = resp.get("name", "?")
    files = resp.get("files", 0)
    size = resp.get("total_bytes", 0)
    print(f"pushed '{name}': {files} files, {size:,} bytes")
    print("the stick has switched to the new character.")
    return 0


def _render_diag(d: dict | None, connected: bool) -> None:
    """Print a board diag report. Abnormal resets get the loud treatment —
    that line is the answer to 'why did it freeze'."""
    print(f"board connected: {connected}")
    if not d:
        print("no diag report yet — the board sends one at boot; if it just\n"
              "reconnected, run this again in a couple of seconds.")
        return
    reset = d.get("reset", "?")
    abnormal = reset in ("PANIC", "TASK-WATCHDOG", "INT-WATCHDOG", "BROWNOUT",
                         "other-watchdog")
    mark = "  <-- ABNORMAL" if abnormal else ""
    print(f"boot #{d.get('boot')}   last reset: {reset}{mark}")
    print(f"uptime {d.get('up')}s   heap {d.get('heap')} (min {d.get('minheap')})"
          f"   psram {d.get('psram')}")
    if d.get("diedIn"):
        # The single most useful line on a hang: the call that never returned.
        print(f"\nDIED IN: {d['diedIn']}   (entered {d.get('diedAtMs')}ms, "
              f"{d.get('loops')} loops)")
    last = d.get("last") or []
    if not last:
        print("no surviving event ring (clean power-on, or first boot on this build)")
        return
    print("\nwhat it was doing before that reset (oldest first):")
    for ev in last:
        ms, _, text = str(ev).partition(":")
        try:
            stamp = f"{int(ms) / 1000:8.2f}s"
        except ValueError:
            stamp = ms
        print(f"  {stamp}  {text}")


# Species table order, mirroring firmware src/eve.cpp SPECIES_TABLE. The
# saved index is positional, so this list must move with that one.
SPECIES = [
    "capybara", "duck", "goose", "blob", "cat", "dragon", "octopus", "owl",
    "penguin", "turtle", "snail", "ghost", "axolotl", "cactus", "robot",
    "rabbit", "mushroom", "chonk",
]


def _run_photos(action: str, last: int) -> int:
    """``eve-bridge photos [list|open]``: the shelf, straight off disk."""
    import subprocess

    from .explore import configured as explore_configured
    from .photos import format_usage, iter_recent

    notes_dir = explore_configured().notes_dir
    recent = list(iter_recent(notes_dir, last if action == "list" else 1))
    if not recent:
        print(f"no photos yet in {notes_dir / 'photos'}")
        return 0
    if action == "open":
        newest = recent[0]
        if sys.platform != "darwin":
            print(newest)
            return 0
        subprocess.run(["open", str(newest)], check=False)
        print(f"opened {newest}")
        return 0
    for path in recent:
        try:
            size = path.stat().st_size
        except OSError:
            size = 0
        print(f"{path.parent.name} {path.stem.split('-')[0]}  {size / 1024:6.0f} KB  {path}")
    print(f"({format_usage(notes_dir)})")
    return 0


def _run_notes(args: Any) -> int:
    """``eve-bridge notes start|stop|status|list``.

    `stop` here is the one with no latency: every other way of stopping has to
    wait for a segment to come back from the transcriber.
    """
    from .hooks._client import post
    from .notes import recent
    from .recall import configured as recall_configured

    if args.action == "list":
        files = recent(recall_configured())
        if not files:
            print("no notes yet — say \u201cstart taking notes\u201d, or run `notes start`")
            return 0
        for f in files:
            first = ""
            try:
                for line in f.read_text(encoding="utf-8").splitlines():
                    if line.startswith("# "):
                        first = line[2:].strip()
                        break
            except OSError:
                pass
            print(f"{f.parent.name} {f.stem[:4]}  {first or f.stem}")
            print(f"    {f}")
        return 0

    resp = post({"evt": "notes", "action": args.action}, socket_path=args.socket, timeout=180.0)
    if resp is None:
        print("eve-bridge: daemon not reachable", file=sys.stderr)
        return 2
    if not resp.get("ok"):
        print(f"eve-bridge: {resp.get('error') or 'that did not work'}", file=sys.stderr)
        return 1
    if args.action == "start":
        if resp.get("already"):
            print(f"already taking notes, {resp.get('minutes', 0)} min in")
        else:
            print("taking notes — say \u201cstop taking notes\u201d, tap the robot, or run `notes stop`")
            print("eve cannot hear its own name while it does this")
        return 0
    if args.action == "stop":
        path = resp.get("path")
        print(f"stopped after {resp.get('minutes', 0)} min, {resp.get('words', 0)} words")
        print(path if path else "nothing was said, so nothing was written")
        return 0
    if resp.get("active"):
        print(f"taking notes: {resp.get('minutes', 0)} min, {resp.get('segments', 0)} segments, "
              f"{resp.get('words', 0)} words")
        last = (resp.get("last") or "").strip()
        if last:
            print(f"  last heard: …{last[-100:]}")
    else:
        print("not taking notes")
    return 0


def _run_move(args: Any) -> int:
    """``eve-bridge move [preset]``: the bench path, needing no conversation.

    There was no way to move the head without talking to the robot, which made
    every motion change cost a wake word and a model round trip to look at.
    """
    import json as _json

    from . import motion as motion_mod
    from .hooks._client import post

    req: dict[str, Any] = {"evt": "move"}
    if args.stop:
        req["kind"] = "stop"
    elif args.keys:
        try:
            req["kind"], req["keys"] = "keys", _json.loads(args.keys)
        except ValueError as e:
            print(f"eve-bridge: --keys is not JSON ({e})", file=sys.stderr)
            return 2
    elif args.preset in motion_mod.KEYS:
        req["kind"], req["preset"] = "keys", args.preset
    else:
        req["kind"], req["preset"], req["speed"] = "osc", args.preset, args.speed
        for flag in ("yaw_amp", "pitch_amp", "period_ms", "cycles"):
            value = getattr(args, flag, None)
            if value is not None:
                req[flag] = value

    resp = post(req, socket_path=args.socket, timeout=10.0)
    if resp is None:
        print("eve-bridge: daemon not reachable", file=sys.stderr)
        return 2
    if not resp.get("ok"):
        print(f"eve-bridge: {resp.get('error') or 'the board refused it'}", file=sys.stderr)
        return 1
    if req["kind"] == "stop":
        print("stopped")
        return 0
    asked = resp.get("asked") or {}
    print(f"asked for {motion_mod.describe(resp.get('sent') or {})}")
    print("the board admits it against its own limits, so the delivered swing may be smaller")
    if asked.get("ms"):
        print(f"about {asked['ms'] / 1000:.1f}s of motion")
    return 0


def describe_mic(resp: dict) -> str:
    """One line for `eve-bridge mic` and the menu-bar app: what the mic is doing and why."""
    if not resp.get("available", True):
        return "eve has no microphone (EVE_VOICE=0, or no input device or wake-word model)"
    if resp.get("mic") == "off":
        return "microphone off (your choice) — `eve-bridge mic on` turns it back on"
    if resp.get("listening"):
        return "microphone on: listening for the wake word" + (" (EVE_MIC_ALWAYS=1)" if resp.get("always") else "")
    if not resp.get("connected"):
        return "microphone closed: it opens when the robot connects"
    return "microphone closed: it could not be opened (see the daemon log)"


def _run_mic(action: str, socket_path: Optional[str]) -> int:
    """``eve-bridge mic [on|off|status]``: the owner's switch, and what the mic is doing now."""
    from .hooks._client import post

    resp = post({"evt": "mic", "action": action}, socket_path=socket_path, timeout=3.0)
    if resp is None:
        print("eve-bridge: daemon not reachable", file=sys.stderr)
        return 2
    print(describe_mic(resp))
    return 0


def _run_sound(action: str, socket_path: Optional[str]) -> int:
    """``eve-bridge sound [on|off|status]``: the daemon owns the port and the choice."""
    from .hooks._client import post

    resp = post({"evt": "sound", "action": action}, socket_path=socket_path, timeout=3.0)
    if resp is None:
        print("eve-bridge: daemon not reachable", file=sys.stderr)
        return 2
    sound = resp.get("sound", "?")
    note = "" if resp.get("connected") else " (board not connected: it gets the setting when it connects)"
    print(("eve is muted — it still moves and lights up" if sound == "off" else "eve's sound is on") + note)
    return 0


def _run_explore(action: str, socket_path: Optional[str]) -> int:
    """``eve-bridge explore [start|stop|status]``: talk to the daemon."""
    from .hooks._client import post

    resp = post({"evt": "explore", "action": action}, socket_path=socket_path, timeout=3.0)
    if resp is None:
        print("eve-bridge: daemon not reachable", file=sys.stderr)
        return 2
    st = resp.get("explore") or {}
    if not resp.get("ok"):
        print(f"explore: {resp.get('error') or 'refused'}", file=sys.stderr)
        return 1
    state = st.get("state", "?")
    how = "manual" if st.get("manual") else "idle"
    if action == "start":
        print(f"eve is off exploring ({state}, {how}; explore stop brings it back)")
    elif action == "stop":
        print("eve is back" if state == "off" else f"explore: {state}")
    else:
        if state == "off":
            print("eve is not exploring" + ("" if resp.get("connected") else " (board not connected)"))
        else:
            wp = st.get("waypoint")
            where = f" at yaw={wp[0]:+d} pitch={wp[1]}" if wp else ""
            print(f"eve is {state} ({how}: {st.get('reason')}){where}; "
                  f"{st.get('cycles', 0)} cycle(s), {st.get('notes', 0)} note(s) this run")
    return 0


NOT_REACHABLE = ("eve-bridge: the memory bus is not reachable at {url}. Start the daemon with "
                 "EVE_ROSBRIDGE=1 (see docs/memory-bus.md).")


def _run_memory(args: Any) -> int:
    """``eve-bridge memory tail|recall``: a rosbridge client on the daemon's memory bus."""
    import json
    import time as time_mod

    from .memory_bus import TOPICS

    async def tail() -> int:
        from websockets.asyncio.client import connect
        topics = ([args.topic] if not args.topic.endswith("*")
                  else [t for t in TOPICS if t.startswith(args.topic[:-1])] or [args.topic])
        try:
            async with connect(args.url, open_timeout=3) as ws:
                for i, topic in enumerate(topics):
                    await ws.send(json.dumps({"op": "subscribe", "id": f"tail-{i}", "topic": topic,
                                              "type": TOPICS.get(topic, "")}))
                print(f"memory: listening on {', '.join(topics)} (Ctrl-C to stop)", file=sys.stderr)
                async for raw in ws:
                    try:
                        msg = json.loads(raw)
                    except ValueError:
                        continue
                    if msg.get("op") != "publish":
                        continue
                    body = msg.get("msg") or {}
                    stamp = time_mod.strftime("%H:%M:%S", time_mod.localtime(float(body.get("time") or time_mod.time())))
                    print(f"{stamp} {msg.get('topic')} {json.dumps(body, ensure_ascii=False)}", flush=True)
        except (OSError, asyncio.TimeoutError, ConnectionError) as e:
            print(NOT_REACHABLE.format(url=args.url) + f" ({type(e).__name__})", file=sys.stderr)
            return 2
        return 0

    async def recall() -> int:
        from websockets.asyncio.client import connect
        try:
            async with connect(args.url, open_timeout=3) as ws:
                await ws.send(json.dumps({"op": "call_service", "id": "recall-1", "service": "/eve/memory/recall",
                                          "args": {"query": args.query, "limit": args.limit}}))
                async with asyncio.timeout(20):
                    async for raw in ws:
                        msg = json.loads(raw)
                        if msg.get("op") != "service_response" or msg.get("id") != "recall-1":
                            continue
                        values = msg.get("values") or {}
                        if not msg.get("result", True) or values.get("error"):
                            print(f"recall: {values.get('error') or 'the service refused'}", file=sys.stderr)
                            return 1
                        results = values.get("results") or []
                        if not results:
                            print("recall: nothing found")
                        for r in results:
                            print(f"#{r.get('id', '?')} {r.get('time', '')} {r.get('title', '')}".rstrip())
                            if r.get("text"):
                                print(f"    {r['text']}")
                        return 0
        except (OSError, asyncio.TimeoutError, ConnectionError) as e:
            print(NOT_REACHABLE.format(url=args.url) + f" ({type(e).__name__})", file=sys.stderr)
            return 2
        print("recall: no answer from the service", file=sys.stderr)
        return 2

    try:
        return asyncio.run(tail() if args.memory_cmd == "tail" else recall())
    except KeyboardInterrupt:
        return 0


def _open_memory_store() -> Optional[tuple[Any, Any]]:
    """(RecallConfig, Memory) over the memory folder, for the offline memory commands. None (with a line on
    stderr) while memory is off or before the daemon has made the folder: the one-time move is the daemon's."""
    from . import mem0_memory, recall, transcripts
    from . import memory as memory_mod

    if not memory_mod.enabled():
        print("memory: off — set EVE_MEMORY=1 (as the daemon has it) to use this", file=sys.stderr)
        return None
    cfg = recall.configured()
    if not (cfg.store / memory_mod.MIGRATED_FILE).exists():
        print(f"memory: {cfg.store} is not set up yet — start the daemon once with EVE_MEMORY=1",
              file=sys.stderr)
        return None
    store = transcripts.Transcripts(transcripts.configured(), archive=cfg.archive_dir)
    memory = memory_mod.Memory(cfg, store, mem0_memory.shared(cfg))
    # The Telegram chat log, as the daemon keeps it: forget reaches it here too. The daemon reads the file again
    # before its next write when this process rewrote it (chat_log.ChatLog._refresh), so the forget holds.
    from . import chat_log
    try:
        memory.chat_log = chat_log.open_configured()
    except Exception as e:  # noqa: BLE001 — without it, forget names the chat log as not covered
        print(f"memory: the Telegram chat log could not be opened ({type(e).__name__})", file=sys.stderr)
    return cfg, memory


def _run_memory_store(args: Any, ask: Optional[Callable[[str], str]] = None) -> int:
    """``eve-bridge memory forget|dream|reindex``: the memory folder itself, no daemon needed.
    Prints counts, never the matched words."""
    import re
    from datetime import datetime

    opened = _open_memory_store()
    if opened is None:
        return 1
    cfg, memory = opened

    if args.memory_cmd == "forget":
        preview = memory.forget_preview(args.query)
        if not preview.get("ok"):
            print(f"forget: {preview.get('reason') or 'refused'}", file=sys.stderr)
            return 1
        total = int(preview.get("total") or 0)
        if not total:
            print("forget: nothing in memory matches")
            return 0
        layers = ", ".join(f"{k} {v}" for k, v in (preview.get("layers") or {}).items())
        print(f"forget: {total} matching lines ({layers})")
        print(f"        not covered: {', '.join(preview.get('not_covered') or [])}")
        try:
            answer = (ask or input)("Forget them everywhere, for good? [y/N] ")
        except EOFError:
            answer = ""
        if answer.strip().lower() not in ("y", "yes"):
            print("forget: nothing changed")
            return 0
        done = memory.forget_apply(str(preview.get("token") or ""))
        if not done.get("ok"):
            print(f"forget: {done.get('reason') or 'failed'}", file=sys.stderr)
            return 1
        layers = ", ".join(f"{k} {v}" for k, v in (done.get("layers") or {}).items())
        squashed = ", ".join(done.get("history_squashed") or []) or "none"
        print(f"forget: {done.get('total', 0)} lines forgotten ({layers}); history squashed: {squashed}")
        return 0

    if args.memory_cmd == "dream":
        from . import dream, records

        if args.day is not None and not re.fullmatch(r"\d{4}-\d{2}-\d{2}", args.day):
            print("dream: --day takes YYYY-MM-DD", file=sys.stderr)
            return 2
        client = records.make_client()
        if client is None:
            print("dream: no model client (OPENROUTER_API_KEY and the openai SDK are needed)", file=sys.stderr)
            return 1
        if args.day is not None and args.day >= memory.transcripts.day_of(datetime.now()):
            # due_nights holds the current day back; --day must too. Dreamt now, it is marked dreamt, and
            # tonight's dream skips everything said after this.
            print(f"dream: {args.day} is not over yet; it is dreamt the night after", file=sys.stderr)
            return 2
        dreamer = dream.Dreamer(cfg, memory.transcripts, client, memory.index)
        days = [args.day] if args.day else dreamer.due_nights(datetime.now())
        if not days:
            print("dream: no night is due")
            return 0
        failed = 0
        for day in days:
            report = asyncio.run(dreamer.dream(day))
            if report.empty:
                print(f"dream: {day} — nothing said")
            elif report.dreamt:
                print(f"dream: {day} — {dreamer._summary(report)}")
            else:
                failed += 1
                print(f"dream: {day} — not dreamt (failed: {', '.join(report.failed)})", file=sys.stderr)
        return 1 if failed else 0

    # reindex
    from .mem0_memory import INGESTED_CONVS

    index = memory.index
    if index is None:
        print("reindex: the mem0 index is off (EVE_MEM0=0, or mem0ai is not installed)", file=sys.stderr)
        return 1
    try:
        (getattr(index, "home", None) or cfg.mem0_dir).joinpath(INGESTED_CONVS).unlink()  # this store's ledger
    except FileNotFoundError:
        pass
    except OSError as e:
        print(f"reindex: could not reset the index ledger ({type(e).__name__})", file=sys.stderr)
        return 1
    days = memory.transcripts.days()
    added = sum(int(index.ingest_day(memory.transcripts, day) or 0) for day in days)
    tidied = int(index.tidy() or 0)
    if getattr(index, "_mem", None) is None:
        print("reindex: the index did not open — stop the daemon first (one opener at a time)", file=sys.stderr)
        return 1
    print(f"reindex: {len(days)} days read, {added} memories added, {tidied} duplicates removed")
    return 0


def _run_lesson(args: Any) -> int:
    """``eve-bridge lesson <action>``: the voice tool's lesson path, from a terminal."""
    from .hooks._client import post
    from .learning import TUTOR_ACTIONS

    if args.action == "ideas" and args.text is None:
        # The lesson saves ideas as given: a bare `lesson ideas` would overwrite the learner's
        # saved thinking with nothing and mark them stuck. Make "stuck" an explicit choice.
        print('lesson: ideas needs --text "..." (use --text "" to say you are stuck)', file=sys.stderr)
        return 2

    req: dict[str, Any] = {"evt": "lesson", "action": args.action}
    for key in ("mode", "topic", "level", "text"):
        value = getattr(args, key, None)
        if value is not None:
            req[key] = value
    # A tutor reply can take the model's 90 s plus a 15 s search; post() cannot tell a slow
    # reply from a daemon that is not running, so give it room and say so on failure.
    slow = args.action in TUTOR_ACTIONS
    resp = post(req, socket_path=args.socket, timeout=150.0 if slow else 15.0, connect_timeout=3.0)
    if resp is None:
        print("eve-bridge: daemon not reachable. Start it with `eve-bridge daemon` (or the "
              "launchd service). `eve-bridge learning` opens the whiteboard without the robot."
              + (" If the daemon is running, the tutor may still be answering; try `lesson status`."
                 if slow else ""),
              file=sys.stderr)
        return 2
    if not resp.get("ok"):
        print(f"lesson: {resp.get('error') or resp.get('reason') or 'that did not work'}", file=sys.stderr)
        return 1
    if args.action == "status":
        lesson = resp.get("lesson")
        if lesson:
            print(f"{lesson.get('topic')} ({lesson.get('mode')}, {lesson.get('stage')})"
                  + (f": {lesson['problem']}" if lesson.get("problem") else ""))
        else:
            print("no lesson is open")
    answer = str(resp.get("answer") or "").strip()
    if answer:
        print(answer)
    return 0


def _run_species(name: str, socket_path: str | None) -> int:
    """Set the pet's character over the serial link.

    The on-device menu that used to do this is gone: customization belongs on
    the host, where it is scriptable and does not suspend the pet's gestures.
    """
    from .hooks._client import post

    key = name.strip().lower()
    if key in ("gif", "character", "pack"):
        idx = 0xFF          # firmware sentinel: use the installed GIF pack
    elif key.isdigit():
        idx = int(key)
        if idx >= len(SPECIES):
            print(f"index {idx} out of range (0..{len(SPECIES) - 1})", file=sys.stderr)
            return 2
    elif key in SPECIES:
        idx = SPECIES.index(key)
    else:
        print(f"unknown species {name!r}\n\navailable: {', '.join(SPECIES)}\n"
              "or 'gif' for an installed character pack", file=sys.stderr)
        return 2

    resp = post({"evt": "species", "idx": idx}, socket_path=socket_path, timeout=3.0)
    if resp is None:
        print("eve-bridge: daemon not reachable "
              "(check `launchctl list | grep eve`).", file=sys.stderr)
        return 2
    if not resp.get("connected"):
        print("board not connected — the change will not apply until it is.",
              file=sys.stderr)
        return 2
    shown = "GIF pack" if idx == 0xFF else f"{SPECIES[idx]} (index {idx})"
    print(f"species set to {shown}")
    return 0


def _run_diag(socket_path: str | None, watch: bool) -> int:
    import time as _t

    from .hooks._client import post

    def ask() -> dict | None:
        return post({"evt": "diag"}, socket_path=socket_path, timeout=3.0)

    resp = ask()
    if resp is None:
        print("eve-bridge: daemon not reachable "
              "(start it, or check `launchctl list | grep eve`).",
              file=sys.stderr)
        return 2
    if not watch:
        _render_diag(resp.get("diag"), bool(resp.get("connected")))
        return 0

    # Watch mode: the point is to be running WHEN it freezes, so the
    # post-reset report is caught the moment the board comes back.
    print("watching for board resets — Ctrl-C to stop\n")
    seen: tuple | None = None
    try:
        while True:
            resp = ask()
            d = (resp or {}).get("diag")
            key = ((d or {}).get("boot"), (d or {}).get("reset"))
            if d and key != seen:
                seen = key
                print(f"--- {_t.strftime('%H:%M:%S')}")
                _render_diag(d, bool((resp or {}).get("connected")))
                print()
            _t.sleep(2.0)
    except KeyboardInterrupt:
        return 0


def _run_chrome_profile(url: str) -> int:
    """Sign in to eve's own Chrome (browser_lane.py, EVE'S OWN CHROME). It opens with no debugging port,
    because Google's sign-in may refuse a Chrome another program can drive. When the owner quits it, it starts
    again with the port and this says which Google accounts eve now sees."""
    import subprocess
    import time as _time

    from . import browser_lane

    cfg = browser_lane.configured()
    if not browser_lane.quit_own_chrome(cfg.own_dir):
        print("eve-bridge: eve's own Chrome did not quit; quit it (⌘Q in that Chrome) and run this again",
              file=sys.stderr)
        return 2
    subprocess.run(browser_lane.sign_in_command(cfg.own_dir, url), capture_output=True, text=True, timeout=15)
    print(f"eve's own Chrome is open for signing in ({cfg.own_dir}), with nothing connected to it.")
    print("Sign in to each site eve may use, then quit that Chrome (⌘Q). This waits; Ctrl-C to stop waiting.")
    try:
        _time.sleep(3)
        while browser_lane.own_chrome_running(cfg.own_dir):
            _time.sleep(1)
    except KeyboardInterrupt:
        print("\nStopped waiting. Quit eve's Chrome when you're done; the next web task starts it again.")
        return 0
    try:
        signed_in = asyncio.run(_own_chrome_signed_in(cfg))
    except browser_lane.AttachError as e:
        print(f"eve-bridge: {e}", file=sys.stderr)
        return 2
    print("Google in eve's Chrome: " + ("signed in (Gmail opened the inbox)" if signed_in
                                           else "NOT signed in (Gmail went to a sign-in page)"))
    if not cfg.own:
        print("Not in use yet: set EVE_BROWSER_OWN=1 in ~/.config/eve-bridge/env and restart the daemon.")
    return 0 if signed_in else 1


def _run_steel_profile(url: str) -> int:
    """Open eve's Steel browser in a window, on the page given, for the owner to sign in. Sign-ins are kept in
    eve's Steel profile (steel_local.DEFAULT_PROFILE); Enter closes the window and saves them."""
    from . import steel_local

    cfg = steel_local.configured()
    try:
        endpoint = steel_local.start_session(cfg, visible=True)
    except steel_local.SteelError as e:
        print(f"eve-bridge: {e}", file=sys.stderr)
        return 2
    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        steel_local.release(cfg)
        print("eve-bridge: Playwright is not installed (pip install -e '.[browser]')", file=sys.stderr)
        return 2
    try:
        with sync_playwright() as pw:
            browser = pw.chromium.connect_over_cdp(endpoint, timeout=30000)
            context = browser.contexts[0] if browser.contexts else browser.new_context()
            page = context.pages[0] if context.pages else context.new_page()
            page.goto(url if "://" in url else "https://" + url, wait_until="domcontentloaded", timeout=30000)
            print(f"eve's Steel browser is open for signing in (profile {cfg.profile}).")
            try:
                input("Sign in to each site eve may use, then press Enter here to save and close it. ")
            except (EOFError, KeyboardInterrupt):
                print()
    finally:
        steel_local.release(cfg)
    print("Saved. Web tasks use this profile when EVE_BROWSER_STEEL=1.")
    return 0


async def _own_chrome_signed_in(cfg: Any) -> bool:
    """Start eve's own Chrome with its port, open Gmail in eve's tab, and say whether it stayed signed in.
    Google's account list (ListAccounts) answers 400 to a request from outside a page, and Chrome 154's
    cookies may not work when copied out, so a real page load is the check. Leaves that Chrome running."""
    import dataclasses

    from . import browser_lane

    lane = browser_lane.BrowserLane(dataclasses.replace(cfg, own=True, attach=True))
    try:
        await lane.connect()
        await lane.open_url("https://mail.google.com/")
        return not (await lane.page_text(limit=200))["signed_out"]
    finally:
        await lane.close()


def _run_unpair() -> int:
    """Tell the running daemon to send cmd:unpair to the stick."""
    from .hooks._client import post

    resp = post({"evt": "unpair"}, timeout=2.0)
    if resp is None:
        print(
            "eve-bridge: daemon not reachable. Start it with "
            "`eve-bridge daemon` (or via the launchd agent).",
            file=sys.stderr,
        )
        return 2
    if not resp.get("ok"):
        err = resp.get("error", "unknown")
        print(f"eve-bridge: unpair failed ({err})", file=sys.stderr)
        return 2

    print("sent cmd:unpair to the stick; its stored bond is cleared.")
    print("")
    print("Next: open macOS System Settings > Bluetooth > Claude-5C66 > info")
    print("'Forget This Device' to purge the cached LTK. Then the next reconnect")
    print("will prompt for a fresh 6-digit passkey (displayed on the stick).")
    print("")
    print("Watch `tail -f ~/Library/Logs/eve-bridge.log` for the moment of truth:")
    print("  \"stick link: ENCRYPTED (was None)\"")
    return 0

def _run_link(args: argparse.Namespace) -> int:
    """``link pair``: Eve Link's pairing link without Telegram's /stick (Telegram is app-only since 2026-10-08)."""
    import shutil
    import subprocess

    from . import notify, stick_link

    token = stick_link.LinkToken(notify.token_path()).ensure()
    base = stick_link.rendezvous_base() or stick_link.RENDEZVOUS_URL
    address = stick_link.latest_address(token, base)
    if not address:
        print("eve's address isn't posted yet. Is the daemon running with the Mini App's tunnel up? "
              "(It posts within a minute of starting.)")
        return 1
    link = stick_link.pair_url(address, token)
    print("Open this on your iPhone (AirDrop it to yourself, or scan the QR code), then tap Open in Eve Link:")
    print(link)
    qr = shutil.which("qrencode")
    if qr:
        subprocess.run([qr, "-t", "ansiutf8", link], check=False)
    else:
        print("(For a QR code here: brew install qrencode)")
    print("The link holds the token: send it only to your own phone.")
    return 0


def _run_notify(args: argparse.Namespace) -> int:
    """``notify topic`` and ``notify test`` (notify.py). Prints the topic, never the link token."""
    from . import notify

    cfg = notify.configured()
    token = notify.read_token()
    if not token:
        print("Eve Link is not paired yet, so there is no topic. Pair the app first (docs/action-button.md).")
        return 1
    if args.notify_cmd == "topic":
        for line in notify.setup_lines(token, cfg.base):
            print(line)
        if not cfg.enabled:
            print("Notifications are off (EVE_NOTIFY=0).")
        return 0
    if notify.post_once(token, notify.TEST_LINE, cfg.base):
        print("Posted a test notification. If the phone is subscribed, it shows now.")
        return 0
    print("Could not post the test notification (see the log line above).")
    return 1


def _run_refresh(args: argparse.Namespace) -> int:
    """``refresh status|remind|schedule|unschedule`` (refresh.py)."""
    from . import apps_mcp, refresh

    if args.refresh_cmd == "schedule":
        return refresh.schedule()
    if args.refresh_cmd == "unschedule":
        return refresh.unschedule()
    text = refresh.current(apps_mcp.configured().google_token_path)
    print(text)
    if args.refresh_cmd == "remind" and not refresh.remind(text):
        print("Shown on the Mac; the phone notification did not go out (notifications off or not paired).")
    return 0


def _run_apps(args: argparse.Namespace) -> int:
    """``apps login google`` and ``apps status`` (apps_mcp.py). Prints no token, ever."""
    import webbrowser

    from . import apps_mcp

    cfg = apps_mcp.configured()
    if args.apps_cmd == "login":
        try:
            print("Signed in to Google. Scopes:", apps_mcp.google_login(cfg, webbrowser.open))
        except Exception as e:  # noqa: BLE001 — the reason, for the owner at the terminal
            print(f"Sign-in failed: {e}")
            return 1
        print("Restart the daemon to use it (launchctl kickstart -k gui/$(id -u)/com.github.eve-bridge.daemon).")
        return 0
    bridge = apps_mcp.AppsBridge(cfg)
    signed = set(bridge.available())
    bridge.start()
    counts: dict[str, int] = {}
    for t in bridge.tools():
        app = apps_mcp.split_name(t["name"])[0]
        counts[app] = counts.get(app, 0) + 1
    print(f"EVE_APPS={'on' if cfg.enabled else 'off'}")
    for app, server in apps_mcp.SERVERS.items():
        if app in bridge.paths:
            state = f"up via {bridge.paths[app]}, {counts.get(app, 0)} tools, {bridge.policy.get(app)}"
        elif app in signed:
            state = "signed in, but not answering (see the log above)"
        else:
            state = "not signed in"
        print(f"  {server.label:16} {state}")
    return 0


# ---- eve lite (EVE_PROFILE=lite) ------------------------------------------------------------------------------------
#
# eve lite (docs/plans/eve-lite.md, profile.py) is eve for people other than the owner: one install line, two browser
# sign-ins and a web app on the Wi-Fi. Under EVE_PROFILE=lite, main() hands the command line to _lite_main, which has
# eve lite's commands and no others: its daemon (lite.py), its login service (_service_launchd's lite unit), its web
# app's sign-in and devices (`web`), a check of the whole install (`doctor`), its uninstall, and `memory forget`. Full
# eve's commands (the robot, BLE, the Mac's lanes, Claude Code's hooks, Telegram, the iPhone app, self-update through
# git) cannot be reached from it, and full eve's own command line is as it was.

EX_CONFIG = 78                    # sysexits.h: EVE_PROFILE is neither full nor lite (profile.ProfileError)
LITE_INSTALL_LINE = "curl -fsSL https://github.com/gurul/eve_01/releases/latest/download/install-lite.sh | bash"
LITE_KEPT = ".eve-lite-kept"      # install-lite.sh's marker: the env file's SHA-256, when an uninstall kept the folder
WEB_OPEN_WAIT_SECS = 20.0         # `web open` waits this long for eve to answer; a restart after a sign-in takes ~5 s
IPC_POLL_SECS = 0.5
OPEN = "/usr/bin/open"
OSADECOMPILE = "/usr/bin/osadecompile"
LAUNCH_SERVICES = Path("Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist")
_CS_DARWIN_USER_TEMP_DIR = 65537  # <unistd.h>: confstr's name for this user's own temporary folder (os has no name)
_BUNDLE_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9.-]{0,254}")
_OPAQUE = re.compile(r"[A-Za-z0-9_\-+/=.%]{24,}")      # a run this long is a key or a code, never a word (lite_web)
OK, INFO, FIX = "ok", "--", "!!"  # doctor's marks: fine; nothing to do (or optional); to fix

# What eve lite's commands say: one plain line each.
LITE_NOT_ANSWERING = ("eve is not answering on this Mac. `eve-bridge status` says whether its login service is "
                      "installed and loaded; its log is ~/Library/Logs/eve-lite.log.")
LITE_REFUSED = "eve did not do that. Try again."
LITE_NO_HOOKS = ("eve lite does not use Claude Code's hooks, so there is nothing to install there. "
                 "`eve-bridge install --service` installs eve lite's login service.")
LITE_NO_HOOKS_GONE = ("eve lite installs no Claude Code hooks, so there is nothing to remove there. "
                      "`eve-bridge lite uninstall` removes eve lite.")
LITE_UPDATE = ("eve-bridge: eve lite does not update itself. Run its install line again; it keeps your settings and "
               "memory:\n  " + LITE_INSTALL_LINE)
LITE_MAC_ONLY = "eve-bridge: eve lite runs on a Mac (macOS 14 or later), not on {platform!r}."
LITE_NOT_LOADED = ("eve-bridge: eve lite is not running on this Mac, and its login service is not loaded. "
                   "`eve-bridge install --service` starts it.")
LITE_NO_CODE = "eve-bridge: eve answered without a sign-in code. Try again."
LITE_NO_BROWSER = ("eve-bridge: no browser opened. Run this in a Terminal on the Mac itself, signed in at its screen, "
                   "or open Eve from Applications.")
LITE_LOGIN_UNUSED = ("eve-bridge: the browser did not sign in to eve within two minutes, so that sign-in is gone. Run "
                     "`eve-bridge web open` again, or open Eve from Applications.")


def _seconds(text: str) -> float:
    """--wait's value: seconds, from 0 to a day."""
    try:
        value = float(text)
    except ValueError:
        raise argparse.ArgumentTypeError(f"not a number of seconds: {text!r}") from None
    if not 0.0 <= value <= 86400.0:                 # NaN fails here too
        raise argparse.ArgumentTypeError(f"seconds from 0 to 86400, not {text!r}")
    return value


def _lite_parser() -> tuple[argparse.ArgumentParser, dict[str, argparse.ArgumentParser]]:
    """eve lite's commands, and the parsers of those that take a subcommand (for their help)."""
    parser = argparse.ArgumentParser(
        prog="eve-bridge",
        description="eve lite on this Mac: its daemon, its login service and its web app (docs/lite.md).")
    parser.add_argument("--version", action="version", version=f"eve-bridge {__version__} (eve lite)")
    sub = parser.add_subparsers(dest="cmd")

    p_daemon = sub.add_parser("daemon", help="Run eve lite's daemon (its login service runs this)")
    p_daemon.add_argument("--socket", default=None, help="IPC socket path override (a socket file)")
    p_daemon.add_argument("--log-level", default="INFO")

    p_install = sub.add_parser("install", help="With --service: install eve lite's login service")
    p_install.add_argument("--service", action="store_true",
                           help="Start eve lite now and at every login (launchd agent com.github.eve-bridge.lite)")
    p_uninstall = sub.add_parser("uninstall", help="With --service: remove eve lite's login service")
    p_uninstall.add_argument("--service", action="store_true", help="Stop eve lite and remove its launchd agent")
    sub.add_parser("status", help="Whether eve lite is installed, running and signed in: yes or no for each")
    p_update = sub.add_parser("update", help="Say how eve lite updates: by running its install line again")
    p_update.add_argument("-y", "--yes", action="store_true", help=argparse.SUPPRESS)

    p_web = sub.add_parser("web", help="eve's web app: open it on this Mac, and the browsers that can use it")
    web_sub = p_web.add_subparsers(dest="web_cmd")
    p_open = web_sub.add_parser("open", help="Open eve in this Mac's browser, signed in (Eve in Applications runs this)")
    p_open.add_argument("--wait", type=_seconds, default=WEB_OPEN_WAIT_SECS, metavar="SECONDS",
                        help=f"How long to wait for eve to answer (default {WEB_OPEN_WAIT_SECS:.0f})")
    web_sub.add_parser("devices", help="List the browsers that can open eve")
    p_revoke = web_sub.add_parser("revoke", help="Sign a browser out of eve: its id from `web devices`, or all")
    p_revoke.add_argument("id", metavar="ID|all")
    p_lan = web_sub.add_parser("lan", help="Turn phone access on or off: eve's door for phones on this Wi-Fi")
    p_lan.add_argument("state", choices=("on", "off"))

    sub.add_parser("doctor", help="Check eve lite on this Mac, part by part, and say what to fix")

    p_lite = sub.add_parser("lite", help="eve lite itself: uninstall")
    lite_sub = p_lite.add_subparsers(dest="lite_cmd")
    p_remove = lite_sub.add_parser("uninstall", help="Remove eve lite from this Mac; asks before deleting its memory")
    p_remove.add_argument("--dry-run", action="store_true", help="Say what it would remove, and remove nothing")

    p_memory = sub.add_parser("memory", help="eve's memory: forget")
    memory_sub = p_memory.add_subparsers(dest="memory_cmd")
    p_forget = memory_sub.add_parser("forget", help="Count what matches in every store, then forget it on a yes")
    p_forget.add_argument("query", help="The words to forget; a phrase in double quotes matches as written")
    return parser, {"web": p_web, "lite": p_lite, "memory": p_memory}


def _lite_main(argv: list[str], applied: list[str]) -> int:
    """eve lite's command line (this section's comment). ``applied``: what profile.apply changed in this process, for
    the daemon's log."""
    parser, parents = _lite_parser()
    args = parser.parse_args(argv)
    if args.cmd is None:
        parser.print_help()
        return 1
    if args.cmd == "daemon":
        from . import lite

        lite.setup_logging(args.log_level)
        _say_profile(applied)
        return lite.run_daemon(args)
    if args.cmd in ("install", "uninstall"):
        if not args.service:
            print(LITE_NO_HOOKS if args.cmd == "install" else LITE_NO_HOOKS_GONE)
            return 0
        if sys.platform != "darwin":
            print(LITE_MAC_ONLY.format(platform=sys.platform), file=sys.stderr)
            return 2
        from . import _service_launchd

        return _service_launchd.install_lite() if args.cmd == "install" else _service_launchd.uninstall_lite()
    if args.cmd == "update":
        print(LITE_UPDATE, file=sys.stderr)
        return 2
    if args.cmd == "status":
        return _lite_status()
    if args.cmd == "doctor":
        return _lite_doctor()
    if getattr(args, f"{args.cmd}_cmd") is None:    # web, lite and memory take a subcommand
        parents[args.cmd].print_help()
        return 1
    if args.cmd == "web":
        if args.web_cmd == "open":
            return _run_web_open(args.wait)
        if args.web_cmd == "devices":
            return _run_web_devices()
        if args.web_cmd == "revoke":
            return _run_web_revoke(args.id)
        return _run_web_lan(args.state == "on")
    if args.cmd == "lite":
        return _lite_uninstall(dry_run=args.dry_run)
    return _run_memory_store(args)


def _say_profile(applied: list[str]) -> None:
    """profile.apply ran before logging was set up, so its own line went nowhere: the daemon's log gets one now, with
    names and a count only. A secret it removed is named, so the log says why a key in the env file does nothing."""
    removed = [name for name in applied if profile.is_secret_name(name)
               and name not in profile.LITE_FORCED and name not in profile.LITE_DEFAULTS]
    logging.getLogger(__name__).info("profile: eve lite: %d switches set; removed %s", len(applied) - len(removed),
                                     ", ".join(removed) or "nothing")


# ---- talking to eve lite's daemon -----------------------------------------------------------------------------------

def _lite_socket() -> str:
    """eve lite's IPC socket, as its daemon takes it (lite.ipc_path: a socket file, never a TCP port). Read once per
    command: a TCP address in EVE_BRIDGE_SOCK is said once, not on every poll."""
    from . import lite

    return str(lite.ipc_path())


def _lite_ask(sock: str, event: dict[str, Any], timeout: float = 5.0) -> Optional[dict[str, Any]]:
    """One request to eve lite's daemon on its 0600 socket (lite.py's IPC); None when nothing answers."""
    from .hooks._client import post

    answer = post(event, socket_path=sock, timeout=timeout, connect_timeout=2.0)
    return answer if isinstance(answer, dict) else None


def _lite_answered(answer: Optional[dict[str, Any]]) -> bool:
    """Whether the daemon answered ``ok``; otherwise why not, on stderr."""
    if answer is None:
        print(f"eve-bridge: {LITE_NOT_ANSWERING}", file=sys.stderr)
        return False
    if answer.get("ok") is not True:
        print(f"eve-bridge: {_plain_line(answer.get('error'), LITE_REFUSED)}", file=sys.stderr)
        return False
    return True


def _shown(text: Any, limit: int = 300) -> str:
    """``text`` without control or format characters (a terminal acts on them), at most ``limit`` characters."""
    import unicodedata

    value = text if isinstance(text, str) else str(text)
    return "".join(ch for ch in value if unicodedata.category(ch)[0] != "C")[:limit]


def _plain_line(text: Any, fallback: str) -> str:
    """``text`` as one printable line; ``fallback`` when there is none, or when it holds a run that reads as a key or a
    code (lite_web's rule: nothing eve shows carries one, whatever said it)."""
    line = " ".join(_shown(text).split()) if isinstance(text, str) else ""
    return fallback if not line or _OPAQUE.search(line) else line


def _is_port(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and 0 < value < 65536


def _tilde(path: Any) -> str:
    """``path`` with this user's home written ~, for a line on the screen."""
    text, home = str(path), str(Path.home())
    return "~" + text[len(home):] if text == home or text.startswith(home.rstrip("/") + "/") else text


# ---- web open: this Mac's browser, signed in ------------------------------------------------------------------------

def _run_web_open(wait: float) -> int:
    """``eve-bridge web open`` (spec M4, the Mac's sign-in): this Mac's browser, signed in to eve. A one-time code comes
    over the 0600 IPC socket and goes into a page in this user's own temporary folder (0600, mkstemp) that sends the
    browser on to ``http://localhost:<port>/#login=<code>``; ``open`` opens that page. So the code is never printed,
    logged or in a command line. The page is deleted once the browser signed in with it, once the code ran out (two
    minutes), and on Ctrl-C, SIGTERM or SIGHUP. ``wait``: how long eve may take to answer, as it may be starting, or
    restarting into a sign-in; when nothing answers and launchd has no eve lite loaded, nothing is coming, so it does not
    wait. One line on stdout names the page's address (install-lite.sh reads its port)."""
    from . import _service_launchd as launchd
    from . import web_auth

    sock = _lite_socket()
    deadline = time.monotonic() + wait
    asked_launchd = False
    while True:
        answer = _lite_ask(sock, {"evt": "web_login"})
        if answer is not None and answer.get("ok") is True:
            break
        if answer is None and not asked_launchd:
            asked_launchd = True
            if not launchd.lite_loaded():
                print(LITE_NOT_LOADED, file=sys.stderr)
                return 1
        if time.monotonic() >= deadline:
            _lite_answered(answer)
            return 1
        time.sleep(IPC_POLL_SECS)
    issued = time.monotonic()
    code, port = answer.get("code"), answer.get("port")
    if not (isinstance(code, str) and web_auth.TOKEN.fullmatch(code) and _is_port(port)):
        print(LITE_NO_CODE, file=sys.stderr)
        return 1
    url = f"http://localhost:{port}"
    try:
        fd, page = tempfile.mkstemp(prefix="eve-open-", suffix=".html", dir=_user_tmp())
    except OSError as e:
        print(f"eve-bridge: the sign-in page could not be written ({type(e).__name__}).", file=sys.stderr)
        return 1
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(_login_page(f"{url}/#login={code}"))
        with _stop_on_signals():
            if not _open_in_browser(page):
                print(LITE_NO_BROWSER, file=sys.stderr)
                return 1
            print(f"eve is open in your browser: {url}", flush=True)
            if not _login_used(sock, code, issued + web_auth.LOGIN_SECS):
                print(LITE_LOGIN_UNUSED, file=sys.stderr)
        return 0
    finally:
        with contextlib.suppress(OSError):
            os.unlink(page)


def _login_used(sock: str, code: str, until: float) -> bool:
    """Wait until the browser signed in with ``code`` (lite.py's web_login_used), or until the monotonic ``until``."""
    while True:
        answer = _lite_ask(sock, {"evt": "web_login_used", "code": code})
        if answer is not None and answer.get("used") is True:
            return True
        if time.monotonic() >= until:
            return False
        time.sleep(IPC_POLL_SECS)


def _login_page(url: str) -> bytes:
    """A page that sends the browser on to ``url`` at once and leaves no history entry (location.replace), with a meta
    refresh where scripts are off. ``url`` was checked (a port, and a code of URL-safe characters), so it needs no
    escaping in either place."""
    return ("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">"
            "<meta name=\"referrer\" content=\"no-referrer\">"
            f"<meta http-equiv=\"refresh\" content=\"0; url={url}\">"
            "<title>Opening eve</title></head><body>"
            f"<script>location.replace({json.dumps(url)})</script>"
            "<p>Opening eve…</p></body></html>").encode("utf-8")


def _user_tmp() -> Optional[str]:
    """This user's own temporary folder: $TMPDIR, else the one macOS keeps for the user (a start from launchd or
    AppleScript may have no TMPDIR, or one that is gone, and /tmp is every account's); None, tempfile's own choice,
    where neither is a folder."""
    folder = os.environ.get("TMPDIR") or ""
    if os.path.isdir(folder):
        return folder
    if sys.platform == "darwin":
        try:
            mine = os.confstr(_CS_DARWIN_USER_TEMP_DIR) or ""
        except (OSError, ValueError):
            mine = ""
        if os.path.isdir(mine):
            return mine
    return None


@contextlib.contextmanager
def _stop_on_signals() -> Iterator[None]:
    """SIGTERM and SIGHUP (a closed Terminal) end the command through Python's own exit, so its ``finally`` runs: the
    page holding a sign-in code is deleted whatever stops ``web open``."""
    def stop(signum: int, _frame: Any) -> None:
        raise SystemExit(128 + signum)

    saved: dict[int, Any] = {}
    for sig in (signal.SIGTERM, signal.SIGHUP):
        try:
            saved[sig] = signal.signal(sig, stop)
        except (ValueError, OSError):               # not the main thread: the default stays
            pass
    try:
        yield
    finally:
        for sig, handler in saved.items():
            signal.signal(sig, handler)


def _default_browser() -> Optional[str]:
    """The bundle id of the app LaunchServices opens http links with, the default browser; None when its record names
    none (Safari, the default) or cannot be read."""
    try:
        with open(Path.home() / LAUNCH_SERVICES, "rb") as f:
            data = plistlib.load(f)
    except Exception:  # noqa: BLE001 — no record, or one that does not parse: the default browser is Safari
        return None
    handlers = data.get("LSHandlers") if isinstance(data, dict) else None
    for scheme in ("http", "https"):
        for handler in handlers if isinstance(handlers, list) else []:
            if not isinstance(handler, dict) or str(handler.get("LSHandlerURLScheme", "")).lower() != scheme:
                continue
            bundle = handler.get("LSHandlerRoleAll")
            if isinstance(bundle, str) and "." in bundle and _BUNDLE_ID.fullmatch(bundle):
                return bundle
    return None


def _open_in_browser(path: str) -> bool:
    """Open the page at ``path`` in the default browser, by the browser's bundle id: the app that opens .html files may
    be an editor, which would show the page instead of following it. Plain ``open`` when no browser is named, or that
    one does not start."""
    browser = _default_browser()
    for argv in ([OPEN, "-b", browser, path], [OPEN, path]) if browser else ([OPEN, path],):
        try:
            done = subprocess.run(argv, capture_output=True, timeout=30, check=False, stdin=subprocess.DEVNULL)
        except (OSError, subprocess.SubprocessError):
            continue
        if done.returncode == 0:
            return True
    return False


# ---- web devices, revoke, lan ---------------------------------------------------------------------------------------

def _when(stamp: Any) -> str:
    if not isinstance(stamp, (int, float)) or isinstance(stamp, bool) or stamp <= 0:
        return "?"
    try:
        return time.strftime("%Y-%m-%d %H:%M", time.localtime(stamp))
    except (OverflowError, OSError, ValueError):
        return "?"


def _run_web_devices() -> int:
    """``eve-bridge web devices``: the browsers that can open eve, oldest first, by id, kind and browser."""
    answer = _lite_ask(_lite_socket(), {"evt": "devices"})
    if not _lite_answered(answer):
        return 1
    listed = answer.get("devices")
    devices = [d for d in listed if isinstance(d, dict)] if isinstance(listed, list) else []
    if not devices:
        print("No browser can open eve yet. On this Mac, `eve-bridge web open`; for a phone, on eve's page: Settings, "
              "Phone access, Add a phone.")
        return 0
    print(f"{len(devices)} browser{'s' if len(devices) != 1 else ''} can open eve:")
    for d in devices:
        print(f"  {_shown(d.get('id'), 8):8}  {_shown(d.get('label'), 64):24}  {_shown(d.get('host'), 64)}  "
              f"paired {_when(d.get('created'))}, last seen {_when(d.get('last_seen'))}")
    print("`eve-bridge web revoke <id>` signs one out; `eve-bridge web revoke all` signs them all out.")
    return 0


def _run_web_revoke(target: str) -> int:
    """``eve-bridge web revoke <id|all>``: sign browsers out; their open links close (lite.py's revoke)."""
    from . import web_auth

    wanted = target.strip().lower()
    if not (wanted == "all" or web_auth.DEVICE_ID.fullmatch(wanted)):
        print("eve-bridge: give a browser's id from `eve-bridge web devices`, or all.", file=sys.stderr)
        return 2
    answer = _lite_ask(_lite_socket(), {"evt": "revoke", "id": wanted}, timeout=30.0)
    if not _lite_answered(answer):
        return 1
    revoked = answer.get("revoked")
    gone = [_shown(i, 8) for i in revoked if isinstance(i, str)] if isinstance(revoked, list) else []
    if gone:
        print("Signed out of eve: " + ", ".join(gone))
        return 0
    if wanted == "all":
        print("No browser was signed in; nothing changed.")
        return 0
    print(f"No browser has the id {wanted}; nothing changed. `eve-bridge web devices` lists them.")
    return 1


def _run_web_lan(on: bool) -> int:
    """``eve-bridge web lan on|off``: eve's door for phones and laptops on the Wi-Fi, remembered across restarts."""
    answer = _lite_ask(_lite_socket(), {"evt": "lan", "on": on}, timeout=30.0)
    if not _lite_answered(answer):
        return 1
    if answer.get("lan") is True:
        print("Phone access is on: a phone or laptop on this Wi-Fi can open eve once it is paired (on eve's page: "
              "Settings, Phone access, Add a phone).")
    else:
        print("Phone access is off: only this Mac's browser can open eve.")
    return 0 if (answer.get("lan") is True) == on else 1


# ---- status and doctor ----------------------------------------------------------------------------------------------

def _is_file(path: Any) -> bool:
    try:
        return Path(path).expanduser().is_file()
    except OSError:
        return False


def _lite_status() -> int:
    """``eve-bridge status`` in eve lite: yes or no for each part, and nothing besides (no key, path, port or address).
    The daemon's own answer when it runs; the sign-ins are read from this Mac's files when it does not."""
    from . import _service_launchd as launchd
    from . import onboarding, openrouter

    answer = _lite_ask(_lite_socket(), {"evt": "status"})
    live = answer is not None and answer.get("ok") is True
    st: dict[str, Any] = answer if live and answer is not None else {}

    def flag(name: str) -> bool:
        return st.get(name) is True

    rows = [
        ("login service installed", launchd.lite_installed()),
        ("login service loaded", launchd.lite_loaded()),
        ("eve answering", live),
        ("OpenRouter signed in", flag("openrouter") if live else openrouter.available()),
        ("Composio signed in", flag("composio") if live else _is_file(onboarding.CONNECT_TOKEN_FILE)),
        ("chat on", flag("chat")),
        ("voice ready", flag("voice")),
        ("phone access on", flag("lan")),
    ]
    width = max(len(name) for name, _ in rows)
    print("eve lite on this Mac:")
    for name, value in rows:
        print(f"  {name:<{width}}  {'yes' if value else 'no'}")
    return 0


def _env_file_profile(path: Path) -> str:
    """The EVE_PROFILE the env file at ``path`` sets (its last line that sets it, as envfile reads the file); "" when it
    sets none or is not there, "unreadable" when it cannot be read."""
    from .envfile import parse_env_file

    try:
        text = Path(path).expanduser().read_text(encoding="utf-8-sig")
    except FileNotFoundError:
        return ""
    except (OSError, ValueError):
        return "unreadable"
    return parse_env_file(text).get(profile.PROFILE_ENV, "")


def _sets_lite(value: str) -> bool:
    try:
        return profile.is_lite({profile.PROFILE_ENV: value})
    except profile.ProfileError:
        return False


def _loose(path: Path) -> bool:
    """Whether ``path``, when it is there, lets another account in: a group or other bit set (a private file is 0600,
    a private folder 0700)."""
    try:
        return bool(path.stat().st_mode & 0o077)
    except OSError:
        return False


def _lite_doctor() -> int:
    """``eve-bridge doctor``: eve lite on this Mac, part by part, each with what to do about it, marked ok, -- (nothing
    to do, or optional) or !! (to fix); exit 1 while anything is to fix. It prints no key, code or token: the daemon's
    status holds words, booleans, ports and counts (lite.py), and files are checked by their mode only."""
    from . import _service_launchd as launchd
    from . import envfile, lite, lite_web, onboarding, openrouter, toolchain

    rows: list[tuple[str, str]] = []

    def say(mark: str, line: str) -> None:
        rows.append((mark, line))

    # the profile: the env file is what makes `eve-bridge` in a Terminal eve lite's
    env_file = envfile.env_path()
    if _sets_lite(_env_file_profile(env_file)):
        say(OK, f"profile: {_tilde(env_file)} sets EVE_PROFILE=lite")
    else:
        say(FIX, f"profile: {_tilde(env_file)} does not set EVE_PROFILE=lite, so `eve-bridge` in a Terminal runs full "
                 "eve's commands. Run eve lite's install line again.")

    # the login service, and full eve beside it
    plist = launchd.installed_lite_plist()
    loaded = launchd.lite_loaded()
    if plist is None:
        say(FIX, "login service: not installed. `eve-bridge install --service` installs it.")
    else:
        env, argv = plist.get("EnvironmentVariables"), plist.get("ProgramArguments")
        program = argv[0] if isinstance(argv, list) and argv else None
        if not (isinstance(env, dict) and _sets_lite(str(env.get(profile.PROFILE_ENV) or ""))):
            say(FIX, "login service: it does not pin EVE_PROFILE=lite. `eve-bridge install --service` writes it "
                     "again.")
        elif not (isinstance(program, str) and os.path.isfile(program) and os.access(program, os.X_OK)):
            say(FIX, "login service: the Python it starts is gone. Run eve lite's install line again.")
        elif not loaded:
            say(FIX, "login service: installed, not loaded. `eve-bridge install --service` loads it.")
        else:
            say(OK, "login service: installed and loaded")
    if lite.full_eve_installed():
        say(FIX, f"full eve's login service is installed for this user ({lite.FULL_LABEL}), so eve lite stays down: "
                 "a Mac account has one of them.")

    # the daemon, by its own status
    answer = _lite_ask(_lite_socket(), {"evt": "status"})
    live = answer is not None and answer.get("ok") is True
    st: dict[str, Any] = answer if live and answer is not None else {}
    if not live:
        say(FIX, "eve: not answering on this Mac. Its log is ~/Library/Logs/eve-lite.log"
                 + ("" if loaded else "; `eve-bridge install --service` starts it") + ".")
    elif st.get("restarting") is True:
        say(INFO, "eve: restarting into a new sign-in; run this again in a few seconds")
    elif st.get("state") == lite.READY:
        say(OK if st.get("chat") is True else FIX,
            "eve: running and set up" if st.get("chat") is True
            else "eve: running, but its chat did not start. Its log is ~/Library/Logs/eve-lite.log.")
    else:
        say(INFO, "eve: running, waiting for the OpenRouter sign-in")
    if not ((st.get("openrouter") is True) if live else openrouter.available()):
        say(FIX, "OpenRouter: not signed in. `eve-bridge web open`, then Sign in with OpenRouter.")
    elif live and st.get("openrouter_rejected") is True:
        say(FIX, "OpenRouter: the key was turned down (it was deleted or revoked). `eve-bridge web open`, then "
                 "Sign in with OpenRouter again.")
    else:
        say(OK, "OpenRouter: signed in")
    if (st.get("composio") is True) if live else _is_file(onboarding.CONNECT_TOKEN_FILE):
        say(OK, "Composio: signed in")
    else:
        say(INFO, "Composio: not signed in (optional: on eve's page, Settings, Apps)")
    if live:
        port, lan_port = st.get("port"), st.get("lan_port")
        if st.get("page") is True and _is_port(port):
            say(OK, f"the Mac's page: http://localhost:{port}")
        else:
            say(FIX, "the Mac's page: not up. If this stays so, every port it may use is taken: restart the Mac.")
        if st.get("lan") is True:
            say(OK, "phone access: on" + (f", port {lan_port}" if _is_port(lan_port) else ""))
        else:
            say(INFO, "phone access: off (on eve's page: Settings, Phone access, Add a phone)")
        network = st.get("local_network")
        if network == "ok":
            say(OK, "Local Network: allowed")
        elif network == "blocked":
            say(FIX, "Local Network: blocked, so voice cannot reach a phone (chat still works over HTTPS). System "
                     "Settings, Privacy & Security, Local Network: turn on python3.13.")
        else:
            say(INFO, "Local Network: not known yet (eve checks it when a phone pairs)")
        if st.get("voice") is True:
            say(OK, "voice: ready")
        else:
            say(INFO, "voice: off (it needs the OpenRouter sign-in; see ~/Library/Logs/eve-lite.log)")
        refused = st.get("firewall") if isinstance(st.get("firewall"), dict) else {}
        named = [f"{_shown(str(k).split(':', 1)[-1], 64)} ({int(v)})" for k, v in sorted(refused.items())
                 if isinstance(v, int) and not isinstance(v, bool)]
        if named:
            say(INFO, "firewall: refused " + ", ".join(named) + ". eve lite starts no other program and stays out of "
                      "private folders; nothing to do unless something you use stopped working.")
        else:
            say(OK, "firewall: nothing refused")

    # the files: the page, and the private ones
    web_root = Path(str(os.environ.get(lite_web.WEB_DIR_ENV) or "").strip() or lite_web.PACKAGED_WEB_DIR).expanduser()
    if (web_root / "index.html").is_file():
        say(OK, "web page: installed")
    else:
        say(FIX, "web page: not installed. Run eve lite's install line again.")
    tls = lite.TLS_DIR.expanduser()
    private = [env_file, Path(onboarding.CONNECT_TOKEN_FILE).expanduser(), lite.DEVICES_FILE.expanduser(),
               Path(lite_web.WEB_FILE).expanduser(), tls, tls / "ca.key", tls / "leaf.key"]
    loose = [_tilde(path) for path in private if _loose(path)]
    if loose:
        say(FIX, "private files: other accounts on this Mac can read " + ", ".join(loose)
                 + ". `chmod 600` each file (700 a folder).")
    else:
        say(OK, "private files: this account's alone")
    say(INFO, f"Python {sys.version.split()[0]}: {_tilde(sys.executable)}")
    say(INFO, "Xcode's command line tools: " + ("installed" if toolchain.dev_tools_present() else "not installed")
              + " (eve lite does not use them)")

    print("eve lite on this Mac:")
    for mark, line in rows:
        print(f"  {mark}  {line}")
    fixes = sum(1 for mark, _ in rows if mark == FIX)
    print("Nothing to fix." if not fixes else f"{fixes} to fix, marked {FIX}.")
    return 1 if fixes else 0


# ---- lite uninstall ---------------------------------------------------------------------------------------------------

def _kept_by_lite(kept: Path, env_file: Path) -> bool:
    """install-lite.sh's kept_by_lite: the folder is as eve lite's uninstall kept it, that is the marker names the env
    file's SHA-256 (or none, with no env file) and the env file has not changed since."""
    import hashlib

    try:
        if not kept.is_file():
            return False
        lines = kept.read_text(encoding="utf-8").splitlines()
    except (OSError, ValueError):
        return False
    want = ""
    for line in lines:
        if line.startswith("env_sha256="):
            want = line[len("env_sha256="):]
    if os.path.lexists(env_file):
        try:
            got = hashlib.sha256(env_file.read_bytes()).hexdigest()
        except OSError:
            return False
    else:
        got = "none"
    return bool(want) and want == got


def _mark_kept(kept: Path, env_file: Path) -> None:
    """install-lite.sh's mark_kept: the marker that lets the install line take the kept folder back, while the env file
    is what this left (its SHA-256), or there is none. Written whole (0600), as the installer writes it."""
    import hashlib

    digest = hashlib.sha256(env_file.read_bytes()).hexdigest() if os.path.lexists(env_file) else "none"
    text = ("# eve lite was uninstalled and kept this folder. Its install line takes the folder back while env is\n"
            "# what the uninstall left: the file with this SHA-256, or none at all.\n"
            f"env_sha256={digest}\n")
    fd, tmp = tempfile.mkstemp(prefix=f"{LITE_KEPT}.", dir=kept.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(text)
        os.chmod(tmp, 0o600)
        os.replace(tmp, kept)
    except BaseException:
        with contextlib.suppress(OSError):
            os.unlink(tmp)
        raise


def _app_is_ours(app: Path) -> bool:
    """install-lite.sh's app_is_ours: ``app`` is the Eve.app it made, whose script runs an eve lite venv's eve-bridge
    (read with osadecompile, as the installer reads it). Any other Eve.app is someone else's, and stays."""
    if not app.is_dir():
        return False
    try:
        done = subprocess.run([OSADECOMPILE, str(app)], capture_output=True, text=True, timeout=20, check=False,
                              stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return False
    return done.returncode == 0 and "/.eve-lite/venv/bin/eve-bridge" in done.stdout


def _has_content(path: Path) -> bool:
    """Whether the file holds any line that is not blank (a comment counts); an unreadable one does, so it is kept."""
    try:
        return any(line.strip() for line in path.read_text(encoding="utf-8-sig").splitlines())
    except FileNotFoundError:
        return False
    except (OSError, ValueError):
        return True


def _remove(path: Path) -> bool:
    """Remove ``path``, a file, a link (never what it points to) or a folder; True once it is gone. A failure is said
    on stderr."""
    try:
        if path.is_symlink() or not path.is_dir():
            path.unlink(missing_ok=True)
        else:
            shutil.rmtree(path)
    except FileNotFoundError:
        pass
    except OSError as e:
        print(f"eve-bridge: {_tilde(path)} was not removed ({type(e).__name__}); delete it yourself.", file=sys.stderr)
        return False
    return True


def _yes(question: str, ask: Optional[Callable[[str], str]]) -> bool:
    """y or yes typed at the terminal; no terminal means no (as install-lite.sh asks)."""
    if ask is None:
        if sys.stdin is None or not sys.stdin.isatty():
            return False
        ask = input
    try:
        answer = ask(question)
    except EOFError:
        answer = ""
    return answer.strip().lower() in ("y", "yes")


def _lite_uninstall(*, dry_run: bool, ask: Optional[Callable[[str], str]] = None) -> int:
    """``eve-bridge lite uninstall``: what ``install-lite.sh --uninstall`` does, from eve lite itself. It stops eve lite
    and removes its login service, Eve.app (only the one the installer made), ~/.eve-lite and eve lite's log. The env
    file loses its EVE_PROFILE=lite line, so a full eve installed later never starts as eve lite. ~/.config/eve-bridge
    holds eve's memory: when it is eve lite's alone (no full eve beside it) it asks before deleting it, and keeps it
    when there is no terminal to ask; kept, it gets the installer's marker, so the install line takes it back.
    ~/.eve-lite goes last: this program runs from it. A part that cannot be removed is said, the rest still goes, and
    the exit is 1."""
    import codecs

    from . import _service_launchd as launchd
    from .envfile import remove_key

    codecs.lookup("utf-8-sig")         # every codec and module used below is loaded before ~/.eve-lite goes
    home = Path.home()
    lite_home, app = home / ".eve-lite", home / "Applications" / "Eve.app"
    env_dir = home / ".config" / "eve-bridge"
    env_file, kept = env_dir / "env", env_dir / LITE_KEPT
    plist, log = launchd.lite_plist_path(), launchd.lite_log_path()
    sets_lite = _sets_lite(_env_file_profile(env_file))
    full = launchd.agent_path(launchd.LABEL).exists()
    ours = not full and (sets_lite or _kept_by_lite(kept, env_file))
    app_ours = _app_is_ours(app)
    removed = [p for p in (plist, lite_home, log) if os.path.lexists(p)] + ([app] if app_ours else [])

    if dry_run:
        head = "eve-bridge lite uninstall, dry run. It would stop eve lite's login service if it is loaded"
        print(head + (", and remove:" if removed else "; nothing else of eve lite's is left to remove."))
        for path in removed:
            print(f"  {_tilde(path)}")
        if ours:
            print("Then it would ask before deleting ~/.config/eve-bridge, which holds eve's memory. If it is kept, "
                  + ("the folder loses only the EVE_PROFILE=lite line (the env file goes when nothing else is in it), "
                     if sets_lite else "the folder stays as an earlier uninstall left it, ")
                  + "and the install line takes it back.")
        elif sets_lite:
            print("It would also take the EVE_PROFILE=lite line out of ~/.config/eve-bridge/env, which full eve keeps.")
        print("Nothing was written.")
        return 0

    forget_all = ours and _yes("Also delete eve's memory, chats and sign-ins in ~/.config/eve-bridge? [y/N] ", ask)
    rc = 0
    try:
        launchd.uninstall_lite()                # the service first, so nothing runs from ~/.eve-lite while it goes
    except OSError as e:
        print(f"eve-bridge: eve lite's login service was not removed ({type(e).__name__}).", file=sys.stderr)
        rc = 1
    for path in ([app] if app_ours else []) + [log]:
        if not _remove(path):
            rc = 1
    if forget_all:
        if not _remove(env_dir):
            rc = 1
        final = "eve lite is uninstalled, with its memory and settings."
    else:
        try:
            if sets_lite:
                remove_key(env_file, profile.PROFILE_ENV)
                if ours and not _has_content(env_file):
                    env_file.unlink(missing_ok=True)    # it held nothing but the profile
            if full:
                kept.unlink(missing_ok=True)            # the folder is full eve's: nothing of eve lite's takes it back
            elif ours:
                _mark_kept(kept, env_file)
        except (OSError, ValueError) as e:
            print(f"eve-bridge: ~/.config/eve-bridge/env could not be changed ({type(e).__name__}): take its "
                  "EVE_PROFILE line out yourself.", file=sys.stderr)
            rc = 1
        if full:
            final = "eve lite is uninstalled. Full eve stays as it is."
        elif ours:
            final = ("eve lite is uninstalled. Its memory and settings stay in ~/.config/eve-bridge; the install "
                     "line takes them back.")
        else:
            final = "eve lite is uninstalled."
    if not _remove(lite_home):
        rc = 1
    print(final)
    return rc


if __name__ == "__main__":
    sys.exit(main())



def _places(args: argparse.Namespace) -> int:
    """``eve-bridge places``: the places history in the terminal (places.py)."""
    import json as _json

    from . import places

    if not places.enabled():
        print(places.OFF_REASON)
        return 1
    store = places.Places()
    if args.places_cmd == "forget":
        summary = store.summary()
        if not args.yes:
            answer = input(f"Forget {summary['places']} places, {summary['visits']} visits and {summary['things']} "
                           "things, and the phone's latest location? [y/N] ")
            if answer.strip().lower() not in ("y", "yes"):
                print("Nothing forgotten.")
                return 0
        print(_json.dumps(store.forget()))
        return 0
    if args.places_cmd == "summary":
        print(_json.dumps(store.summary(), indent=2))
        return 0
    what = {"day": "day", "week": "week", "habits": "habits"}[args.places_cmd]
    out = places.call(places.RECALL, {"what": what, "start_date": args.day, "end_date": ""}, store)
    print(out.get("text") or _json.dumps(out, indent=2, ensure_ascii=False))
    return 0 if out.get("ok") else 1
