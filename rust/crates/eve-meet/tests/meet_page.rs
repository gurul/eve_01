//! tests/test_meet_page.py: meet_page.js, run for real: headless Chromium, the real script, fixture pages shaped
//! like Meet's.
//!
//! Every fixture button counts its own clicks, so each test can say not only what the script pressed but that it
//! pressed nothing else. The fixtures use the labels Meet uses (aria-label, as OpenClaw's plugin reads them).
//! Skipped (with a line) where Playwright's Chromium is not installed, as the Python file skips.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use eve_browser_lane::cdp::{find_chromium, CdpDriver};
use eve_browser_lane::driver::{ContextHandle, Driver, PageHandle};
use eve_meet::{CaptionMerger, PAGE_SCRIPT};
use regex::Regex;
use serde_json::{json, Map, Value};

/// One Chromium at a time across the tests in this file.
static ONE: Mutex<()> = Mutex::new(());

struct Browser {
    _one: MutexGuard<'static, ()>,
    driver: CdpDriver,
    _context: Arc<dyn ContextHandle>,
    page: Box<dyn PageHandle>,
    dir: tempfile::TempDir,
    n: usize,
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.driver.stop();
    }
}

fn browser() -> Option<Browser> {
    if find_chromium(true).is_none() {
        eprintln!("skipped: no Chromium for Playwright");
        return None;
    }
    let one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut driver = CdpDriver::new();
    let context = driver.launch_persistent_context(&dir.path().join("profile"), true, (1280, 720), &[]).unwrap();
    let page = context.new_page().unwrap();
    Some(Browser { _one: one, driver, _context: context, page, dir, n: 0 })
}

// A button that counts its clicks; a toggle flips its label between "Turn off X" and "Turn on X".
const HARNESS: &str = r#"
<script>
window.__hits = {};
function hit(label) { window.__hits[label] = (window.__hits[label] || 0) + 1; }
document.addEventListener('click', (e) => {
  const b = e.target.closest('button, [role=button]');
  if (!b) return;
  const label = b.getAttribute('aria-label') || b.textContent.trim();
  hit(label);
  const m = label.match(/^Turn (off|on) (microphone|camera|captions)$/);
  if (m) b.setAttribute('aria-label', 'Turn ' + (m[1] === 'off' ? 'on' : 'off') + ' ' + m[2]);
  if (label === 'Join now' || label === 'Join here too') document.body.dataset.joined = '1';
}, true);
</script>
"#;

impl Browser {
    /// `page.set_content(...)`: a fresh document holding `body` and the harness.
    fn load(&mut self, body: &str) {
        self.n += 1;
        let f = self.dir.path().join(format!("page{}.html", self.n));
        std::fs::write(&f, format!("<html><body>{body}{HARNESS}</body></html>")).unwrap();
        self.page.goto(&format!("file://{}", f.display()), "load", 10_000).unwrap();
    }

    fn run(&self, opts: Value) -> Map<String, Value> {
        let mut all = json!({"act": true});
        all.as_object_mut().unwrap().extend(opts.as_object().cloned().unwrap_or_default());
        self.eval(&all)
    }

    fn eval(&self, opts: &Value) -> Map<String, Value> {
        self.page.evaluate(PAGE_SCRIPT, Some(opts)).unwrap().as_object().cloned().unwrap()
    }

    fn hits(&self) -> BTreeMap<String, i64> {
        let v = self.page.evaluate("() => window.__hits", None).unwrap();
        v.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_i64().unwrap())).collect()
    }

    fn js(&self, expr: &str) {
        self.page.evaluate(&format!("() => {{ {expr}; }}"), None).unwrap();
    }
}

fn btn(label: &str) -> String {
    format!(r#"<button aria-label="{label}"><i>icon</i></button>"#)
}

fn clicked(st: &Map<String, Value>) -> Vec<String> {
    st["clicked"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

fn hits_of(pairs: &[(&str, i64)]) -> BTreeMap<String, i64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

fn on(st: &Map<String, Value>, key: &str) -> bool {
    st.get(key).is_some_and(|v| match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        _ => true,
    })
}

#[test]
fn test_prejoin_turns_mic_and_camera_off_before_it_joins() {
    let Some(mut b) = browser() else { return };
    b.load(&format!("{}{}<button>Join now</button>", btn("Turn off microphone"), btn("Turn off camera")));
    let first = b.run(json!({}));
    assert_eq!(clicked(&first), ["mic-off", "camera-off"]);
    assert!(!on(&first, "joinClicked")); // never in the same pass as turning one off
    let second = b.run(json!({}));
    assert!(clicked(&second) == ["join"] && on(&second, "joinClicked"));
    assert_eq!(b.hits(), hits_of(&[("Turn off microphone", 1), ("Turn off camera", 1), ("Join now", 1)]));
}

#[test]
fn test_no_toggle_yet_is_not_off() {
    let Some(mut b) = browser() else { return };
    b.load("<button>Join now</button>");
    assert!(clicked(&b.run(json!({}))).is_empty()); // the preview's toggles have not rendered
    assert_eq!(clicked(&b.run(json!({"noControlsOk": true}))), ["join"]); // meet allows it after NO_CONTROLS_SECS
}

#[test]
fn test_ask_to_join_is_pressed_when_already_dark() {
    let Some(mut b) = browser() else { return };
    b.load(&format!("{}{}<button>Ask to join</button>", btn("Turn on microphone"), btn("Turn on camera")));
    assert_eq!(clicked(&b.run(json!({}))), ["join"]);
}

#[test]
fn test_the_same_account_elsewhere_gets_join_here_too_never_switch_here() {
    let Some(mut b) = browser() else { return };
    b.load(&format!(
        "{}{}<button>Switch here</button><button>Join here too</button><div>You are in this call on another device</div>",
        btn("Turn on microphone"),
        btn("Turn on camera")
    ));
    let st = b.run(json!({}));
    assert!(on(&st, "joinHereToo") && on(&st, "switchHere"));
    assert_eq!(clicked(&st), ["join-here-too"]);
    let hits = b.hits();
    assert_eq!(hits.get("Switch here").copied().unwrap_or(0), 0);
    assert_eq!(hits["Join here too"], 1);
}

#[test]
fn test_only_switch_here_offered_presses_nothing() {
    let Some(mut b) = browser() else { return };
    b.load(&format!(
        "{}{}<button>Switch here</button><button>Use Companion mode</button>",
        btn("Turn on microphone"),
        btn("Turn on camera")
    ));
    for _ in 0..3 {
        assert!(clicked(&b.run(json!({"noControlsOk": true}))).is_empty());
    }
    assert!(b.hits().is_empty());
}

#[test]
fn test_the_microphone_prompt_is_answered_without_it() {
    let Some(mut b) = browser() else { return };
    b.load(
        "<div>Do you want people to hear you in the meeting?</div>\
         <button>Use microphone</button><button>Continue without microphone</button>",
    );
    let st = b.run(json!({}));
    assert!(clicked(&st).contains(&"no-mic".to_string()));
    assert_eq!(b.hits().get("Use microphone").copied().unwrap_or(0), 0);
}

#[test]
fn test_in_call_the_mic_is_muted_again_and_captions_go_on() {
    let Some(mut b) = browser() else { return };
    b.load(&format!("{}{}{}{}", btn("Leave call"), btn("Turn off microphone"), btn("Turn on camera"), btn("Turn on captions")));
    let st = b.run(json!({"captions": true}));
    assert!(on(&st, "inCall"));
    assert_eq!(clicked(&st), ["mic-off", "captions-on"]);
    assert_eq!(b.hits().get("Turn on camera").copied().unwrap_or(0), 0);
}

#[test]
fn test_lobby_and_ended_and_sign_in_are_read() {
    let Some(mut b) = browser() else { return };
    b.load(&format!(
        "<div>Asking to be let in…</div><div>You'll join the call when someone lets you in</div>{}{}",
        btn("Turn on microphone"),
        btn("Turn on camera")
    ));
    let st = b.run(json!({}));
    assert!(on(&st, "lobby") && !on(&st, "inCall") && clicked(&st).is_empty());
    b.load("<div>You left the meeting</div><button>Rejoin</button><button>Return to home screen</button>");
    assert_eq!(b.run(json!({}))["ended"], "you left the meeting");
    b.load("<div>No one responded to your request to join the call</div>");
    assert!(b.run(json!({}))["ended"].as_str().unwrap().contains("no one responded"));
    b.load("<div>Choose an account to continue to Google Meet</div><button>Join now</button>");
    let st = b.run(json!({"noControlsOk": true}));
    assert!(on(&st, "signIn") && clicked(&st).is_empty());
}

#[test]
fn test_leave_presses_leave_call_then_the_confirmation() {
    let Some(mut b) = browser() else { return };
    b.load(&format!("{}{}", btn("Leave call"), btn("Turn on microphone")));
    assert_eq!(clicked(&b.run(json!({"leave": true}))), ["leave"]);
    b.load(&format!("{}<button>Leave meeting</button>", btn("Leave call")));
    assert_eq!(clicked(&b.run(json!({"leave": true}))), ["leave-confirm"]);
}

#[test]
fn test_act_false_reads_without_touching() {
    let Some(mut b) = browser() else { return };
    b.load(&format!("{}{}<button>Join now</button>", btn("Turn off microphone"), btn("Turn off camera")));
    let st = b.eval(&json!({"act": false}));
    assert!(on(&st, "micOn") && on(&st, "camOn") && clicked(&st).is_empty() && b.hits().is_empty());
}

// Meet's caption region as it rendered live (2026-09-25, trimmed of classes): one block per speaker turn,
// the name in a span two divs down, the words in one div as several text nodes, a hidden div and the
// "Jump to bottom" button beside the blocks. Plus eve's own tile (data-is-self) and a name with no words yet.
const CAPTIONS: &str = r#"
<div tabindex="0" role="region" aria-label="Captions">
  <div class="nMcdL" id="a"><div class="adE6rb"><div class="KcIKyf"><span class="NWpY1d">Alice Chen</span></div></div><div class="ygicle words">We should </div></div>
  <div class="nMcdL" id="b"><div class="adE6rb"><div class="KcIKyf"><span class="NWpY1d">Bob</span></div></div><div class="ygicle words">Sounds good</div></div>
  <div class="nMcdL" id="c" data-is-self="true"><div class="adE6rb"><div class="KcIKyf"><span class="NWpY1d">You</span></div></div><div class="ygicle words">test test</div></div>
  <div class="nMcdL" id="d"><div class="adE6rb"><div class="KcIKyf"><span class="NWpY1d">Carol</span></div></div><div class="ygicle words"></div></div>
  <div><div class="GvZY2" style="display: none;"></div></div>
  <div class="IMKgW"><div data-is-touch-wrapper="true"><button aria-label="Jump to most recent captions"><span><i aria-hidden="true">arrow_downward</i></span><span aria-hidden="true">Jump to bottom</span></button></div></div>
</div>
<script>document.querySelector('#a .words').append('ship ', 'on');</script>
"#;

fn in_call_with_captions() -> String {
    format!("{}{}{}{}{CAPTIONS}", btn("Leave call"), btn("Turn on microphone"), btn("Turn on camera"), btn("Turn off captions"))
}

fn rows(st: &Map<String, Value>) -> Vec<Value> {
    st["snaps"].as_array().unwrap().last().unwrap()["rows"].as_array().unwrap().clone()
}

#[test]
fn test_caption_blocks_are_recorded_with_speakers_and_self_flagged() {
    let Some(mut b) = browser() else { return };
    b.load(&in_call_with_captions());
    let st = b.run(json!({"captions": true}));
    let first = rows(&st);
    let seen: Vec<(String, String, bool)> = first
        .iter()
        .map(|r| (r["speaker"].as_str().unwrap().into(), r["text"].as_str().unwrap().into(), r["self"].as_bool().unwrap()))
        .collect();
    assert_eq!(
        seen,
        [("Alice Chen".into(), "We should ship on".into(), false), ("Bob".into(), "Sounds good".into(), false),
         ("You".into(), "test test".into(), true)]
    );
    b.js("document.querySelector('#a .words').append(' Friday')");
    b.page.wait_for_timeout(400); // the observer records on its own
    let st2 = b.run(json!({"captions": true, "since": st["seq"]}));
    assert!(!st2["snaps"].as_array().unwrap().is_empty());
    let second = rows(&st2);
    assert_eq!(second[0]["text"], "We should ship on Friday");
    assert_eq!(second[0]["id"], first[0]["id"]); // the same block keeps its id
    let seq = st["seq"].as_f64().unwrap();
    assert!(st2["snaps"].as_array().unwrap().iter().all(|s| s["seq"].as_f64().unwrap() > seq));
}

#[test]
fn test_captions_feed_the_merger_end_to_end() {
    let Some(mut b) = browser() else { return };
    b.load(&in_call_with_captions());
    let mut merger = CaptionMerger::new();
    let mut lines = Vec::new();
    let st = b.run(json!({"captions": true}));
    for s in st["snaps"].as_array().unwrap() {
        lines.extend(merger.feed(s).unwrap());
    }
    b.js("document.querySelector('#a .words').append(' Friday')");
    b.page.wait_for_timeout(400);
    for s in b.run(json!({"captions": true, "since": st["seq"]}))["snaps"].as_array().unwrap() {
        lines.extend(merger.feed(s).unwrap());
    }
    lines.extend(merger.finish());
    let got: Vec<(String, String)> = lines.iter().map(|l| (l.speaker.clone(), l.text.clone())).collect();
    assert_eq!(got, [("Alice Chen".to_string(), "We should ship on Friday".to_string()), ("Bob".into(), "Sounds good".into())]);
}

fn safe_block() -> &'static str {
    PAGE_SCRIPT.split_once("const SAFE = {").unwrap().1.split_once("};").unwrap().0
}

/// Listen only: across every page shape, the script's click log holds only the safe names, and no control
/// that could unmute, show video, present or take the call from another device is ever pressed.
#[test]
fn test_only_safe_clicks_across_every_fixture() {
    let Some(mut b) = browser() else { return };
    let danger = ["Turn on microphone", "Turn on camera", "Switch here", "Use Companion mode", "Present now",
                  "Use microphone", "Unmute"];
    let shapes = [
        format!("{}{}<button>Join now</button>", btn("Turn off microphone"), btn("Turn off camera")),
        format!("{}{}<button>Switch here</button><button>Join here too</button>", btn("Turn on microphone"), btn("Turn on camera")),
        format!("{}{}{}{}{CAPTIONS}", btn("Leave call"), btn("Turn on microphone"), btn("Turn on camera"), btn("Turn on captions")),
        "<div>Do you want people to hear you?</div><button>Use microphone</button><button>Continue without microphone</button>".into(),
        format!("<div>Asking to be let in</div>{}{}", btn("Turn on microphone"), btn("Turn on camera")),
    ];
    let extra: String = danger.iter().map(|d| format!("<button>{d}</button>")).collect();
    let name = Regex::new(r#"(?m)^\s*"([a-z-]+)":"#).unwrap();
    let safe: BTreeSet<String> = name.captures_iter(safe_block()).map(|c| c[1].to_string()).collect();
    for shape in &shapes {
        b.load(&format!("{shape}{extra}"));
        for opts in [json!({}), json!({"noControlsOk": true}), json!({"captions": true}), json!({"leave": true})] {
            let st = b.run(opts);
            assert!(clicked(&st).iter().all(|c| safe.contains(c)), "{:?}", clicked(&st));
        }
        let pressed = b.hits();
        assert!(!danger.iter().any(|d| pressed.get(*d).copied().unwrap_or(0) > 0), "{pressed:?}");
    }
}

/// The same rule, read off the script: one element click in the whole file (inside click(), which refuses a
/// name outside SAFE), every click("…") names a SAFE entry, and no SAFE pattern can match an unmute or a
/// camera-on label. Positive control: the check does find the one real `b.click()`.
#[test]
fn test_only_safe_by_reading_the_source() {
    let code: String = PAGE_SCRIPT.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    assert_eq!(code.matches(".click()").count(), 1);
    assert!(code.contains("b.click();"));
    let entry = Regex::new(r#"(?m)^\s*"([a-z-]+)":\s*(/.+/i),?$"#).unwrap();
    let safe: BTreeMap<String, String> = entry.captures_iter(safe_block()).map(|c| (c[1].to_string(), c[2].to_string())).collect();
    let names: BTreeSet<&str> = safe.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        BTreeSet::from(["mic-off", "camera-off", "no-mic", "join", "join-here-too", "captions-on", "leave-confirm", "leave"])
    );
    let call = Regex::new(r#"click\("([a-z-]+)"\)"#).unwrap();
    let called: BTreeSet<String> = call.captures_iter(&code).map(|c| c[1].to_string()).collect();
    assert!(!called.is_empty() && called.iter().all(|c| safe.contains_key(c)));
    for bad in ["Turn on microphone", "Turn on camera", "Switch here", "Unmute", "Use microphone", "Present now"] {
        for (name, pattern) in &safe {
            let body = &pattern[1..pattern.len() - 2];
            let re = Regex::new(&format!("(?i){body}")).unwrap();
            assert!(!re.is_match(bad), "{name} {bad}");
        }
    }
    // the Rust module never writes the words that would turn a device on, outside a comment or a "never"
    let rust = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/meet.rs"));
    let danger = Regex::new(r"(?i)turn on (microphone|camera)|unmute\(").unwrap();
    for line in rust.lines() {
        let t = line.trim_start();
        assert!(
            !danger.is_match(line) || t.starts_with("//") || t.starts_with('"') || line.to_lowercase().contains("never"),
            "{line}"
        );
    }
    // positive control: the same check catches such a line
    assert!(danger.is_match("page.click(\"Turn on microphone\")"));
}

/// ChromeMeetPage on a lane of its own: before the tab exists the page reads as closed (the owner leaving), and
/// once open the real script runs in it through the lane's thread.
#[test]
fn chrome_meet_page_runs_the_script_on_its_own_lane() {
    use eve_browser_lane::{BrowserLane, BrowserLaneConfig, LaneOptions};
    use eve_meet::{ChromeMeetPage, MeetPage};
    let Some(b) = browser() else { return };
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("meet.html");
    std::fs::write(&f, format!("<html><body>{}{}<button>Join now</button>{HARNESS}</body></html>",
                               btn("Turn off microphone"), btn("Turn off camera"))).unwrap();
    let cfg = BrowserLaneConfig { profile: dir.path().join("profile"), headless: true, ..Default::default() };
    let page = ChromeMeetPage::new(BrowserLane::new(cfg, LaneOptions::default()).unwrap(), None);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        assert_eq!(Value::Object(page.run(json!({"act": true})).await.unwrap()), json!({"closed": true}));
        page.open(&format!("file://{}", f.display())).await.unwrap();
        let st = page.run(json!({"act": true})).await.unwrap();
        assert_eq!(clicked(&st), ["mic-off", "camera-off"]);
        assert_eq!(clicked(&page.run(json!({"act": true})).await.unwrap()), ["join"]);
        page.close().await.unwrap();
    });
    drop(b);
}
