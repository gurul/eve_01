"""Entry point. `eve-bridge [daemon|install|uninstall|status|notes-widget|...]`."""

from __future__ import annotations

import argparse
import asyncio
import logging
import os
import signal
import sys
from typing import Any, Callable, Optional

from . import __version__
from .daemon import Daemon
from .envfile import load_env_file
from .ipc import make_transport
from .learning import LESSON_ACTIONS


def main(argv: list[str] | None = None) -> int:
    # Secrets and knobs the service cannot inherit from a shell
    # (OPENAI_API_KEY, EVE_*) — read before any subcommand looks at
    # os.environ. Existing variables win; the file only fills gaps.
    load_env_file()
    rest = sys.argv[1:] if argv is None else list(argv)
    if rest[:1] == ["lights"]:
        # everything after "lights" is the lights CLI's own (argparse's REMAINDER drops a leading --help)
        from . import lights
        return lights.cli(rest[1:] or ["--help"])
    if rest[:1] == ["spotify"]:
        from . import spotify
        return spotify.cli(rest[1:] or ["--help"])
    if rest[:1] == ["firetv"]:
        from . import firetv
        return firetv.cli(rest[1:] or ["--help"])
    if rest[:1] in (["obsbot"], ["cam"]):
        from . import obsbot
        return obsbot.cli(rest[1:] or ["--help"])
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

    p_lights = sub.add_parser(
        "lights",
        help="The owner's lights (Govee, HappyLighting, Sylvania/Tuya): scan, list, name, set. See docs/lights.md",
        add_help=False,
    )
    p_lights.add_argument("lights_args", nargs=argparse.REMAINDER)

    p_spotify = sub.add_parser(
        "spotify",
        help="The owner's Spotify (spotKnob's powers): login, status, devices, play, pause, next, previous, "
             "volume, transfer. See docs/spotify.md",
        add_help=False,
    )
    p_spotify.add_argument("spotify_args", nargs=argparse.REMAINDER)

    p_firetv = sub.add_parser(
        "firetv",
        help="The owner's Fire TV over network ADB: pair, status, on, off, key, volume, mute, open, apps, "
             "youtube, type, screenshot, forget. See docs/firetv.md",
        add_help=False,
    )
    p_firetv.add_argument("firetv_args", nargs=argparse.REMAINDER)

    p_obsbot = sub.add_parser(
        "obsbot",
        aliases=["cam"],
        help="The OBSBOT Tiny 3 webcam through OBSBOT Center's OSC: setup, status, wake, sleep, move, nudge, "
             "center, zoom, fov, preset, track, mode. See docs/obsbot.md",
        add_help=False,
    )
    p_obsbot.add_argument("obsbot_args", nargs=argparse.REMAINDER)

    p_chrome = sub.add_parser(
        "chrome-profile",
        help="Sign in to eve's own Chrome (EVE_BROWSER_OWN): opens it with nothing connected, waits "
             "for you to quit it, then checks the sign-in. "
             "See docs/stackchan/routing.md",
    )
    p_chrome.add_argument("url", nargs="?", default="https://accounts.google.com/",
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


if __name__ == "__main__":
    sys.exit(main())

