//! Port of bridge/tests/test_chrome_consent.py: the owner's Telegram yes/no answers Chrome's
//! "Allow remote debugging?".

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use super::*;

type Log = Arc<Mutex<Vec<String>>>;

fn log() -> Log {
    Arc::new(Mutex::new(Vec::new()))
}

fn opts(showing: Showing, pressing: Pressing) -> BrokerOptions {
    BrokerOptions { showing, pressing, ..BrokerOptions::default() }
}

fn ask_fn(f: impl Fn(String) -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync + 'static) -> AskOwner {
    Arc::new(f)
}

#[derive(Default)]
struct Rig {
    showing_after: usize,
    never_shows: bool,
    no_telegram: bool,
    ask_raises: bool,
}

/// `reply` None: the owner never answers.
fn rig(reply: Option<&str>, r: Rig) -> (ConsentBroker, Log, Log) {
    let pressed = log();
    let asked = log();
    let polls = Arc::new(AtomicUsize::new(0));
    let showing: Showing = {
        let pressed = pressed.clone();
        let never_shows = r.never_shows;
        let after = r.showing_after;
        Arc::new(move || {
            let n = polls.fetch_add(1, Ordering::SeqCst) + 1;
            let shows = !never_shows && pressed.lock().is_empty() && n > after; // a press dismisses it
            Box::pin(async move { shows })
        })
    };
    let pressing: Pressing = {
        let pressed = pressed.clone();
        Arc::new(move |label: String| {
            pressed.lock().push(label);
            Box::pin(async { true })
        })
    };
    let ask = {
        let asked = asked.clone();
        let reply = reply.map(str::to_string);
        let raises = r.ask_raises;
        ask_fn(move |q: String| {
            asked.lock().push(q);
            let reply = reply.clone();
            Box::pin(async move {
                if raises {
                    anyhow::bail!("telegram down");
                }
                match reply {
                    Some(reply) => Ok(reply),
                    None => {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        Ok(String::new())
                    }
                }
            })
        })
    };
    let broker = ConsentBroker::new(
        if r.no_telegram { None } else { Some(ask) },
        BrokerOptions {
            appear_secs: 0.05,
            ask_timeout_secs: 0.05,
            poll_secs: 0.01,
            settle_secs: 0.001,
            ..opts(showing, pressing)
        },
    );
    (broker, pressed, asked)
}

fn got(l: &Log) -> Vec<String> {
    l.lock().clone()
}

fn strs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn test_a_clear_yes_presses_allow() {
    for reply in ["yes", "Yes please", "ok", "allow", "go ahead"] {
        let (broker, pressed, asked) = rig(Some(reply), Rig { showing_after: 2, ..Rig::default() });
        assert_eq!(broker.answer_own_connection().await, "allowed", "{reply}");
        assert_eq!(got(&pressed), strs(&["Allow"]));
        assert_eq!(got(&asked), strs(&[QUESTION]));
        assert_eq!(broker.last(), "allowed");
    }
}

#[tokio::test]
async fn test_anything_else_presses_cancel() {
    for reply in ["no", "nope", "yeah no", "ok wait", "maybe", ""] {
        let (broker, pressed, _) = rig(Some(reply), Rig::default());
        assert_eq!(broker.answer_own_connection().await, "declined", "{reply:?}");
        assert_eq!(got(&pressed), strs(&["Cancel"]));
    }
}

#[tokio::test]
async fn test_silence_and_a_broken_telegram_press_cancel() {
    let (broker, pressed, _) = rig(None, Rig::default());
    assert_eq!(broker.answer_own_connection().await, "unanswered");
    assert_eq!(got(&pressed), strs(&["Cancel"]));
    let (broker, pressed, _) = rig(Some("yes"), Rig { ask_raises: true, ..Rig::default() });
    assert_eq!(broker.answer_own_connection().await, "unanswered");
    assert_eq!(got(&pressed), strs(&["Cancel"]));
}

#[tokio::test]
async fn test_no_way_to_ask_presses_cancel_without_asking() {
    let (broker, pressed, asked) = rig(Some("yes"), Rig { no_telegram: true, ..Rig::default() });
    assert_eq!(broker.answer_own_connection().await, "no_way_to_ask");
    assert_eq!(got(&pressed), strs(&["Cancel"]));
    assert!(got(&asked).is_empty());
}

#[tokio::test]
async fn test_no_dialog_means_nothing_is_pressed_or_asked() {
    let (broker, pressed, asked) = rig(Some("yes"), Rig { never_shows: true, ..Rig::default() });
    assert_eq!(broker.answer_own_connection().await, "no_dialog");
    assert!(got(&pressed).is_empty() && got(&asked).is_empty());
}

#[tokio::test]
async fn test_only_allow_and_cancel_can_be_pressed() {
    assert_eq!(press("Turn off in settings").await, Err(InvalidLabel("Turn off in settings".into())));
}

/// Python's `_broker`: `showing_seq` is popped one look at a time, its last value repeating.
fn broker_with(showing_seq: &[bool], reply: &str, press_ok: bool, slow_reply: bool) -> (ConsentBroker, Log, Log) {
    let told = log();
    let pressed = log();
    let seq = Arc::new(Mutex::new(showing_seq.to_vec()));
    let showing: Showing = {
        let pressed = pressed.clone();
        Arc::new(move || {
            let shows = if !pressed.lock().is_empty() && press_ok {
                false // a press that landed dismisses the dialog
            } else {
                let mut seq = seq.lock();
                if seq.len() > 1 {
                    seq.remove(0)
                } else {
                    seq[0]
                }
            };
            Box::pin(async move { shows })
        })
    };
    let pressing: Pressing = {
        let pressed = pressed.clone();
        Arc::new(move |label: String| {
            pressed.lock().push(label);
            Box::pin(async move { press_ok })
        })
    };
    let reply = reply.to_string();
    let ask = ask_fn(move |_q| {
        let reply = reply.clone();
        Box::pin(async move {
            if slow_reply {
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            Ok(reply)
        })
    });
    let tell: TellOwner = {
        let told = told.clone();
        Arc::new(move |text: String| {
            told.lock().push(text);
            Box::pin(async { Ok(()) })
        })
    };
    let b = ConsentBroker::new(
        Some(ask),
        BrokerOptions {
            tell_owner: Some(tell),
            appear_secs: 1.0,
            ask_timeout_secs: 2.0,
            poll_secs: 0.01,
            settle_secs: 0.001,
            ..opts(showing, pressing)
        },
    );
    (b, pressed, told)
}

/// BrowserLane.connect cancels the broker when the connection is done; the cancellation skipped the cleanup,
/// the Telegram question stayed pending, and the owner's next message was swallowed as its answer.
#[tokio::test]
async fn test_a_connection_that_finishes_first_withdraws_the_phone_question() {
    struct Withdrawn(Arc<AtomicBool>);
    impl Drop for Withdrawn {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let asked = Arc::new(tokio::sync::Notify::new());
    let withdrawn = Arc::new(AtomicBool::new(false));
    let ask = {
        let (asked, withdrawn) = (asked.clone(), withdrawn.clone());
        ask_fn(move |_q| {
            let (asked, withdrawn) = (asked.clone(), withdrawn.clone());
            Box::pin(async move {
                let _guard = Withdrawn(withdrawn);
                asked.notify_one();
                futures::future::pending::<()>().await;
                Ok(String::new())
            })
        })
    };
    let showing: Showing = Arc::new(|| Box::pin(async { true }));
    let pressing: Pressing = Arc::new(|_l: String| Box::pin(async { true }));
    let b = ConsentBroker::new(
        Some(ask),
        BrokerOptions {
            appear_secs: 1.0,
            ask_timeout_secs: 60.0,
            poll_secs: 0.001,
            settle_secs: 0.001,
            ..opts(showing, pressing)
        },
    );
    let answering = tokio::spawn(async move { b.answer_own_connection().await });
    asked.notified().await;
    answering.abort(); // as connect() does once the connection is up
    let err = answering.await.expect_err("the answer was cancelled");
    assert!(err.is_cancelled());
    for _ in 0..1000 {
        if withdrawn.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(withdrawn.load(Ordering::SeqCst));
}

/// A fake AX tree: nodes are indices into an arena of attribute maps.
struct FakeAx {
    nodes: Vec<HashMap<&'static str, AxValue<usize>>>,
    apps: HashMap<i32, usize>,
}

impl Ax for FakeAx {
    type Node = usize;
    fn application(&self, pid: i32) -> usize {
        self.apps[&pid]
    }
    fn attr(&self, el: &usize, name: &str) -> Option<AxValue<usize>> {
        self.nodes[*el].get(name).cloned()
    }
    fn perform(&self, _el: &usize, _action: &str) -> i32 {
        0
    }
}

fn node(attrs: &[(&'static str, AxValue<usize>)]) -> HashMap<&'static str, AxValue<usize>> {
    attrs.iter().cloned().collect()
}

fn s(x: &str) -> AxValue<usize> {
    AxValue::Str(x.to_string())
}

fn kids(ids: &[usize]) -> AxValue<usize> {
    AxValue::Array(ids.iter().map(|&i| AxValue::Element(i)).collect())
}

/// With two Chromes running, only the first pgrep PID was searched: the dialog in the other was never seen.
#[test]
fn test_the_dialog_is_found_in_whichever_chrome_shows_it() {
    let ax = FakeAx {
        nodes: vec![
            node(&[("AXRole", s("AXHeading")), ("AXTitle", s(DIALOG_TEXT))]), // 0 heading
            node(&[("AXRole", s("AXButton")), ("AXTitle", s("Allow"))]),     // 1 allow
            node(&[("AXRole", s("AXWindow")), ("AXChildren", kids(&[]))]),   // 2 a Chrome with no dialog
            node(&[("AXRole", s("AXWindow")), ("AXChildren", kids(&[0, 1]))]), // 3 the one that asks
            node(&[("AXWindows", kids(&[2]))]),                               // 4 app 111
            node(&[("AXWindows", kids(&[3]))]),                               // 5 app 222
        ],
        apps: HashMap::from([(111, 4), (222, 5)]),
    };
    let pids = parse_pids("111\n222\n");
    assert_eq!(pids, vec![111, 222]);
    assert_eq!(find_dialog_in(&ax, &pids, "Allow"), (true, Some(1)));
    assert!(press_in(&ax, &pids, "Allow"));
    assert!(!press_in(&ax, &pids, "Cancel"), "no Cancel button: nothing pressed");
    assert_eq!(find_dialog_in(&ax, &[111], "Allow"), (false, None));
}

#[test]
fn test_a_label_falls_back_to_description_then_value_and_is_stripped() {
    let ax = FakeAx {
        nodes: vec![
            node(&[("AXRole", s("AXStaticText")), ("AXTitle", s("")), ("AXValue", s(&format!("  {DIALOG_TEXT}\n")))]),
            node(&[("AXRole", s("AXButton")), ("AXDescription", s("Cancel"))]),
            node(&[("AXRole", s("AXWindow")), ("AXChildren", kids(&[0, 1]))]),
            node(&[("AXWindows", kids(&[2]))]),
        ],
        apps: HashMap::from([(7, 3)]),
    };
    assert_eq!(find_dialog_in(&ax, &[7], "Cancel"), (true, Some(1)));
    // no label asked for: the dialog is seen, no button is picked
    assert_eq!(find_dialog_in(&ax, &[7], ""), (true, None));
    assert_eq!(py_strip("\x1d Allow\u{3000}\x1e"), "Allow");
}

#[test]
fn test_pgrep_output_keeps_only_digit_words() {
    assert_eq!(parse_pids("12 abc\n 34\n-5\n"), vec![12, 34]);
    assert!(parse_pids("").is_empty());
}

#[tokio::test]
async fn test_answered_at_the_mac_withdraws_the_phone_question() {
    let (b, pressed, told) = broker_with(&[true, true, false], "yes", true, true); // the dialog vanishes while we wait
    assert_eq!(b.answer_own_connection().await, "answered_at_mac");
    assert!(got(&pressed).is_empty());
    assert_eq!(got(&told), strs(&[GONE_LINE]));
}

#[tokio::test]
async fn test_a_press_that_fails_while_the_dialog_is_still_up_says_why() {
    let (b, pressed, told) = broker_with(&[true], "yes", false, false);
    assert_eq!(b.answer_own_connection().await, "press_failed");
    assert_eq!(got(&pressed), vec!["Allow".to_string(); PRESS_TRIES]);
    assert_eq!(got(&told), strs(&[PRESS_FAILED_LINE]));
}

/// Measured 2026-09-24: AXPress in the dialog's first moments reports success and dismisses nothing. The
/// daemon logged auto_allowed while the dialog stayed up. Only a dialog that has gone counts as pressed.
#[tokio::test]
async fn test_a_press_chrome_ignored_is_repeated_until_the_dialog_is_gone() {
    let pressed = log();
    let showing: Showing = {
        let pressed = pressed.clone();
        Arc::new(move || {
            let shows = pressed.lock().len() < 2; // the first press does nothing; the second lands
            Box::pin(async move { shows })
        })
    };
    let pressing: Pressing = {
        let pressed = pressed.clone();
        Arc::new(move |label: String| {
            pressed.lock().push(label);
            Box::pin(async { true }) // Chrome says "done" both times
        })
    };
    let b = ConsentBroker::new(
        None,
        BrokerOptions {
            appear_secs: 0.05,
            poll_secs: 0.01,
            settle_secs: 0.001,
            auto_allow: true,
            ..opts(showing.clone(), pressing.clone())
        },
    );
    assert_eq!(b.answer_own_connection().await, "auto_allowed");
    assert_eq!(got(&pressed), strs(&["Allow", "Allow"]));

    pressed.lock().clear();
    let ask = ask_fn(|_q| Box::pin(async { Ok("yes".to_string()) }));
    let b = ConsentBroker::new(
        Some(ask),
        BrokerOptions {
            appear_secs: 0.05,
            ask_timeout_secs: 1.0,
            poll_secs: 0.01,
            settle_secs: 0.001,
            ..opts(showing, pressing)
        },
    );
    assert_eq!(b.answer_own_connection().await, "allowed");
    assert_eq!(got(&pressed), strs(&["Allow", "Allow"]));
}

/// browser_lane.CONSENT_TIMEOUT_MS is 120_000 in the Python (browser_lane.py); this crate cannot depend on the
/// browser lane (it depends on this one), so the bound is written here.
#[test]
fn test_the_phone_wait_ends_before_the_connection_gives_up() {
    const CONSENT_TIMEOUT_MS: f64 = 120_000.0;
    assert!(ASK_TIMEOUT_SECS * 1000.0 < CONSENT_TIMEOUT_MS);
}

#[tokio::test]
async fn test_the_standing_yes_presses_allow_without_asking() {
    let pressed = log();
    let asked = log();
    let showing: Showing = {
        let pressed = pressed.clone();
        Arc::new(move || {
            let shows = pressed.lock().is_empty();
            Box::pin(async move { shows })
        })
    };
    let pressing: Pressing = {
        let pressed = pressed.clone();
        Arc::new(move |label: String| {
            pressed.lock().push(label);
            Box::pin(async { true })
        })
    };
    let ask = {
        let asked = asked.clone();
        ask_fn(move |q| {
            asked.lock().push(q);
            Box::pin(async { Ok("no".to_string()) })
        })
    };
    for ask_owner in [Some(ask), None] {
        // with or without Telegram
        pressed.lock().clear();
        let b = ConsentBroker::new(
            ask_owner,
            BrokerOptions {
                appear_secs: 0.05,
                poll_secs: 0.01,
                settle_secs: 0.001,
                auto_allow: true,
                ..opts(showing.clone(), pressing.clone())
            },
        );
        assert_eq!(b.answer_own_connection().await, "auto_allowed");
        assert_eq!(got(&pressed), strs(&["Allow"]));
        assert!(got(&asked).is_empty());
    }
}

#[tokio::test]
async fn test_the_standing_yes_retries_a_lagging_button_and_never_presses_cancel() {
    let pressed = log();
    let landed = Arc::new(AtomicUsize::new(0));
    let showing: Showing = {
        let landed = landed.clone();
        Arc::new(move || {
            let shows = landed.load(Ordering::SeqCst) == 0;
            Box::pin(async move { shows })
        })
    };
    let pressing: Pressing = {
        let (pressed, landed) = (pressed.clone(), landed.clone());
        Arc::new(move |label: String| {
            let mut p = pressed.lock();
            p.push(label);
            let ok = p.len() >= 3; // the button appears on the third look
            if ok {
                landed.fetch_add(1, Ordering::SeqCst);
            }
            Box::pin(async move { ok })
        })
    };
    let b = ConsentBroker::new(
        None,
        BrokerOptions {
            appear_secs: 0.05,
            poll_secs: 0.01,
            settle_secs: 0.001,
            auto_allow: true,
            ..opts(showing.clone(), pressing)
        },
    );
    assert_eq!(b.answer_own_connection().await, "auto_allowed");
    assert_eq!(got(&pressed), vec!["Allow".to_string(); 3]);

    pressed.lock().clear();
    landed.store(0, Ordering::SeqCst); // a fresh dialog
    let never: Pressing = {
        let pressed = pressed.clone();
        Arc::new(move |label: String| {
            pressed.lock().push(label);
            Box::pin(async { false })
        })
    };
    let b = ConsentBroker::new(
        None,
        BrokerOptions {
            appear_secs: 0.05,
            poll_secs: 0.01,
            settle_secs: 0.001,
            auto_allow: true,
            ..opts(showing, never)
        },
    );
    assert_eq!(b.answer_own_connection().await, "press_failed");
    assert!(!got(&pressed).contains(&"Cancel".to_string()));
    assert_eq!(got(&pressed), vec!["Allow".to_string(); PRESS_TRIES]);
}

#[tokio::test]
async fn test_the_standing_yes_still_waits_for_chromes_own_dialog() {
    let pressed = log();
    let showing: Showing = Arc::new(|| Box::pin(async { false }));
    let pressing: Pressing = {
        let pressed = pressed.clone();
        Arc::new(move |label: String| {
            pressed.lock().push(label);
            Box::pin(async { true })
        })
    };
    let b = ConsentBroker::new(
        None,
        BrokerOptions {
            appear_secs: 0.05,
            poll_secs: 0.01,
            settle_secs: 0.001,
            auto_allow: true,
            ..opts(showing, pressing)
        },
    );
    assert_eq!(b.answer_own_connection().await, "no_dialog");
    assert!(got(&pressed).is_empty());
}

#[test]
fn test_the_access_preference_is_allow_only_when_spelled_allow() {
    let cases: [(Option<&str>, &str); 6] = [
        (Some("allow"), "allow"),
        (Some(" Allow "), "allow"),
        (Some("ask"), "ask"),
        (Some(""), "ask"),
        (Some("yes"), "ask"),
        (None, "ask"),
    ];
    // Python's strip() also drops the \x1c..\x1f separators
    assert_eq!(access_preference_in(&HashMap::from([("EVE_CHROME_ACCESS".to_string(), "\x1callow\x1f".to_string())])), "allow");
    for (value, expect) in cases {
        let env: HashMap<String, String> = match value {
            None => HashMap::new(),
            Some(v) => HashMap::from([("EVE_CHROME_ACCESS".to_string(), v.to_string())]),
        };
        assert_eq!(access_preference_in(&env), expect, "{value:?}");
    }
}

#[test]
fn test_ax_values_read_as_python_str() {
    assert_eq!(AxValue::<usize>::Float(1.0).py_str(), "1.0");
    assert_eq!(AxValue::<usize>::Int(3).py_str(), "3");
    assert!(!AxValue::<usize>::Str(String::new()).truthy());
    assert!(!AxValue::<usize>::Array(vec![]).truthy());
}

/// The real AX walk on this Mac: never a crash, whatever Chrome is doing (no dialog is expected to show).
#[tokio::test]
#[ignore = "live: walks the running Chrome's Accessibility tree"]
async fn test_live_dialog_showing_reads_without_crashing() {
    // read-only: it never presses anything on the owner's Chrome
    let _ = dialog_showing().await;
}
