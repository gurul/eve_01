//! Cross-checks against the live Python module, when python3 and the bridge source are on this machine: the tool
//! schemas and the instructions byte for byte, the code path's words, and the file formats.

use std::path::PathBuf;
use std::process::Command as Proc;

use serde_json::{json, Value};

use eve_spotify as S;

fn bridge_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../bridge/src")
}

/// Runs `code` with eve_bridge importable; None when Python or the source is not here.
fn python(code: &str, input: &str) -> Option<String> {
    use std::io::Write as _;
    if !bridge_src().join("eve_bridge/spotify.py").exists() {
        return None;
    }
    let mut child = Proc::new("python3")
        .arg("-c")
        .arg(code)
        .env("PYTHONPATH", bridge_src())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(input.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        eprintln!("python failed: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

#[test]
fn tools_and_instructions_match_the_python() {
    let Some(out) = python(
        "import json; from eve_bridge import spotify as S; print(json.dumps({'tools': S.TOOLS, 'instructions': S.INSTRUCTIONS, \
         'not_set_up': S.NOT_SET_UP_LINE, 'off': S.OFF_REASON, 'scopes': S.SCOPES}))",
        "",
    ) else {
        return;
    };
    let py: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(py["tools"], Value::Array(S::TOOLS.clone()));
    assert_eq!(S::py_dumps(&py["tools"], None), S::py_dumps(&Value::Array(S::TOOLS.clone()), None));
    assert_eq!(py["instructions"], json!(S::INSTRUCTIONS));
    assert_eq!(py["not_set_up"], json!(S::NOT_SET_UP_LINE));
    assert_eq!(py["off"], json!(S::OFF_REASON));
    assert_eq!(py["scopes"], json!(S::SCOPES));
}

const PHRASES: &[&str] = &[
    "pause the music", "Stop music please", "hey eve, pause spotify", "play music", "resume spotify",
    "play music on Spotify", "next song", "skip this song", "previous track", "music volume 40",
    "spotify volume to 45%", "set the volume of the music to 30", "turn the music up", "music quieter",
    "what's playing?", "what song is this", "play the music on the kitchen speaker please", "move spotify to my phone",
    "play Daft Punk on Spotify", "pause", "skip", "next", "volume 40", "turn the volume down", "music volume 400",
    "what is this", "what's on", "pause the music in ten minutes", "play some jazz", "turn off the lights", "stop",
    "what's the weather", "spotify mode", "turn off spotify mode", "exit spotify mode", "what is spotify mode",
    "turn on spotify mode", "spotify mode off", "enter spotify mode", "go into spotify mode please",
    "Okay eve, could you skip the track on spotify thanks", "play the previous song", "go back a song",
    "music to 20%", "music at 120%", "louder music", "turn down the music", "the music off", "music on",
    "play the music on spotify", "send the music to the living room tv", "put my music onto kitchen",
    "play lofi beats for studying on spotify", "play the music again", "what am I listening to",
    "now playing", "which song is on", "What\u{2019}s playing on Spotify", "skip this", "unpause music",
    "transfer playback to the bedroom speaker eve", "play \"Blinding Lights\" on Spotify!", "  ", "",
    "play spotify on spotify", "move the music to spotify", "spotify volume 0", "music volume 100 %",
];

fn cmd_json(c: &Option<S::Command>) -> Value {
    match c {
        None => Value::Null,
        Some(c) => json!({"action": c.action, "query": c.query, "volume": c.volume, "change": c.change,
                          "device": c.device, "on": c.on}),
    }
}

#[test]
fn the_code_paths_words_match_the_python() {
    let input = serde_json::to_string(PHRASES).unwrap();
    let Some(out) = python(
        "import json, sys, dataclasses; from eve_bridge import spotify as S\n\
         out = []\n\
         for t in json.load(sys.stdin):\n    c = S.match(t)\n    out.append(dataclasses.asdict(c) if c else None)\n\
         print(json.dumps(out))",
        &input,
    ) else {
        return;
    };
    let py: Vec<Value> = serde_json::from_str(&out).unwrap();
    for (text, want) in PHRASES.iter().zip(py) {
        assert_eq!(cmd_json(&S::match_text(text)), want, "{text:?}");
    }
}

#[test]
fn the_login_file_reads_both_ways() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("spotify.json");
    let cfg: S::Dict = serde_json::from_value(json!({"client_id": "cid", "refresh_token": "r\u{e9}", "scopes": S::SCOPES})).unwrap();
    S::save(&cfg, Some(&path)).unwrap();
    let Some(out) = python(
        "import sys, json; from pathlib import Path; from eve_bridge import spotify as S\n\
         p = Path(sys.stdin.read())\n\
         d = S.load(p); before = p.read_text(); S.save(d, p); print(json.dumps([before == p.read_text(), d]))",
        path.to_str().unwrap(),
    ) else {
        return;
    };
    let py: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(py[0], json!(true), "Python rewrote the file differently");
    assert_eq!(py[1], Value::Object(cfg));
}
