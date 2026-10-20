//! The CLI: scan, list, name, wiz-import, set (`eve-bridge lights ...`).

use std::path::Path;

use clap::{Arg, ArgAction, Command as ClapCommand};
use serde_json::{json, Value};
use tracing::warn;

use crate::govee::govee_scan;
use crate::lights::{blocking, config_path, expand_user, load, parse_color, save, Change, Light, Lights};
use crate::py;
use crate::triones::triones_scan;
use crate::tuya::tuya_scan;
use crate::wiz::{wiz_import, wiz_scan};

/// Everything found, as Lights with default names. Never fails for one brand failing.
pub async fn scan(ble: bool, tuya: bool) -> Vec<Light> {
    let mut out = Vec::new();
    match blocking(|| govee_scan(3.0)).await {
        Ok(found) => {
            for d in found {
                let tag = if d.sku.is_empty() { py::tail(&d.device, 5) } else { d.sku.clone() };
                out.push(Light {
                    name: format!("govee {tag}").to_lowercase(),
                    kind: "govee".into(),
                    ip: d.ip,
                    device: d.device,
                    sku: d.sku,
                    ..Light::default()
                });
            }
        }
        Err(e) => warn!("lights: Govee scan failed ({e})"),
    }
    match blocking(|| wiz_scan(3.0)).await {
        Ok(found) => {
            for d in found {
                out.push(Light {
                    name: format!("wiz {}", py::tail(&d.device, 4)),
                    kind: "wiz".into(),
                    ip: d.ip,
                    device: d.device,
                    sku: d.sku,
                    ..Light::default()
                });
            }
        }
        Err(e) => warn!("lights: WiZ scan failed ({e})"),
    }
    if ble {
        match triones_scan(8.0).await {
            Ok(found) => {
                for d in found {
                    out.push(Light {
                        name: format!("happylighting {}", d.name).to_lowercase(),
                        kind: "triones".into(),
                        address: d.address,
                        ..Light::default()
                    });
                }
            }
            Err(e) => warn!("lights: Bluetooth scan failed ({e})"),
        }
    }
    if tuya {
        match tuya_scan(8).await {
            Ok(found) => {
                for d in found {
                    out.push(Light {
                        name: format!("tuya {}", py::tail(&d.id, 4)),
                        kind: "tuya".into(),
                        id: d.id,
                        ip: d.ip,
                        version: d.version,
                        ..Light::default()
                    });
                }
            }
            Err(e) => warn!("lights: Tuya scan failed ({e})"),
        }
    }
    out
}

/// Known lights keep their names, rooms and keys (a LAN light's new address is taken); new ones are added.
/// Returns (all, new); the new ones are at the end of all, in order.
pub fn merge(existing: Vec<Light>, found: Vec<Light>) -> (Vec<Light>, Vec<Light>) {
    let mut out = existing;
    let mut new = Vec::new();
    let same = |a: &Light, b: &Light| {
        a.kind == b.kind
            && ((!a.device.is_empty() && a.device == b.device)
                || (!a.address.is_empty() && a.address == b.address)
                || (!a.id.is_empty() && a.id == b.id))
    };
    for mut f in found {
        match out.iter_mut().find(|e| same(e, &f)) {
            None => {
                let mut n = f.name.clone();
                let mut i = 2;
                while out.iter().any(|e| e.name == n) {
                    n = format!("{} {i}", f.name);
                    i += 1;
                }
                f.name = n;
                new.push(f.clone());
                out.push(f);
            }
            Some(known) => {
                if !f.ip.is_empty() && matches!(f.kind.as_str(), "govee" | "wiz" | "tuya") {
                    known.ip = f.ip;
                }
            }
        }
    }
    (out, new)
}

/// argparse's `type=int`: Python's `int()` ("+5", " 50 ", "5_0" and "５０" are all numbers).
fn py_int(s: &str) -> Result<i64, String> {
    py::int_of(&Value::String(s.to_string())).map_err(|_| format!("invalid int value: {}", py::repr_str(s)))
}

/// The command line's clap definition, read-only: `eve-bridge` renders argparse's `--help` layout from it (the
/// app's own `cli` still parses). Added for the eve-bridge port.
pub fn clap_command() -> ClapCommand {
    parser()
}

pub(crate) fn parser() -> ClapCommand {
    ClapCommand::new("eve-bridge lights")
        .no_binary_name(true)
        .subcommand_required(true)
        // argparse takes a long option by any unique prefix ("--bright 40")
        .infer_long_args(true)
        .subcommand(
            ClapCommand::new("scan")
                .about("Find Govee, WiZ, HappyLighting and Tuya lights and add new ones to the lights file")
                .arg(
                    Arg::new("no-ble")
                        .long("no-ble")
                        .action(ArgAction::SetTrue)
                        .help("skip Bluetooth (a terminal without Bluetooth permission)"),
                )
                .arg(Arg::new("no-tuya").long("no-tuya").action(ArgAction::SetTrue)),
        )
        .subcommand(ClapCommand::new("list").about("The lights and their state"))
        .subcommand(
            ClapCommand::new("name")
                .about("Rename a light, and set its room or Tuya key")
                .arg(Arg::new("old").required(true))
                .arg(Arg::new("new").required(true))
                .arg(Arg::new("room").long("room"))
                .arg(Arg::new("key").long("key").help("a Tuya light's local key")),
        )
        .subcommand(
            ClapCommand::new("wiz-import")
                .about("Give WiZ lights their home's signing key, from the WiZ app's local-integration export (a JSON file)")
                .arg(Arg::new("file").required(true)),
        )
        .subcommand(
            ClapCommand::new("set")
                .about("Change lights now")
                .arg(Arg::new("target").required(true))
                .arg(Arg::new("power").value_parser(["on", "off"]))
                .arg(
                    Arg::new("brightness")
                        .long("brightness")
                        .allow_negative_numbers(true)
                        .value_parser(py_int),
                )
                .arg(Arg::new("color").long("color")),
        )
}

/// The Python exception name a failed read of the export file raises.
fn export_error(e: &std::io::Error) -> &'static str {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => "FileNotFoundError",
        PermissionDenied => "PermissionError",
        IsADirectory => "IsADirectoryError",
        InvalidData => "UnicodeDecodeError",
        _ => "OSError",
    }
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread().enable_all().build()
}

/// `eve-bridge lights scan [--no-ble] [--no-tuya] | list | name OLD NEW [--room R] [--key K] | wiz-import FILE |
/// set TARGET [on|off] [--brightness N] [--color C]`. `argv` is the arguments after `lights`; `out` gets each
/// line. Returns the exit code. Runs its own async runtime: call it from outside one.
pub fn cli(argv: &[String], path: Option<&Path>, out: &mut dyn FnMut(&str)) -> i32 {
    let a = match parser().try_get_matches_from(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return e.exit_code();
        }
    };
    let cfg = match path {
        Some(p) => expand_user(p),
        None => config_path(),
    };
    // clap refuses a command line without one (subcommand_required), as argparse does
    let Some((act, sub)) = a.subcommand() else { return 2 };
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => {
            out(&format!("Could not start: {e}"));
            return 1;
        }
    };

    if act == "scan" {
        let found = rt.block_on(scan(!sub.get_flag("no-ble"), !sub.get_flag("no-tuya")));
        let existing = load(Some(&cfg));
        let known = existing.len();
        let (merged, new) = merge(existing, found);
        if let Err(e) = save(&merged, Some(&cfg)) {
            out(&format!("Could not write {}: {}", cfg.display(), e));
            return 1;
        }
        for (i, lt) in merged.iter().enumerate() {
            let mut line = format!("{}{}  [{}]", if i >= known { "new  " } else { "     " }, lt.name, lt.kind);
            if !lt.room.is_empty() {
                line += &format!("  room: {}", lt.room);
            }
            if lt.kind == "tuya" && lt.key.is_empty() {
                line += "  (needs --key)";
            }
            out(&line);
        }
        out(&format!(
            "{} new, {} in {}. Rename with: eve-bridge lights name OLD NEW --room ROOM",
            new.len(),
            merged.len(),
            cfg.display()
        ));
        return 0;
    }
    let mut lights = load(Some(&cfg));
    if lights.is_empty() {
        out(&format!("No lights in {}. Run: eve-bridge lights scan", cfg.display()));
        return 1;
    }
    if act == "wiz-import" {
        let file = sub.get_one::<String>("file").cloned().unwrap_or_default();
        let done = std::fs::read_to_string(expand_user(Path::new(&file)))
            .map_err(|e| export_error(&e))
            .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|_| "JSONDecodeError"))
            .and_then(|export| wiz_import(&export, &mut lights));
        let done = match done {
            Ok(done) => done,
            Err(kind) => {
                out(&format!("Could not read a WiZ export from {file}: {kind}"));
                return 1;
            }
        };
        if done.is_empty() {
            out("No WiZ light in the lights file matches that export. Run: eve-bridge lights scan");
            return 1;
        }
        if let Err(e) = save(&lights, Some(&cfg)) {
            out(&format!("Could not write {}: {}", cfg.display(), e));
            return 1;
        }
        out(&format!("Signing key set for: {}. Restart the daemon to use it.", done.join(", ")));
        return 0;
    }
    if act == "name" {
        let old = sub.get_one::<String>("old").cloned().unwrap_or_default();
        let hub = Lights::new(lights);
        let (matched, why) = hub.resolve(&old);
        if matched.len() != 1 {
            let names: Vec<&str> = matched.iter().map(|lt| lt.name.as_str()).collect();
            out(&if why.is_empty() {
                format!("{} is more than one light: {}", py::repr_str(&old), names.join(", "))
            } else {
                why
            });
            return 1;
        }
        let at = hub.lights.iter().position(|lt| std::ptr::eq(lt, matched[0])).unwrap_or(0);
        let mut lights = hub.lights;
        let lt = &mut lights[at];
        lt.name = sub.get_one::<String>("new").map(|s| py::strip(s).to_string()).unwrap_or_default();
        if let Some(room) = sub.get_one::<String>("room") {
            lt.room = py::strip(room).to_string();
        }
        if let Some(key) = sub.get_one::<String>("key") {
            lt.key = py::strip(key).to_string();
        }
        let line = format!("{}  [{}]", lt.name, lt.kind) + &if lt.room.is_empty() {
            String::new()
        } else {
            format!("  room: {}", lt.room)
        };
        if let Err(e) = save(&lights, Some(&cfg)) {
            out(&format!("Could not write {}: {}", cfg.display(), e));
            return 1;
        }
        out(&line);
        return 0;
    }

    let result = rt.block_on(async {
        let hub = Lights::new(lights);
        let result = if act == "list" {
            hub.status("all").await
        } else {
            let color_arg = sub.get_one::<String>("color").cloned();
            let color = color_arg.as_deref().filter(|c| !c.is_empty()).and_then(parse_color);
            match color_arg.as_deref() {
                Some(c) if !c.is_empty() && color.is_none() => {
                    json!({"ok": false, "reason": format!("unknown colour {}", py::repr_str(c))})
                }
                _ => {
                    let change = Change {
                        power: sub.get_one::<String>("power").map(|p| p == "on"),
                        brightness: sub.get_one::<i64>("brightness").copied(),
                        color,
                    };
                    let target = sub.get_one::<String>("target").cloned().unwrap_or_default();
                    hub.set(&target, &change).await
                }
            }
        };
        hub.close().await;
        result
    });
    out(&py::dumps_indent(&result, 2));
    if result.get("ok").map(py::truthy).unwrap_or(false) {
        0
    } else {
        1
    }
}
