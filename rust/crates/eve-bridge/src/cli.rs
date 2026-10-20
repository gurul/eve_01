//! Entry point. `eve-bridge [daemon|install|uninstall|status|notes-widget|...]`. Port of `cli.py`.
//!
//! Under EVE_PROFILE=lite (eve_profile) the command line is eve lite's own, smaller one ([`lite::lite_main`]):
//! `eve-bridge [daemon|install|uninstall|status|update|web|doctor|lite|memory]`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::argparse::{Arg, Exit, Namespace, Parser, Val, SUPPRESS};
use crate::io::Io;
use crate::py::{self, get, pstr, truthy};

pub mod lite;
pub mod memory;

pub use lite::{EX_CONFIG, LITE_INSTALL_LINE};
pub use memory::{open_memory_store, run_memory, run_memory_store, NOT_REACHABLE};

/// The process environment as a lookup (`os.environ.get`).
pub fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// `main(argv)`: the process's command line, or `argv`. Returns the exit code.
pub fn main(argv: Option<Vec<String>>) -> i32 {
    let rest: Vec<String> = argv.unwrap_or_else(|| std::env::args().skip(1).collect());
    if rest.first().map(String::as_str) == Some("mcp") {
        // eve's tools for a Work session (eve_mcp.py): before the env file, so the server holds none of its keys
        return eve_eve_mcp::main();
    }
    if rest.first().map(String::as_str) == Some("vault-mcp") {
        // the vault's fill tool for Codex tasks (vault_mcp.py): no env file either, it needs none of its keys
        return eve_vault_mcp::main();
    }
    // Secrets and knobs the service cannot inherit from a shell (OPENAI_API_KEY, EVE_*) — read before any
    // subcommand looks at the environment. Existing variables win; the file only fills gaps.
    if let Err(e) = eve_envfile::load_env_file(None, &mut eve_envfile::ProcessEnv) {
        eprintln!("eve-bridge: {e}");
        return 1;
    }
    // eve lite (EVE_PROFILE=lite): its switches forced, its defaults filled and every other secret removed before
    // any command reads the environment, then its own commands. Unset or "full" changes nothing here.
    let applied = match eve_profile::apply(&mut eve_profile::ProcessEnv) {
        Ok(applied) => applied,
        Err(e) => {
            eprintln!("eve-bridge: {e}");
            return EX_CONFIG;
        }
    };
    let io = Io::process();
    if eve_profile::is_lite(None).unwrap_or(false) {
        return lite::lite_main(&rest, &applied, &lite::Ctx::process(), &io);
    }
    // eve's apps' own CLIs (app_registry.APPS: lights, spotify, firetv, obsbot/cam, ...): everything after the word
    // is the app's own (argparse's REMAINDER drops a leading --help)
    if let Some(first) = rest.first() {
        for app in eve_app_registry::APPS.iter() {
            if app.cli.iter().any(|w| w == first) {
                let args: Vec<String> = if rest.len() > 1 { rest[1..].to_vec() } else { vec!["--help".into()] };
                return app_cli(&app.slot, &args);
            }
        }
    }
    if rest.first().map(String::as_str) == Some("app") {
        return eve_app_template::cli(&rest[1..]);
    }
    if rest.first().map(String::as_str) == Some("vault") {
        let args: Vec<String> = if rest.len() > 1 { rest[1..].to_vec() } else { vec!["--help".into()] };
        return eve_vault::cli_main(&args);
    }
    full_main(&rest, &io, &process_env)
}

/// `app.cli_mod().cli(argv)` for the app in `slot`.
fn app_cli(slot: &str, argv: &[String]) -> i32 {
    // argparse's --help layout, as the Python apps print it (the apps parse with clap)
    if let Some(help) = crate::apphelp::help_for(slot, argv) {
        print!("{help}");
        return 0;
    }
    match slot {
        "lights" => eve_lights::cli(argv, None, &mut |line| println!("{line}")),
        "spotify" => match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(rt) => rt.block_on(eve_spotify::cli(argv, None, &mut |line: String| println!("{line}"), None)),
            Err(e) => {
                eprintln!("eve-bridge: {e}");
                1
            }
        },
        "firetv" => eve_firetv::cli(argv, &mut |line| println!("{line}"), eve_firetv::CliOptions::default()),
        "obsbot" => eve_obsbot::main_with_args(argv),
        other => {
            eprintln!("eve-bridge: the {other} app has no command line");
            1
        }
    }
}

/// argparse's output (help, version, an error) through `io`; Err(exit status) when it stops the command.
pub fn parse(parser: &Parser, argv: &[String], io: &Io) -> Result<Namespace, i32> {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let r = parser.parse_args_to(argv, &mut out, &mut err);
    emit(&out, &io.out);
    emit(&err, &io.err);
    r.map_err(|Exit(code)| code)
}

fn emit(bytes: &[u8], say: &crate::io::Say) {
    if bytes.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(bytes);
    say(text.strip_suffix('\n').unwrap_or(&text));
}

/// `parser.print_help()` through `io`.
pub fn print_help(parser: &Parser, io: &Io) {
    let help = parser.format_help();
    io.say(help.strip_suffix('\n').unwrap_or(&help));
}

const CONFIG_DIR_HELP: &str = "Claude Code config home to operate on (default: $CLAUDE_CONFIG_DIR, else ~/.claude). \
Wrappers such as era-code run sessions against a private home — hooks installed into the wrong one never fire and \
never warn.";
const SOCKET_HELP: &str = "IPC path or host:port override";

fn sock() -> Arg {
    Arg::opt(&["--socket"]).help(SOCKET_HELP)
}

fn env_default(env: &dyn Fn(&str) -> Option<String>, name: &str) -> Val {
    match env(name) {
        Some(v) if !v.is_empty() => Val::Str(v),
        _ => Val::None,
    }
}

fn sub(prog: &str, name: &str, description: Option<&str>) -> Parser {
    Parser::new(&format!("{prog} {name}"), description, true)
}

/// Full eve's argument parser (cli.py's `main`). `env` gives the defaults read from the environment.
pub fn full_parser(env: &dyn Fn(&str) -> Option<String>) -> Parser {
    let prog = "eve-bridge";
    let mut parser = Parser::new(prog, None, true);
    parser.version(&format!("eve-bridge {}", eve_meta::VERSION));
    let mut cmds: Vec<(String, Vec<String>, Option<String>, Parser)> = Vec::new();
    let mut add = |name: &str, help: &str, p: Parser| cmds.push((name.into(), Vec::new(), Some(help.into()), p));

    add("codex-home", "Prepare Eve's separate Codex API-key home without changing your own sign-in", sub(prog, "codex-home", None));

    let mut p = sub(prog, "daemon", None);
    p.arg(Arg::opt(&["--socket"]).help("IPC path or host:port override"))
        .arg(Arg::opt(&["--device-name"]).default_str("Claude").help("BLE name prefix to match (default: Claude)"))
        .arg(Arg::opt(&["--device-address"]).help("BLE address to connect to (skips scan)"))
        .arg(Arg::opt(&["--serial-port"]).default(env_default(env, "EVE_SERIAL_PORT")).help(
            "USB serial port of the eve (e.g. /dev/cu.usbmodem*). Uses serial instead of BLE.",
        ))
        .arg(Arg::opt(&["--log-level"]).default_str("INFO"))
        .arg(
            Arg::opt(&["--save-frames"])
                .default(env_default(env, "EVE_SAVE_FRAMES"))
                .metavar(&["DIR"])
                .help("Bench debugging: write received camera frames here (at most one per second)"),
        );
    add("daemon", "Run the bridge daemon (connects to BLE device, serves hooks)", p);

    let mut p = sub(prog, "install", None);
    p.arg(Arg::opt(&["--config-dir"]).help(CONFIG_DIR_HELP))
        .arg(Arg::opt(&["--service"]).flag().help(
            "Install a user-level service so the daemon auto-starts on login (macOS: launchd agent; Linux: systemd \
             user unit) instead of registering hooks",
        ))
        .arg(Arg::opt(&["--serial-port"]).default(env_default(env, "EVE_SERIAL_PORT")).help(
            "With --service: bake this USB serial port (glob ok, e.g. '/dev/cu.usbmodem*') into the service so the \
             daemon uses serial instead of BLE.",
        ))
        .arg(Arg::opt(&["--notes-widget"]).flag().help(
            "macOS: also install a second launchd agent that shows the robot's idle-explorer notes as a desktop \
             widget at login (com.github.eve-bridge.notes-widget)",
        ));
    add("install", "Register hooks in Claude Code's settings.json", p);

    let mut p = sub(prog, "uninstall", None);
    p.arg(Arg::opt(&["--config-dir"]).help(CONFIG_DIR_HELP))
        .arg(Arg::opt(&["--service"]).flag().help(
            "Remove the user-level service (launchd agent / systemd unit) instead of removing hooks",
        ))
        .arg(Arg::opt(&["--notes-widget"]).flag().help("macOS: remove the notes-widget launchd agent"));
    add("uninstall", "Remove eve-bridge hooks from Claude Code's settings.json", p);

    let mut p = sub(prog, "status", None);
    p.arg(Arg::opt(&["--config-dir"]).help(CONFIG_DIR_HELP));
    add("status", "Show install status", p);

    let mut p = sub(prog, "celebrate", None);
    p.arg(Arg::opt(&["--secs"]).float().default(Val::Float(5.0)).help("How long to celebrate (default 5)"))
        .arg(sock());
    add("celebrate", "Make the pet celebrate — useful to signal a green CI run or a finished job", p);

    let mut p = sub(prog, "species", None);
    p.arg(Arg::pos("name").help(
        "Species name (e.g. capybara, bongo, cat) or index; 'gif' selects an installed GIF character pack",
    ))
    .arg(sock());
    add("species", "Set the pet's character (replaces the retired on-device menu)", p);

    let mut p = sub(prog, "notes-widget", None);
    p.arg(Arg::opt(&["--once"]).flag().help("Build the window, print what it rendered, and exit (smoke test)"));
    add("notes-widget", "macOS: show the robot's idle-explorer notes in a desktop widget (foreground)", p);

    add("key-check", "Diagnose swipe-to-key (Accessibility grant, signing, pyobjc)", sub(prog, "key-check", None));

    let mut p = sub(prog, "ears-check", None);
    p.arg(Arg::opt(&["--seconds"]).float().default(Val::Float(8.0)).help("How long to listen (default 8)"));
    add("ears-check", "Diagnose the \"hey eve\" wake word (mic, model, then listen for the phrase)", p);

    let mut p = sub(prog, "diag", None);
    p.arg(sock()).arg(Arg::opt(&["--watch"]).flag().help("Poll every 2s — leave running to catch the next freeze"));
    add("diag", "Ask the board why it last reset and what it was doing (crash/hang report)", p);

    let mut p = sub(prog, "hud", None);
    p.arg(Arg::opt(&["--ascii"]).flag().help("ASCII-only output (no emoji)")).arg(sock());
    add("hud", "Print a one-line stick status summary (stdout; designed for Claude Code's statusLine)", p);

    add(
        "unpair",
        "Clear the stick's stored BLE bond (you must also Forget on the macOS side afterwards)",
        sub(prog, "unpair", None),
    );

    let mut p = sub(prog, "vision-test", None);
    p.arg(Arg::pos("image").help("Path to a JPEG/PNG (anything macOS ImageIO decodes)"));
    add("vision-test", "Run the host face detector on an image file and print the face cmd it would send", p);

    let mut p = sub(prog, "scene-test", None);
    p.arg(Arg::pos("image").help("Path to a JPEG or PNG"))
        .arg(Arg::opt(&["--find"]).help("Also look for this thing in the image"));
    add("scene-test", "Describe one image with the model eve's voice sees through (one real API call)", p);

    let mut p = sub(prog, "intent-test", None);
    p.arg(
        Arg::opt(&["--expect"])
            .append(2)
            .required()
            .metavar(&["LABEL", "TEXT"])
            .help("leave | mute | unmute | look | stay, then a phrase; repeat for more"),
    );
    add("intent-test", "Check the goodbye / mute classifier on phrases (one real API call each)", p);

    // "notes" is already the diary's own listing (what eve SAW). This records the room, so it takes the name the
    // owner says out loud.
    let mut p = sub(prog, "take-notes", None);
    p.arg(Arg::pos("action").choices(&["start", "stop", "status", "list"]).optional().default_str("status"))
        .arg(sock());
    add("take-notes", "Take notes on the room: eve transcribes what is said until you stop it", p);

    let mut p = sub(prog, "move", None);
    p.arg(Arg::pos("preset").optional().default_str("dance").help(
        "sway, dance, nod, shake, bounce, wiggle, fig8, doubletake, shrug, tilt, perk, droop, lean_peek, home",
    ))
    .arg(Arg::opt(&["--speed"]).float().default(Val::Float(1.0)).help(
        "0.5-2.0; above 1 is a quicker beat, and the board narrows the swing to match",
    ))
    .arg(Arg::opt(&["--stop"]).flag().help("stop whatever is running, where it is"))
    .arg(Arg::opt(&["--keys"]).help(
        "a raw keyframe list instead of a preset, e.g. '[[0,127,35,400],[900,127,45,400]]' (127 leaves an axis alone)",
    ))
    .arg(Arg::opt(&["--yaw-amp"]).int())
    .arg(Arg::opt(&["--pitch-amp"]).int())
    .arg(Arg::opt(&["--period-ms"]).int())
    .arg(Arg::opt(&["--cycles"]).int())
    .arg(sock());
    add("move", "Move the head: a named rhythm or gesture the board runs by itself", p);

    let mut p = sub(prog, "head-cal", None);
    p.arg(Arg::opt(&["--port"]).int().default(Val::Int(8766)))
        .arg(Arg::opt(&["--no-open"]).flag().help("do not open a browser"))
        .arg(sock());
    add(
        "head-cal",
        "Open the head calibration page: tap an extreme, confirm the axis or flip it (saved on the robot)",
        p,
    );

    // help only: main() hands these words to the app first
    for app in eve_app_registry::APPS.iter() {
        if let Some((first, aliases)) = app.cli.split_first() {
            let mut p = Parser::new(&format!("{prog} {first}"), None, false);
            p.arg(Arg::pos(&format!("{}_args", app.slot)).remainder());
            cmds.push((first.clone(), aliases.to_vec(), Some(app.cli_help.clone()), p));
        }
    }
    let mut p = Parser::new(&format!("{prog} app"), None, false);
    p.arg(Arg::pos("app_args").remainder());
    cmds.push((
        "app".into(),
        Vec::new(),
        Some(
            "Your own apps: new <name> [--dry-run] writes one (a module, its test, its doc and its registry line); \
             list shows them. See docs/adding-an-app.md"
                .into(),
        ),
        p,
    ));
    let mut add = |name: &str, help: &str, p: Parser| cmds.push((name.into(), Vec::new(), Some(help.into()), p));

    let mut p = sub(prog, "chrome-profile", None);
    p.arg(
        Arg::pos("url")
            .optional()
            .default_str("https://accounts.google.com/")
            .help("the page to open in it (default: Google sign-in)"),
    );
    add(
        "chrome-profile",
        "Sign in to eve's own Chrome (EVE_BROWSER_OWN): opens it with nothing connected, waits for you to quit it, \
         then checks the sign-in. See docs/stackchan/routing.md",
        p,
    );

    let mut p = sub(prog, "steel-profile", None);
    p.arg(
        Arg::pos("url")
            .optional()
            .default_str("https://accounts.google.com/")
            .help("the page to open in it (default: Google sign-in)"),
    );
    add(
        "steel-profile",
        "Sign in to sites in eve's Steel browser (EVE_BROWSER_STEEL): opens a Steel session in a window on eve's \
         Steel profile, waits for Enter, then closes it. See docs/steel.md",
        p,
    );

    let mut p = sub(prog, "sound", None);
    p.arg(Arg::pos("action").choices(&["on", "off", "status"]).optional().default_str("status")).arg(sock());
    add("sound", "Mute or unmute eve (the head and lights keep moving), or show which it is", p);

    let mut p = sub(prog, "mic", None);
    p.arg(Arg::pos("action").choices(&["on", "off", "status"]).optional().default_str("status")).arg(sock());
    add(
        "mic",
        "Turn the Mac microphone (the wake word) off or on for good, or show whether eve is listening",
        p,
    );

    let mut p = sub(prog, "identity", None);
    p.arg(Arg::pos("action").choices(&["status", "reset"]).optional().default_str("status")).arg(sock());
    add("identity", "Owner face prints: show what the robot knows, or forget it (enrol by holding Option)", p);

    let mut p = sub(prog, "explore", None);
    p.arg(Arg::pos("action").choices(&["start", "stop", "status"]).optional().default_str("start").help(
        "start (default): explore now, ignoring the idle timer; stop: come back; status: what the explorer is doing",
    ))
    .arg(sock());
    add("explore", "Send the robot off to look around the room now (or call it back / ask what it is doing)", p);

    let mut p = sub(prog, "notes", None);
    p.arg(Arg::opt(&["-n", "--last"]).int().default(Val::Int(20)).help("Show the last N notes (default 20; 0 = all)"));
    add("notes", "Print the idle explorer's recent notes (what the robot saw while Claude was quiet)", p);

    let mut p = sub(prog, "photos", None);
    p.arg(Arg::pos("action").choices(&["list", "open"]).optional().default_str("list")).arg(
        Arg::opt(&["-n", "--last"]).int().default(Val::Int(10)).help("How many to list, newest first (default 10; 0 = all)"),
    );
    add("photos", "The pictures eve kept of views it found cool: list them, or open the newest", p);

    let mut p = sub(prog, "notes-test", None);
    p.arg(Arg::pos("image").help("Path to a JPEG or PNG"));
    add("notes-test", "Send one image to the notes model and print the sentence (one real API call)", p);

    let mut p = sub(prog, "push-character", None);
    p.arg(Arg::pos("path").help("Path to the character folder"));
    add("push-character", "Upload a GIF character pack folder to the stick (manifest.json + *.gif)", p);

    add("check-update", "Check GitHub for a newer eve-bridge release (forces refresh)", sub(prog, "check-update", None));

    let mut p = sub(prog, "update", None);
    p.arg(Arg::opt(&["-y", "--yes"]).flag().help("Skip the confirmation prompt"));
    add("update", "Pull latest release, reinstall the package, and restart the daemon", p);

    let mut p = sub(prog, "audit", None);
    p.arg(Arg::opt(&["-n", "--last"]).int().default(Val::Int(20)).help("Show the last N entries (default 20; 0 = all)"))
        .arg(Arg::opt(&["-f", "--follow"]).flag().help("Stream new entries as they're written"))
        .arg(Arg::opt(&["--decision"]).choices(&["allow", "deny"]).help("Filter by final decision"))
        .arg(
            Arg::opt(&["--source"])
                .choices(&["auto_allow", "stick", "timeout", "defer", "ble_disconnected"])
                .help("Filter by decision source"),
        )
        .arg(Arg::opt(&["--tool"]).help("Filter by tool name (Bash, Edit, ...)"))
        .arg(Arg::opt(&["--ascii"]).flag().help("ASCII-only output (no colour)"))
        .arg(Arg::opt(&["--path"]).flag().help("Print the audit log path and exit"));
    add("audit", "Show the PreToolUse decision audit log (tail + filter + follow)", p);

    let mut p = sub(prog, "learning", None);
    p.arg(Arg::opt(&["--demo"]).flag())
        .arg(Arg::opt(&["--port"]).int().default(Val::Int(48766)))
        .arg(Arg::opt(&["--data-dir"]))
        .arg(Arg::opt(&["--no-open"]).flag())
        .arg(Arg::opt(&["--env-file"]).help("Explicit environment file (default ~/.config/eve-bridge/env)"));
    add("learning", "Open the saved lessons and whiteboard", p);

    let mut p = sub(
        prog,
        "lesson",
        Some(
            "Drive eve's lesson on the running daemon, in any subject. Actions: open, start, ideas, hint, check, \
             step, status, recap, end, listen, stop-listening.",
        ),
    );
    p.arg(Arg::pos("action").choices(&eve_learning::LESSON_ACTIONS).metavar(&["action"]).help(
        "open: show the whiteboard; start: new lesson (--mode, --topic, --level); ideas: save your thinking (--text; \
         empty means stuck); hint: a nudge; check: review your work; step: exactly one next step; status: latest \
         feedback; recap: summary and a question; end: save and finish; listen: eve listens while you think out \
         loud about the open lesson; stop-listening: eve stops listening and the microphone closes",
    ))
    .arg(
        Arg::opt(&["--mode"])
            .choices(&["learn", "help"])
            .help("start: learn a topic (default) or get help with your own problem"),
    )
    .arg(Arg::opt(&["--topic"]).help("start: the topic, for example Fractions or Rust ownership"))
    .arg(Arg::opt(&["--level"]).help(
        "start: the level, free text, for example \"Grade 4\" or \"second-year physics\"",
    ))
    .arg(Arg::opt(&["--text"]).help("ideas: what you are thinking"))
    .arg(sock());
    add("lesson", "Drive eve's lesson from the terminal (same path as the voice tool)", p);

    add("memory", "eve's memory: forget, dream, reindex; or watch the memory bus (EVE_ROSBRIDGE=1)", memory_parser(prog));

    let mut p = sub(
        prog,
        "apps",
        Some(
            "login google: browser sign-in with the Desktop OAuth client in \
             ~/.config/eve-bridge/google-oauth-client.json. GitHub and Maps read EVE_GITHUB_MCP_TOKEN and \
             EVE_GOOGLE_MAPS_KEY from the env file. status: each app's sign-in, path (mcp or rest) and tool count. \
             See docs/apps.md.",
        ),
    );
    let pp = p.prog.clone();
    let mut login = Parser::new(&format!("{pp} login"), None, true);
    login.arg(Arg::pos("service").choices(&["google"]));
    p.subparsers(
        "apps_cmd",
        vec![
            ("login", &[], Some("One-time browser sign-in for Google"), login),
            ("status", &[], Some("Which apps are signed in and answering"), Parser::new(&format!("{pp} status"), None, true)),
        ],
    );
    add("apps", "The owner's direct apps (Gmail, Calendar, Drive, GitHub, Maps): sign in, or show what is up", p);

    let mut p = sub(
        prog,
        "link",
        Some(
            "pair: print the link that pairs Eve Link with this Mac (eve's current address, read from the \
             rendezvous, plus the link token in the fragment). Open it on the iPhone. The link holds the token: \
             anyone with it can use eve, so send it only to your own phone.",
        ),
    );
    let pp = p.prog.clone();
    p.subparsers(
        "link_cmd",
        vec![(
            "pair",
            &[],
            Some("Print the pairing link (and a QR code when qrencode is installed)"),
            Parser::new(&format!("{pp} pair"), None, true),
        )],
    );
    add("link", "Eve Link: the pairing link for a phone, without Telegram", p);

    let mut p = sub(
        prog,
        "notify",
        Some(
            "topic: the ntfy topic (derived from Eve Link's link token) and how to subscribe on the iPhone. test: \
             post one notification on it. See docs/notifications.md.",
        ),
    );
    let pp = p.prog.clone();
    p.subparsers(
        "notify_cmd",
        vec![
            ("topic", &[], Some("Print the ntfy topic and how to subscribe"), Parser::new(&format!("{pp} topic"), None, true)),
            ("test", &[], Some("Post one test notification"), Parser::new(&format!("{pp} test"), None, true)),
        ],
    );
    add(
        "notify",
        "Notifications on the phone through ntfy: the topic to subscribe to, or one test notification",
        p,
    );

    let mut p = sub(
        prog,
        "places",
        Some(
            "day [YYYY-MM-DD]: that day's places and times (today by default). week [YYYY-MM-DD]: the week holding \
             it. habits: the usual places, days and times. summary: what is kept. forget: wipe the history, the \
             places, the things and the latest fix, after a yes. See docs/places.md.",
        ),
    );
    p.arg(Arg::pos("places_cmd").choices(&["day", "week", "habits", "summary", "forget"]))
        .arg(Arg::pos("day").optional().default_str(""))
        .arg(Arg::opt(&["--yes"]).flag().help("forget without asking"));
    add("places", "The places history: a day's digest, the week, habits, or forget it all", p);

    let mut p = sub(
        prog,
        "refresh",
        Some(
            "status: print both dates and the steps. remind: post them on the phone and the Mac. schedule / \
             unschedule: run remind every Friday at 09:00. See docs/refresh.md.",
        ),
    );
    let pp = p.prog.clone();
    let refresh: Vec<(&str, &[&str], Option<&str>, Parser)> = [
        ("status", "Print when each lapses and what to run"),
        ("remind", "Post the reminder on the phone (ntfy) and the Mac"),
        ("schedule", "Run remind every Friday at 09:00 (a LaunchAgent)"),
        ("unschedule", "Stop the weekly reminder"),
    ]
    .into_iter()
    .map(|(name, text)| (name, &[][..], Some(text), Parser::new(&format!("{pp} {name}"), None, true)))
    .collect();
    p.subparsers("refresh_cmd", refresh);
    add("refresh", "The weekly refresh reminder: when Eve Link's signing and the Google sign-in lapse", p);

    add(
        "telegram-check",
        "Text door: check EVE_TELEGRAM_TOKEN and print the numeric id of whoever messaged the bot",
        sub(
            prog,
            "telegram-check",
            Some(
                "Run with the daemon's text door off (one poller per bot). Prints ids and first names only, never \
                 message text. See docs/stackchan/telegram.md.",
            ),
        ),
    );

    let choices: Vec<(&str, &[&str], Option<&str>, Parser)> = Vec::new();
    parser.subparsers("cmd", choices);
    if let Some(crate::argparse::Arg { kind: crate::argparse::Kind::Subparsers(s), .. }) = parser.args.last_mut() {
        s.choices = cmds
            .into_iter()
            .map(|(name, aliases, help, parser)| crate::argparse::Choice { name, aliases, help, parser })
            .collect();
    }
    parser
}

/// `eve-bridge memory`'s parser (full eve).
fn memory_parser(prog: &str) -> Parser {
    let mut p = sub(
        prog,
        "memory",
        Some(
            "forget: remove words from every memory store, after a preview and a yes. dream: run the nightly dream \
             now. reindex: rebuild the mem0 index from the transcripts. tail: print every event on the memory bus. \
             recall: search the owner's claude-mem for eve's memories (EVE_CLAUDE_MEM=1). forget, dream and reindex \
             need EVE_MEMORY=1; run dream and reindex with the daemon stopped (the mem0 index takes one opener at a \
             time).",
        ),
    );
    let pp = p.prog.clone();
    let mut tail = Parser::new(&format!("{pp} tail"), None, true);
    tail.arg(Arg::opt(&["--url"]).default_str("ws://127.0.0.1:9090").help("rosbridge WebSocket URL"))
        .arg(Arg::opt(&["--topic"]).default_str("/eve/*").help("Topic, or a prefix ending in * (default /eve/*)"));
    let mut recall = Parser::new(&format!("{pp} recall"), None, true);
    recall
        .arg(Arg::pos("query"))
        .arg(Arg::opt(&["--limit"]).int().default(Val::Int(5)))
        .arg(Arg::opt(&["--url"]).default_str("ws://127.0.0.1:9090").help("rosbridge WebSocket URL"));
    let mut forget = Parser::new(&format!("{pp} forget"), None, true);
    forget.arg(Arg::pos("query").help("The words to forget; a phrase in double quotes matches as written"));
    let mut dream = Parser::new(&format!("{pp} dream"), None, true);
    dream.arg(Arg::opt(&["--day"]).help("A transcript day, YYYY-MM-DD, dreamt again even if done"));
    p.subparsers(
        "memory_cmd",
        vec![
            ("tail", &[], Some("Print one line per event until Ctrl-C"), tail),
            ("recall", &[], Some("Call /eve/memory/recall and print the results"), recall),
            ("forget", &[], Some("Count what matches in every store, then forget it on a yes"), forget),
            ("dream", &[], Some("Dream the due nights now, or one day with --day"), dream),
            ("reindex", &[], Some("Rebuild the mem0 index from every transcript day"), Parser::new(&format!("{pp} reindex"), None, true)),
        ],
    );
    p
}

/// Full eve's command line, after the env file, the profile and the apps' own words (cli.py's `main` from
/// `argparse.ArgumentParser` on).
pub fn full_main(argv: &[String], io: &Io, env: &dyn Fn(&str) -> Option<String>) -> i32 {
    let parser = full_parser(env);
    let args = match parse(&parser, argv, io) {
        Ok(a) => a,
        Err(code) => return code,
    };
    let Some(cmd) = args.str("cmd") else {
        print_help(&parser, io);
        return 1;
    };
    let socket = args.str("socket");
    let socket = socket.as_deref();
    match cmd.as_str() {
        "codex-home" => run_codex_home(io),
        "learning" => {
            let mut options = vec!["--port".to_string(), args.get("port").int().unwrap_or(48766).to_string()];
            if args.flag("demo") {
                options.push("--demo".into());
            }
            if args.flag("no_open") {
                options.push("--no-open".into());
            }
            if let Some(d) = args.str("data_dir").filter(|d| !d.is_empty()) {
                options.extend(["--data-dir".to_string(), d]);
            }
            if let Some(f) = args.str("env_file").filter(|f| !f.is_empty()) {
                options.extend(["--env-file".to_string(), f]);
            }
            report(eve_learning::server::main_with(
                &options,
                &mut eve_envfile::ProcessEnv,
                &eve_learning::server::start,
                &mut std::io::stderr(),
            ))
        }
        "lesson" => run_lesson(&args, io),
        "memory" => match args.str("memory_cmd").as_deref() {
            None => {
                if let Some(p) = parser.find(&["memory"]) {
                    print_help(p, io);
                }
                1
            }
            Some("forget" | "dream" | "reindex") => run_memory_store(&args, io, &memory::Seams::process()),
            Some(_) => run_memory(&args, io),
        },
        "apps" => match args.str("apps_cmd") {
            None => help_of(&parser, "apps", io),
            Some(c) => run_apps(&c, io),
        },
        "link" => match args.str("link_cmd") {
            None => help_of(&parser, "link", io),
            Some(_) => run_link(io),
        },
        "notify" => match args.str("notify_cmd") {
            None => help_of(&parser, "notify", io),
            Some(c) => run_notify(&c, io),
        },
        "places" => run_places(&args, io),
        "refresh" => match args.str("refresh_cmd") {
            None => help_of(&parser, "refresh", io),
            Some(c) => run_refresh(&c, io),
        },
        "daemon" => run_daemon(&args, io),
        "install" => {
            // --notes-widget is additive: on its own it installs only the widget unit; combined with --service it
            // installs both. Without it the install path is exactly what it was.
            if args.flag("notes_widget") {
                let rc = report(eve_service::service::install_notes_widget());
                if rc != 0 || !args.flag("service") {
                    return rc;
                }
            }
            if args.flag("service") {
                return report(eve_service::service::install_service(args.str("serial_port").as_deref()));
            }
            report(eve_installer::install_hooks(args.str("config_dir").as_deref().map(std::ffi::OsStr::new)))
        }
        "uninstall" => {
            if args.flag("notes_widget") {
                let rc = report(eve_service::service::uninstall_notes_widget());
                if rc != 0 || !args.flag("service") {
                    return rc;
                }
            }
            if args.flag("service") {
                return report(eve_service::service::uninstall_service());
            }
            report(eve_installer::uninstall_hooks(args.str("config_dir").as_deref().map(std::ffi::OsStr::new)))
        }
        "notes-widget" => {
            init_logging("INFO");
            report(eve_notes_widget::run(args.flag("once"), None, None, None))
        }
        "steel-profile" => run_steel_profile(&args.str("url").unwrap_or_default(), io),
        "chrome-profile" => run_chrome_profile(&args.str("url").unwrap_or_default(), io),
        "status" => report(eve_installer::show_status(args.str("config_dir").as_deref().map(std::ffi::OsStr::new))),
        "celebrate" => {
            let secs = args.get("secs").float();
            let Some(resp) = io.post(&json!({"evt": "celebrate", "secs": secs}), socket, 3.0) else {
                io.warn("eve-bridge: daemon not reachable");
                return 2;
            };
            if !truthy(get(&resp, "connected")) {
                io.warn("board not connected — nothing to celebrate on");
                return 2;
            }
            io.say(&format!("pet celebrating for {secs:.0}s"));
            0
        }
        "species" => run_species(&args.str("name").unwrap_or_default(), socket, io),
        "diag" => run_diag(socket, args.flag("watch"), io),
        "key-check" => eve_key_tap::KeyTapper::new(None, None).diagnose(),
        "telegram-check" => match runtime() {
            Ok(rt) => rt.block_on(eve_brain::telegram::make::diagnose(None, None)) as i32,
            Err(e) => report::<i32>(Err(e.into())),
        },
        "ears-check" => eve_ears::diagnose(args.get("seconds").float()),
        "hud" => eve_hud::run(args.flag("ascii"), socket),
        "unpair" => run_unpair(io),
        "vision-test" => eve_vision::run_vision_test(&args.str("image").unwrap_or_default()),
        "scene-test" => eve_scene::run_scene_test(&args.str("image").unwrap_or_default(), args.str("find").as_deref()),
        "intent-test" => {
            let pairs: Vec<(String, String)> = match args.get("expect") {
                Val::Lists(l) => l.iter().map(|p| (p[0].clone(), p[1].clone())).collect(),
                _ => Vec::new(),
            };
            let bad: Vec<Value> = pairs
                .iter()
                .filter(|(label, _)| !["leave", "mute", "unmute", "look", "stay"].contains(&label.as_str()))
                .map(|(label, _)| Value::String(label.clone()))
                .collect();
            if !bad.is_empty() {
                io.warn(&format!(
                    "intent-test: unknown label(s) {}; use leave, mute, unmute, look or stay",
                    eve_ipc::py_repr(&Value::Array(bad))
                ));
                return 2;
            }
            eve_intent::run_intent_test(&pairs)
        }
        "take-notes" => run_notes(&args.str("action").unwrap_or_default(), socket, io),
        "move" => run_move(&args, io),
        "head-cal" => {
            let port = args.get("port").int().unwrap_or(8766);
            let Ok(port) = u16::try_from(port) else {
                io.warn(&format!("eve-bridge: OverflowError: bind(): port must be 0-65535. ({port})"));
                return 1;
            };
            report(eve_head_cal::serve(port, socket, !args.flag("no_open")))
        }
        "sound" => run_sound(&args.str("action").unwrap_or_default(), socket, io),
        "mic" => run_mic(&args.str("action").unwrap_or_default(), socket, io),
        "identity" => eve_identity::run_identity(&args.str("action").unwrap_or_default(), socket),
        "explore" => run_explore(&args.str("action").unwrap_or_default(), socket, io),
        "notes" => eve_explore::run_notes_cli(args.get("last").int().unwrap_or(20), None),
        "photos" => run_photos(&args.str("action").unwrap_or_default(), args.get("last").int().unwrap_or(10), io),
        "notes-test" => eve_explore::run_notes_test(&args.str("image").unwrap_or_default(), None),
        "push-character" => run_push_character(&args.str("path").unwrap_or_default(), io),
        "audit" => {
            if args.flag("path") {
                io.say(&eve_audit::default_path().display().to_string());
                return 0;
            }
            let stop = interrupt_flag();
            let opts = eve_audit::RenderOptions {
                path: None,
                last: args.get("last").int().unwrap_or(20),
                decision: args.str("decision"),
                source: args.str("source"),
                tool: args.str("tool"),
                ascii_only: args.flag("ascii"),
                follow: args.flag("follow"),
                stop: Some(stop),
            };
            report(eve_audit::render_stdout(&opts).map_err(anyhow::Error::from))
        }
        "check-update" => {
            let info = eve_version_check::check(true);
            io.say(&eve_version_check::render_cli(&info));
            if info.has_update() {
                1
            } else {
                0
            }
        }
        "update" => eve_update::run_update(args.flag("yes")),
        _ => 1,
    }
}

fn help_of(parser: &Parser, name: &str, io: &Io) -> i32 {
    if let Some(p) = parser.find(&[name]) {
        print_help(p, io);
    }
    1
}

/// A command's result, as the Python CLI ends: its exit code, or the exception's line and 1 (Python's traceback
/// exit).
pub fn report<T: Into<i64>>(r: anyhow::Result<T>) -> i32 {
    match r {
        Ok(code) => code.into() as i32,
        Err(e) => {
            eprintln!("eve-bridge: {e:#}");
            1
        }
    }
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread().enable_all().build()
}

/// `logging.basicConfig(level=..., format="%(asctime)s %(levelname)s %(name)s: %(message)s")`.
pub fn init_logging(level: &str) {
    let filter = tracing_subscriber::EnvFilter::new(match eve_lite::level_of(level) {
        tracing::Level::TRACE => "trace",
        tracing::Level::DEBUG => "debug",
        tracing::Level::INFO => "info",
        tracing::Level::WARN => "warn",
        tracing::Level::ERROR => "error",
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .event_format(eve_lite::PyFormat)
        .try_init();
}

/// Ctrl-C as Python's KeyboardInterrupt for the commands that wait in a loop: a flag the loop reads, so it can
/// return 0 as cli.py does.
pub fn interrupt_flag() -> Arc<AtomicBool> {
    static FLAG: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();
    FLAG.get_or_init(|| {
        let flag = Arc::new(AtomicBool::new(false));
        #[cfg(unix)]
        {
            static RAISED: AtomicBool = AtomicBool::new(false);
            extern "C" fn on_sigint(_: libc::c_int) {
                RAISED.store(true, Ordering::SeqCst);
            }
            // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
            unsafe {
                libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
            }
            let f = flag.clone();
            let _ = std::thread::Builder::new().name("sigint".into()).spawn(move || loop {
                if RAISED.load(Ordering::SeqCst) {
                    f.store(true, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(50));
            });
        }
        flag
    })
    .clone()
}

/// Sleep `secs`, waking early on Ctrl-C; false when interrupted.
fn sleep_unless_interrupted(secs: f64, flag: &AtomicBool) -> bool {
    let end = std::time::Instant::now() + Duration::from_secs_f64(secs);
    while std::time::Instant::now() < end {
        if flag.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !flag.load(Ordering::SeqCst)
}

fn run_codex_home(io: &Io) -> i32 {
    match eve_codex_auth::setup(&eve_codex_computer::executable(), None) {
        Ok(home) => {
            io.say(&format!("Eve Codex API home ready: {}", home.display()));
            io.say("Your personal Codex sign-in is unchanged. Enable with EVE_CODEX_AUTH=api.");
            0
        }
        // Never echo config/subprocess exceptions: they may contain secrets.
        Err(eve_codex_auth::Error::Setup(e)) => {
            io.warn(&e.to_string());
            1
        }
        Err(_) => {
            io.warn("Could not prepare Eve's Codex home.");
            1
        }
    }
}

// ---- the daemon ------------------------------------------------------------------------------------------------

/// What `eve-bridge daemon` builds full eve's Daemon with.
pub fn daemon_options(args: &Namespace) -> eve_daemon::DaemonOptions {
    eve_daemon::DaemonOptions {
        socket_path: args.str("socket"),
        device_name_prefix: args.str("device_name").unwrap_or_default(),
        device_address: args.str("device_address"),
        matchers: None,
        serial_port: args.str("serial_port"),
        save_frames: args.str("save_frames"),
    }
}

fn run_daemon(args: &Namespace, io: &Io) -> i32 {
    init_logging(&args.str("log_level").unwrap_or_else(|| "INFO".into()));
    // Refuse to start if another daemon is already listening on this IPC address. A stale Unix socket is safe to
    // remove and proceed.
    let socket = args.str("socket");
    let transport = match eve_ipc::make_transport(socket.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            io.warn(&format!("eve-bridge: invalid IPC address: {e}"));
            return 2;
        }
    };
    if transport.is_in_use() {
        io.warn(&format!(
            "eve-bridge: another daemon is already listening at {}.\n  Stop it first, or pass --socket to use a \
             different path.",
            transport.address()
        ));
        return 2;
    }
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return report::<i32>(Err(e.into())),
    };
    let daemon = {
        let _entered = rt.enter();
        match eve_daemon::Daemon::new(daemon_options(args)) {
            Ok(d) => d,
            Err(e) => return report::<i32>(Err(e)),
        }
    };
    // A stalled loop cannot log why it stalled; this thread does it from outside (loop_watchdog), and SIGUSR1
    // dumps every thread for the cases it cannot see.
    let watchdog = eve_loop_watchdog::LoopWatchdog::new(rt.handle().clone()).start().ok();
    #[cfg(unix)]
    let _ = eve_loop_watchdog::register_dump_signal(libc::SIGUSR1);
    let result = rt.block_on(async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            for kind in [SignalKind::interrupt(), SignalKind::terminate()] {
                if let Ok(mut stream) = signal(kind) {
                    let d = daemon.clone();
                    tokio::spawn(async move {
                        while stream.recv().await.is_some() {
                            d.shutdown().await;
                        }
                    });
                }
            }
        }
        daemon.run().await
    });
    if let Some(w) = watchdog {
        w.stop();
    }
    rt.shutdown_timeout(Duration::from_secs(1));
    match result {
        Ok(()) => 0,
        Err(e) => report::<i32>(Err(e)),
    }
}

// ---- the robot over IPC ----------------------------------------------------------------------------------------

fn run_push_character(path: &str, io: &Io) -> i32 {
    // Pushing a full 1.8 MB pack at BLE speeds can take 1-2 minutes with the per-chunk ack requirement. Give the
    // IPC call plenty of headroom.
    let Some(resp) = io.post(&json!({"evt": "push_character", "path": path}), None, 600.0) else {
        io.warn("eve-bridge: daemon not reachable. Start it first.");
        return 2;
    };
    if !truthy(get(&resp, "ok")) {
        io.warn(&format!("push failed: {}", pstr(&py::get_or(&resp, "error", json!("unknown")))));
        return 2;
    }
    let name = pstr(&py::get_or(&resp, "name", json!("?")));
    let files = pstr(&py::get_or(&resp, "files", json!(0)));
    let size = py::commas(&py::get_or(&resp, "total_bytes", json!(0)));
    io.say(&format!("pushed '{name}': {files} files, {size} bytes"));
    io.say("the stick has switched to the new character.");
    0
}

/// `_render_diag`: print a board diag report. Abnormal resets get the loud treatment — that line is the answer to
/// "why did it freeze".
pub fn render_diag(d: Option<&Value>, connected: bool, io: &Io) {
    io.say(&format!("board connected: {}", if connected { "True" } else { "False" }));
    let Some(d) = d.filter(|d| truthy(d)) else {
        io.say("no diag report yet — the board sends one at boot; if it just\nreconnected, run this again in a couple of seconds.");
        return;
    };
    let reset = py::get_or(d, "reset", json!("?"));
    let abnormal = reset
        .as_str()
        .is_some_and(|r| ["PANIC", "TASK-WATCHDOG", "INT-WATCHDOG", "BROWNOUT", "other-watchdog"].contains(&r));
    let mark = if abnormal { "  <-- ABNORMAL" } else { "" };
    io.say(&format!("boot #{}   last reset: {}{mark}", pstr(get(d, "boot")), pstr(&reset)));
    io.say(&format!(
        "uptime {}s   heap {} (min {})   psram {}",
        pstr(get(d, "up")),
        pstr(get(d, "heap")),
        pstr(get(d, "minheap")),
        pstr(get(d, "psram"))
    ));
    if truthy(get(d, "diedIn")) {
        // The single most useful line on a hang: the call that never returned.
        io.say(&format!(
            "\nDIED IN: {}   (entered {}ms, {} loops)",
            pstr(get(d, "diedIn")),
            pstr(get(d, "diedAtMs")),
            pstr(get(d, "loops"))
        ));
    }
    let last: Vec<Value> = match get(d, "last") {
        Value::Array(a) => a.clone(),
        v if truthy(v) => match v {
            Value::String(s) => s.chars().map(|c| Value::String(c.to_string())).collect(),
            Value::Object(m) => m.keys().map(|k| Value::String(k.clone())).collect(),
            other => vec![other.clone()],
        },
        _ => Vec::new(),
    };
    if last.is_empty() {
        io.say("no surviving event ring (clean power-on, or first boot on this build)");
        return;
    }
    io.say("\nwhat it was doing before that reset (oldest first):");
    for ev in &last {
        let text = pstr(ev);
        let (ms, rest) = match text.split_once(':') {
            Some((a, b)) => (a.to_string(), b.to_string()),
            None => (text.clone(), String::new()),
        };
        let stamp = match crate::argparse::py_int(&ms) {
            Some(n) => format!("{:8.2}s", n as f64 / 1000.0),
            None => ms,
        };
        io.say(&format!("  {stamp}  {rest}"));
    }
}

/// Species table order, mirroring firmware src/eve.cpp SPECIES_TABLE. The saved index is positional, so this list
/// must move with that one.
pub const SPECIES: [&str; 18] = [
    "capybara", "duck", "goose", "blob", "cat", "dragon", "octopus", "owl", "penguin", "turtle", "snail", "ghost",
    "axolotl", "cactus", "robot", "rabbit", "mushroom", "chonk",
];

fn run_photos(action: &str, last: i64, io: &Io) -> i32 {
    let notes_dir = eve_explore::configured(None).notes_dir;
    let recent = eve_photos::iter_recent(&notes_dir, if action == "list" { last } else { 1 });
    if recent.is_empty() {
        io.say(&format!("no photos yet in {}", notes_dir.join("photos").display()));
        return 0;
    }
    if action == "open" {
        let newest = &recent[0];
        if !cfg!(target_os = "macos") {
            io.say(&newest.display().to_string());
            return 0;
        }
        let _ = std::process::Command::new("open").arg(newest).status();
        io.say(&format!("opened {}", newest.display()));
        return 0;
    }
    for path in &recent {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let parent = path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let head = stem.split('-').next().unwrap_or("");
        io.say(&format!("{parent} {head}  {:6.0} KB  {}", size as f64 / 1024.0, path.display()));
    }
    io.say(&format!("({})", eve_photos::format_usage(&notes_dir)));
    0
}

/// `eve-bridge take-notes start|stop|status|list`. `stop` here is the one with no latency: every other way of
/// stopping has to wait for a segment to come back from the transcriber.
fn run_notes(action: &str, socket: Option<&str>, io: &Io) -> i32 {
    if action == "list" {
        let files = match eve_notes::recent(&eve_records::recall::configured(None), 20) {
            Ok(f) => f,
            Err(e) => return report::<i32>(Err(e.into())),
        };
        if files.is_empty() {
            io.say("no notes yet — say \u{201c}start taking notes\u{201d}, or run `notes start`");
            return 0;
        }
        for f in files {
            let mut first = String::new();
            if let Ok(text) = std::fs::read_to_string(&f) {
                for line in eve_envfile::py_splitlines(&text) {
                    if let Some(rest) = line.strip_prefix("# ") {
                        first = eve_envfile::py_strip(rest).to_string();
                        break;
                    }
                }
            }
            let parent = f.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let head: String = stem.chars().take(4).collect();
            io.say(&format!("{parent} {head}  {}", if first.is_empty() { &stem } else { &first }));
            io.say(&format!("    {}", f.display()));
        }
        return 0;
    }
    let Some(resp) = io.post(&json!({"evt": "notes", "action": action}), socket, 180.0) else {
        io.warn("eve-bridge: daemon not reachable");
        return 2;
    };
    if !truthy(get(&resp, "ok")) {
        io.warn(&format!("eve-bridge: {}", py::or_str(&resp, "error", "that did not work")));
        return 1;
    }
    let minutes = || pstr(&py::get_or(&resp, "minutes", json!(0)));
    if action == "start" {
        if truthy(get(&resp, "already")) {
            io.say(&format!("already taking notes, {} min in", minutes()));
        } else {
            io.say("taking notes — say \u{201c}stop taking notes\u{201d}, tap the robot, or run `notes stop`");
            io.say("eve cannot hear its own name while it does this");
        }
        return 0;
    }
    if action == "stop" {
        let path = get(&resp, "path");
        io.say(&format!("stopped after {} min, {} words", minutes(), pstr(&py::get_or(&resp, "words", json!(0)))));
        io.say(&if truthy(path) { pstr(path) } else { "nothing was said, so nothing was written".to_string() });
        return 0;
    }
    if truthy(get(&resp, "active")) {
        io.say(&format!(
            "taking notes: {} min, {} segments, {} words",
            minutes(),
            pstr(&py::get_or(&resp, "segments", json!(0))),
            pstr(&py::get_or(&resp, "words", json!(0)))
        ));
        let last = py::or_str(&resp, "last", "");
        let last = eve_envfile::py_strip(&last);
        if !last.is_empty() {
            let chars: Vec<char> = last.chars().collect();
            let tail: String = chars[chars.len().saturating_sub(100)..].iter().collect();
            io.say(&format!("  last heard: …{tail}"));
        }
    } else {
        io.say("not taking notes");
    }
    0
}

/// `eve-bridge move [preset]`: the bench path, needing no conversation. There was no way to move the head without
/// talking to the robot, which made every motion change cost a wake word and a model round trip to look at.
fn run_move(args: &Namespace, io: &Io) -> i32 {
    let mut req = Map::new();
    req.insert("evt".into(), json!("move"));
    let preset = args.str("preset").unwrap_or_default();
    if args.flag("stop") {
        req.insert("kind".into(), json!("stop"));
    } else if let Some(keys) = args.str("keys").filter(|k| !k.is_empty()) {
        match serde_json::from_str::<Value>(&keys) {
            Ok(v) => {
                req.insert("kind".into(), json!("keys"));
                req.insert("keys".into(), v);
            }
            Err(e) => {
                io.warn(&format!("eve-bridge: --keys is not JSON ({e})"));
                return 2;
            }
        }
    } else if eve_motion::KEYS.iter().any(|(name, _)| *name == preset) {
        req.insert("kind".into(), json!("keys"));
        req.insert("preset".into(), json!(preset));
    } else {
        req.insert("kind".into(), json!("osc"));
        req.insert("preset".into(), json!(preset));
        req.insert("speed".into(), json!(args.get("speed").float()));
        for flag in ["yaw_amp", "pitch_amp", "period_ms", "cycles"] {
            if let Some(v) = args.get(flag).int() {
                req.insert(flag.into(), json!(v));
            }
        }
    }
    let stop = req.get("kind") == Some(&json!("stop"));
    let Some(resp) = io.post(&Value::Object(req), args.str("socket").as_deref(), 10.0) else {
        io.warn("eve-bridge: daemon not reachable");
        return 2;
    };
    if !truthy(get(&resp, "ok")) {
        io.warn(&format!("eve-bridge: {}", py::or_str(&resp, "error", "the board refused it")));
        return 1;
    }
    if stop {
        io.say("stopped");
        return 0;
    }
    let asked = get(&resp, "asked").clone();
    let sent = match get(&resp, "sent") {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    match eve_motion::describe(&sent) {
        Ok(text) => io.say(&format!("asked for {text}")),
        Err(e) => {
            io.warn(&format!("eve-bridge: {e}"));
            return 1;
        }
    }
    io.say("the board admits it against its own limits, so the delivered swing may be smaller");
    if truthy(get(&asked, "ms")) {
        if let Some(ms) = py::num(get(&asked, "ms")) {
            io.say(&format!("about {:.1}s of motion", ms / 1000.0));
        }
    }
    0
}

/// One line for `eve-bridge mic` and the menu-bar app: what the mic is doing and why.
pub fn describe_mic(resp: &Value) -> String {
    if !truthy(&py::get_or(resp, "available", json!(true))) {
        return "eve has no microphone (EVE_VOICE=0, or no input device or wake-word model)".into();
    }
    if get(resp, "mic") == &json!("off") {
        return "microphone off (your choice) — `eve-bridge mic on` turns it back on".into();
    }
    if truthy(get(resp, "listening")) {
        return format!(
            "microphone on: listening for the wake word{}",
            if truthy(get(resp, "always")) { " (EVE_MIC_ALWAYS=1)" } else { "" }
        );
    }
    if !truthy(get(resp, "connected")) {
        return "microphone closed: it opens when the robot connects".into();
    }
    "microphone closed: it could not be opened (see the daemon log)".into()
}

/// `eve-bridge mic [on|off|status]`: the owner's switch, and what the mic is doing now.
fn run_mic(action: &str, socket: Option<&str>, io: &Io) -> i32 {
    let Some(resp) = io.post(&json!({"evt": "mic", "action": action}), socket, 3.0) else {
        io.warn("eve-bridge: daemon not reachable");
        return 2;
    };
    io.say(&describe_mic(&resp));
    0
}

/// `eve-bridge sound [on|off|status]`: the daemon owns the port and the choice.
fn run_sound(action: &str, socket: Option<&str>, io: &Io) -> i32 {
    let Some(resp) = io.post(&json!({"evt": "sound", "action": action}), socket, 3.0) else {
        io.warn("eve-bridge: daemon not reachable");
        return 2;
    };
    let sound = py::get_or(&resp, "sound", json!("?"));
    let note = if truthy(get(&resp, "connected")) {
        ""
    } else {
        " (board not connected: it gets the setting when it connects)"
    };
    let head = if sound == json!("off") { "eve is muted — it still moves and lights up" } else { "eve's sound is on" };
    io.say(&format!("{head}{note}"));
    0
}

/// `eve-bridge explore [start|stop|status]`: talk to the daemon.
fn run_explore(action: &str, socket: Option<&str>, io: &Io) -> i32 {
    let Some(resp) = io.post(&json!({"evt": "explore", "action": action}), socket, 3.0) else {
        io.warn("eve-bridge: daemon not reachable");
        return 2;
    };
    let st = if truthy(get(&resp, "explore")) { get(&resp, "explore").clone() } else { json!({}) };
    if !truthy(get(&resp, "ok")) {
        io.warn(&format!("explore: {}", py::or_str(&resp, "error", "refused")));
        return 1;
    }
    let state = pstr(&py::get_or(&st, "state", json!("?")));
    let how = if truthy(get(&st, "manual")) { "manual" } else { "idle" };
    if action == "start" {
        io.say(&format!("eve is off exploring ({state}, {how}; explore stop brings it back)"));
    } else if action == "stop" {
        io.say(&if state == "off" { "eve is back".to_string() } else { format!("explore: {state}") });
    } else if state == "off" {
        io.say(&format!(
            "eve is not exploring{}",
            if truthy(get(&resp, "connected")) { "" } else { " (board not connected)" }
        ));
    } else {
        let wp = get(&st, "waypoint");
        let r#where = if truthy(wp) {
            let yaw = &wp[0];
            let yaw = match yaw.as_i64() {
                Some(y) => format!("{y:+}"),
                None => pstr(yaw),
            };
            format!(" at yaw={yaw} pitch={}", pstr(&wp[1]))
        } else {
            String::new()
        };
        io.say(&format!(
            "eve is {state} ({how}: {}){where}; {} cycle(s), {} note(s) this run",
            pstr(get(&st, "reason")),
            pstr(&py::get_or(&st, "cycles", json!(0))),
            pstr(&py::get_or(&st, "notes", json!(0)))
        ));
    }
    0
}

/// `eve-bridge lesson <action>`: the voice tool's lesson path, from a terminal.
pub fn run_lesson(args: &Namespace, io: &Io) -> i32 {
    let action = args.str("action").unwrap_or_default();
    if action == "ideas" && args.str("text").is_none() {
        // The lesson saves ideas as given: a bare `lesson ideas` would overwrite the learner's saved thinking with
        // nothing and mark them stuck. Make "stuck" an explicit choice.
        io.warn("lesson: ideas needs --text \"...\" (use --text \"\" to say you are stuck)");
        return 2;
    }
    let mut req = Map::new();
    req.insert("evt".into(), json!("lesson"));
    req.insert("action".into(), json!(action));
    for key in ["mode", "topic", "level", "text"] {
        if let Some(v) = args.str(key) {
            req.insert(key.into(), json!(v));
        }
    }
    // A tutor reply can take the model's 90 s plus a 15 s search; post() cannot tell a slow reply from a daemon
    // that is not running, so give it room and say so on failure.
    let slow = eve_learning::TUTOR_ACTIONS.contains(&action.as_str());
    let resp = (io.post)(&Value::Object(req), args.str("socket").as_deref(), if slow { 150.0 } else { 15.0 }, 3.0);
    let Some(resp) = resp else {
        io.warn(&format!(
            "eve-bridge: daemon not reachable. Start it with `eve-bridge daemon` (or the launchd service). \
             `eve-bridge learning` opens the whiteboard without the robot.{}",
            if slow { " If the daemon is running, the tutor may still be answering; try `lesson status`." } else { "" }
        ));
        return 2;
    };
    if !truthy(get(&resp, "ok")) {
        let why = if truthy(get(&resp, "error")) {
            pstr(get(&resp, "error"))
        } else if truthy(get(&resp, "reason")) {
            pstr(get(&resp, "reason"))
        } else {
            "that did not work".into()
        };
        io.warn(&format!("lesson: {why}"));
        return 1;
    }
    if action == "status" {
        let lesson = get(&resp, "lesson");
        if truthy(lesson) {
            let problem = get(lesson, "problem");
            io.say(&format!(
                "{} ({}, {}){}",
                pstr(get(lesson, "topic")),
                pstr(get(lesson, "mode")),
                pstr(get(lesson, "stage")),
                if truthy(problem) { format!(": {}", pstr(problem)) } else { String::new() }
            ));
        } else {
            io.say("no lesson is open");
        }
    }
    let answer = py::or_str(&resp, "answer", "");
    let answer = eve_envfile::py_strip(&answer);
    if !answer.is_empty() {
        io.say(answer);
    }
    0
}

/// Set the pet's character over the serial link. The on-device menu that used to do this is gone: customization
/// belongs on the host, where it is scriptable and does not suspend the pet's gestures.
fn run_species(name: &str, socket: Option<&str>, io: &Io) -> i32 {
    let key = eve_envfile::py_strip(name).to_lowercase();
    let idx: usize = if ["gif", "character", "pack"].contains(&key.as_str()) {
        0xFF // firmware sentinel: use the installed GIF pack
    } else if !key.is_empty() && key.chars().all(|c| c.is_numeric()) {
        let idx = key.chars().map(|c| c.to_digit(10).unwrap_or(0) as usize).fold(0usize, |a, d| a.saturating_mul(10).saturating_add(d));
        if idx >= SPECIES.len() {
            io.warn(&format!("index {idx} out of range (0..{})", SPECIES.len() - 1));
            return 2;
        }
        idx
    } else if let Some(i) = SPECIES.iter().position(|s| *s == key) {
        i
    } else {
        io.warn(&format!(
            "unknown species {}\n\navailable: {}\nor 'gif' for an installed character pack",
            crate::argparse::py_repr(name),
            SPECIES.join(", ")
        ));
        return 2;
    };
    let Some(resp) = io.post(&json!({"evt": "species", "idx": idx}), socket, 3.0) else {
        io.warn("eve-bridge: daemon not reachable (check `launchctl list | grep eve`).");
        return 2;
    };
    if !truthy(get(&resp, "connected")) {
        io.warn("board not connected — the change will not apply until it is.");
        return 2;
    }
    let shown = if idx == 0xFF { "GIF pack".to_string() } else { format!("{} (index {idx})", SPECIES[idx]) };
    io.say(&format!("species set to {shown}"));
    0
}

fn run_diag(socket: Option<&str>, watch: bool, io: &Io) -> i32 {
    let ask = || io.post(&json!({"evt": "diag"}), socket, 3.0);
    let Some(resp) = ask() else {
        io.warn("eve-bridge: daemon not reachable (start it, or check `launchctl list | grep eve`).");
        return 2;
    };
    if !watch {
        render_diag(resp.get("diag"), truthy(get(&resp, "connected")), io);
        return 0;
    }
    // Watch mode: the point is to be running WHEN it freezes, so the post-reset report is caught the moment the
    // board comes back.
    io.say("watching for board resets — Ctrl-C to stop\n");
    let stop = interrupt_flag();
    let mut seen: Option<(Value, Value)> = None;
    loop {
        if stop.load(Ordering::SeqCst) {
            return 0;
        }
        let resp = ask().unwrap_or(Value::Null);
        let d = get(&resp, "diag").clone();
        let key = (get(&d, "boot").clone(), get(&d, "reset").clone());
        if truthy(&d) && seen.as_ref() != Some(&key) {
            seen = Some(key);
            io.say(&format!("--- {}", chrono::Local::now().format("%H:%M:%S")));
            render_diag(Some(&d), truthy(get(&resp, "connected")), io);
            io.say("");
        }
        if !sleep_unless_interrupted(2.0, &stop) {
            return 0;
        }
    }
}

/// Sign in to eve's own Chrome (browser_lane, EVE'S OWN CHROME). It opens with no debugging port, because
/// Google's sign-in may refuse a Chrome another program can drive. When the owner quits it, it starts again with
/// the port and this says which Google accounts eve now sees.
fn run_chrome_profile(url: &str, io: &Io) -> i32 {
    let cfg = eve_browser_lane::configured(None);
    match eve_browser_lane::quit_own_chrome(&cfg.own_dir, 10.0) {
        Ok(true) => {}
        Ok(false) => {
            io.warn("eve-bridge: eve's own Chrome did not quit; quit it (⌘Q in that Chrome) and run this again");
            return 2;
        }
        Err(e) => return report::<i32>(Err(e)),
    }
    let cmd = eve_browser_lane::sign_in_command(&cfg.own_dir, url);
    if let Some((program, rest)) = cmd.split_first() {
        let _ = run_with_timeout(program, rest, Duration::from_secs(15));
    }
    io.say(&format!(
        "eve's own Chrome is open for signing in ({}), with nothing connected to it.",
        cfg.own_dir.display()
    ));
    io.say("Sign in to each site eve may use, then quit that Chrome (⌘Q). This waits; Ctrl-C to stop waiting.");
    let stop = interrupt_flag();
    let mut waited = sleep_unless_interrupted(3.0, &stop);
    while waited {
        match eve_browser_lane::own_chrome_running(&cfg.own_dir) {
            Ok(true) => waited = sleep_unless_interrupted(1.0, &stop),
            Ok(false) => break,
            Err(e) => return report::<i32>(Err(e)),
        }
    }
    if !waited {
        io.say("\nStopped waiting. Quit eve's Chrome when you're done; the next web task starts it again.");
        return 0;
    }
    let signed_in = match runtime() {
        Ok(rt) => rt.block_on(own_chrome_signed_in(&cfg)),
        Err(e) => Err(e.into()),
    };
    let signed_in = match signed_in {
        Ok(s) => s,
        Err(e) => {
            if let Some(attach) = e.downcast_ref::<eve_browser_lane::AttachError>() {
                io.warn(&format!("eve-bridge: {attach}"));
                return 2;
            }
            return report::<i32>(Err(e));
        }
    };
    io.say(&format!(
        "Google in eve's Chrome: {}",
        if signed_in { "signed in (Gmail opened the inbox)" } else { "NOT signed in (Gmail went to a sign-in page)" }
    ));
    if !cfg.own {
        io.say("Not in use yet: set EVE_BROWSER_OWN=1 in ~/.config/eve-bridge/env and restart the daemon.");
    }
    if signed_in {
        0
    } else {
        1
    }
}

/// `subprocess.run(argv, capture_output=True, text=True, timeout=...)`, the result ignored.
fn run_with_timeout(program: &str, args: &[String], timeout: Duration) -> std::io::Result<()> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let end = std::time::Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if std::time::Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Start eve's own Chrome with its port, open Gmail in eve's tab, and say whether it stayed signed in. Google's
/// account list (ListAccounts) answers 400 to a request from outside a page, and Chrome 154's cookies may not work
/// when copied out, so a real page load is the check. Leaves that Chrome running.
async fn own_chrome_signed_in(cfg: &eve_browser_lane::BrowserLaneConfig) -> anyhow::Result<bool> {
    let mut cfg = cfg.clone();
    cfg.own = true;
    cfg.attach = true;
    let lane = eve_browser_lane::BrowserLane::new(cfg, eve_browser_lane::LaneOptions::default())?;
    let result = async {
        lane.connect().await?;
        lane.open_url("https://mail.google.com/").await?;
        Ok::<bool, anyhow::Error>(!lane.page_text(200).await?.signed_out)
    }
    .await;
    let _ = lane.close().await;
    result
}

/// Open eve's Steel browser in a window, on the page given, for the owner to sign in. Sign-ins are kept in eve's
/// Steel profile (steel_local's DEFAULT_PROFILE); Enter closes the window and saves them.
fn run_steel_profile(url: &str, io: &Io) -> i32 {
    let cfg = eve_steel_local::configured();
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return report::<i32>(Err(e.into())),
    };
    let endpoint = match rt.block_on(eve_steel_local::start_session(&cfg, true)) {
        Ok(e) => e,
        Err(e) => {
            if let Some(steel) = e.downcast_ref::<eve_steel_local::SteelError>() {
                io.warn(&format!("eve-bridge: {steel}"));
                return 2;
            }
            return report::<i32>(Err(e));
        }
    };
    let opened = (|| -> anyhow::Result<()> {
        let mut driver = eve_browser_lane::cdp::CdpDriver::start()?;
        let browser = driver.connect_over_cdp(&endpoint, 30000)?;
        let context = browser
            .contexts()?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("eve's Steel browser has no window to open the page in"))?;
        let page = match context.pages()?.into_iter().next() {
            Some(p) => p,
            None => context.new_page()?,
        };
        let target = if url.contains("://") { url.to_string() } else { format!("https://{url}") };
        page.goto(&target, "domcontentloaded", 30000)?;
        io.say(&format!("eve's Steel browser is open for signing in (profile {}).", cfg.profile.display()));
        if io.ask("Sign in to each site eve may use, then press Enter here to save and close it. ").is_err() {
            io.say("");
        }
        Ok(())
    })();
    rt.block_on(eve_steel_local::release(&cfg));
    if let Err(e) = opened {
        return report::<i32>(Err(e));
    }
    io.say("Saved. Web tasks use this profile when EVE_BROWSER_STEEL=1.");
    0
}

/// Tell the running daemon to send cmd:unpair to the stick.
fn run_unpair(io: &Io) -> i32 {
    let Some(resp) = io.post(&json!({"evt": "unpair"}), None, 2.0) else {
        io.warn("eve-bridge: daemon not reachable. Start it with `eve-bridge daemon` (or via the launchd agent).");
        return 2;
    };
    if !truthy(get(&resp, "ok")) {
        io.warn(&format!("eve-bridge: unpair failed ({})", pstr(&py::get_or(&resp, "error", json!("unknown")))));
        return 2;
    }
    io.say("sent cmd:unpair to the stick; its stored bond is cleared.");
    io.say("");
    io.say("Next: open macOS System Settings > Bluetooth > Claude-5C66 > info");
    io.say("'Forget This Device' to purge the cached LTK. Then the next reconnect");
    io.say("will prompt for a fresh 6-digit passkey (displayed on the stick).");
    io.say("");
    io.say("Watch `tail -f ~/Library/Logs/eve-bridge.log` for the moment of truth:");
    io.say("  \"stick link: ENCRYPTED (was None)\"");
    0
}

// ---- the phone: link, notify, refresh, apps, places ----------------------------------------------------------

/// `link pair`: Eve Link's pairing link without Telegram's /stick (Telegram is app-only since 2026-10-08).
fn run_link(io: &Io) -> i32 {
    run_link_with(io, &eve_brain::notify::token_path(), &|token, base| eve_stick_link::latest_address(token, base), &|name| {
        eve_profile::pypath::get_exec_path(None)
            .iter()
            .map(|d| PathBuf::from(d).join(name))
            .find(|p| eve_profile::pypath::runnable(&p.to_string_lossy()))
    })
}

/// [`run_link`] with the token file, the rendezvous read and `shutil.which` given.
pub fn run_link_with(
    io: &Io,
    token_path: &Path,
    latest: &dyn Fn(&str, &str) -> String,
    which: &dyn Fn(&str) -> Option<PathBuf>,
) -> i32 {
    let token = match eve_stick_link::LinkToken::new(token_path).ensure() {
        Ok(t) => t,
        Err(e) => return report::<i32>(Err(e.into())),
    };
    let base = eve_stick_link::rendezvous_base();
    let base = if base.is_empty() { eve_stick_link::RENDEZVOUS_URL.to_string() } else { base };
    let address = latest(&token, &base);
    if address.is_empty() {
        io.say(
            "eve's address isn't posted yet. Is the daemon running with the Mini App's tunnel up? (It posts within a \
             minute of starting.)",
        );
        return 1;
    }
    let link = eve_stick_link::pair_url(&address, &token);
    io.say("Open this on your iPhone (AirDrop it to yourself, or scan the QR code), then tap Open in Eve Link:");
    io.say(&link);
    match which("qrencode") {
        Some(qr) => {
            let _ = std::process::Command::new(qr).args(["-t", "ansiutf8", &link]).status();
        }
        None => io.say("(For a QR code here: brew install qrencode)"),
    }
    io.say("The link holds the token: send it only to your own phone.");
    0
}

/// `notify topic` and `notify test` (notify.py). Prints the topic, never the link token.
fn run_notify(cmd: &str, io: &Io) -> i32 {
    run_notify_with(
        cmd,
        io,
        &eve_brain::notify::configured(None),
        &eve_brain::notify::read_token(None),
        &eve_brain::notify::default_post(),
    )
}

/// [`run_notify`] with the config, the token and the ntfy post given.
pub fn run_notify_with(
    cmd: &str,
    io: &Io,
    cfg: &eve_brain::notify::NotifyConfig,
    token: &str,
    post: &eve_brain::notify::PostFn,
) -> i32 {
    if token.is_empty() {
        io.say("Eve Link is not paired yet, so there is no topic. Pair the app first (docs/action-button.md).");
        return 1;
    }
    if cmd == "topic" {
        for line in eve_brain::notify::setup_lines(token, &cfg.base) {
            io.say(&line);
        }
        if !cfg.enabled {
            io.say("Notifications are off (EVE_NOTIFY=0).");
        }
        return 0;
    }
    if eve_brain::notify::post_once(token, eve_brain::notify::TEST_LINE, &cfg.base, post) {
        io.say("Posted a test notification. If the phone is subscribed, it shows now.");
        return 0;
    }
    io.say("Could not post the test notification (see the log line above).");
    1
}

/// `refresh status|remind|schedule|unschedule` (refresh.py).
fn run_refresh(cmd: &str, io: &Io) -> i32 {
    if cmd == "schedule" || cmd == "unschedule" {
        let launchd = match eve_service::Launchd::from_process() {
            Ok(l) => l,
            Err(e) => return report::<i32>(Err(e.into())),
        };
        return report(if cmd == "schedule" { eve_refresh::schedule(&launchd) } else { eve_refresh::unschedule(&launchd) });
    }
    let text = match eve_refresh::current(&eve_apps_mcp::apps_mcp::configured(None).google_token_path(), None) {
        Ok(t) => t,
        Err(e) => return report::<i32>(Err(e)),
    };
    io.say(&text);
    if cmd == "remind" {
        match eve_refresh::remind(&text) {
            Ok(true) => {}
            Ok(false) => {
                io.say("Shown on the Mac; the phone notification did not go out (notifications off or not paired).")
            }
            Err(e) => return report::<i32>(Err(e.into())),
        }
    }
    0
}

/// `webbrowser.open(url)`.
fn open_url(url: &str) {
    let program = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = std::process::Command::new(program)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// `apps login google` and `apps status` (apps_mcp.py). Prints no token, ever.
fn run_apps(cmd: &str, io: &Io) -> i32 {
    use eve_apps_mcp::apps_mcp as apps;
    let cfg = apps::configured(None);
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return report::<i32>(Err(e.into())),
    };
    if cmd == "login" {
        let say = |line: &str| println!("{line}");
        match rt.block_on(apps::google_login(&cfg, &open_url, 0, &say)) {
            Ok(scopes) => io.say(&format!("Signed in to Google. Scopes: {scopes}")),
            Err(e) => {
                // the reason, for the owner at the terminal
                io.say(&format!("Sign-in failed: {e}"));
                return 1;
            }
        }
        io.say("Restart the daemon to use it (launchctl kickstart -k gui/$(id -u)/com.github.eve-bridge.daemon).");
        return 0;
    }
    let mut bridge = apps::AppsBridge::new(cfg.clone());
    let signed: std::collections::HashSet<String> = bridge.available().into_iter().collect();
    rt.block_on(bridge.start());
    let mut counts: HashMap<String, usize> = HashMap::new();
    for t in bridge.tools() {
        let name = t.get("name").and_then(Value::as_str).unwrap_or("");
        *counts.entry(apps::split_name(name).0.to_string()).or_insert(0) += 1;
    }
    io.say(&format!("EVE_APPS={}", if cfg.enabled { "on" } else { "off" }));
    for (app, server) in apps::SERVERS.iter() {
        let state = if let Some(path) = bridge.paths.get(*app) {
            format!(
                "up via {path}, {} tools, {}",
                counts.get(*app).copied().unwrap_or(0),
                bridge.policy.get(*app).cloned().unwrap_or_else(|| "None".into())
            )
        } else if signed.contains(*app) {
            "signed in, but not answering (see the log above)".into()
        } else {
            "not signed in".into()
        };
        io.say(&format!("  {:16} {state}", server.label));
    }
    0
}

/// `eve-bridge places`: the places history in the terminal (places.py).
fn run_places(args: &Namespace, io: &Io) -> i32 {
    use eve_places::places;
    if !places::enabled(None) {
        io.say(places::OFF_REASON);
        return 1;
    }
    let store = places::Places::new(None, None, None);
    let cmd = args.str("places_cmd").unwrap_or_default();
    if cmd == "forget" {
        let summary = match store.summary() {
            Ok(s) => s,
            Err(e) => return report::<i32>(Err(anyhow::anyhow!("{e}"))),
        };
        if !args.flag("yes") {
            let n = |k: &str| pstr(summary.get(k).unwrap_or(&Value::Null));
            let answer = io
                .ask(&format!(
                    "Forget {} places, {} visits and {} things, and the phone's latest location? [y/N] ",
                    n("places"),
                    n("visits"),
                    n("things")
                ))
                .unwrap_or_default();
            if !["y", "yes"].contains(&eve_envfile::py_strip(&answer).to_lowercase().as_str()) {
                io.say("Nothing forgotten.");
                return 0;
            }
        }
        return match store.forget() {
            Ok(done) => {
                let v: Map<String, Value> = done.into_iter().map(|(k, n)| (k, json!(n))).collect();
                io.say(&py::dumps(&Value::Object(v), None, true));
                0
            }
            Err(e) => report::<i32>(Err(anyhow::anyhow!("{e}"))),
        };
    }
    if cmd == "summary" {
        return match store.summary() {
            Ok(s) => {
                io.say(&py::dumps(&Value::Object(s), Some(2), true));
                0
            }
            Err(e) => report::<i32>(Err(anyhow::anyhow!("{e}"))),
        };
    }
    let mut call = Map::new();
    call.insert("what".into(), json!(cmd));
    call.insert("start_date".into(), json!(args.str("day").unwrap_or_default()));
    call.insert("end_date".into(), json!(""));
    let out = places::call(places::RECALL, &call, Some(&store), None);
    let out = Value::Object(out);
    let text = get(&out, "text");
    io.say(&if truthy(text) { pstr(text) } else { py::dumps(&out, Some(2), false) });
    if truthy(get(&out, "ok")) {
        0
    } else {
        1
    }
}

/// The SUPPRESS marker, re-exported for the lite parser.
pub const HELP_SUPPRESS: &str = SUPPRESS;

#[cfg(test)]
mod tests;
