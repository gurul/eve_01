//! The parts of bridge/tests/test_app_check.py that need no browser, and the module's constants checked
//! against app_check.py itself. The browser tests are in tests/browser.rs.

use serde_json::{json, Map, Value};

use super::*;

const PY_SOURCE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/app_check.py"));
const APPS_MAKER: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/apps_maker.py"));

fn field(tag: &str, type_: &str, label: &str, extra: &[(&str, &str)]) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("tag".into(), json!(tag));
    m.insert("type".into(), json!(type_));
    m.insert("label".into(), json!(label));
    m.insert("named".into(), json!(label));
    for (k, v) in extra {
        m.insert((*k).into(), json!(v));
    }
    m
}

fn input(type_: &str, label: &str, extra: &[(&str, &str)]) -> Map<String, Value> {
    field("input", type_, label, extra)
}

fn v(ctl: &Map<String, Value>, alt: bool) -> String {
    let d = NaiveDate::from_ymd_opt(2026, 9, 24).unwrap();
    value_for(ctl, d, alt).unwrap()
}

#[test]
fn test_values_typed_into_fields_fit_the_field() {
    assert_eq!(v(&input("date", "Target date", &[]), false), "2026-09-24");
    assert_eq!(v(&input("number", "", &[]), false), "12");
    assert_eq!(v(&input("", "Weight (kg)", &[]), false), "60");
    assert_eq!(v(&input("search", "Search", &[]), false), "");
    assert_eq!(v(&field("textarea", "", "Notes", &[]), false), "Felt good today.");
    assert_eq!(v(&input("", "Habit name", &[]), false), "Read 10 pages");
    assert_eq!(v(&input("", "Title", &[]), false), "Morning run");
    assert_eq!(v(&input("", "Author", &[]), false), "Ursula K. Le Guin");
    assert_eq!(v(&input("week", "", &[]), false), "2026-W39");
    // the placeholder's own example beats any guess, and a word in it is not mistaken for a number field
    assert_eq!(v(&input("", "Name", &[("placeholder", "e.g. Stretch for 5 minutes")]), false), "Stretch for 5 minutes");
    assert_eq!(v(&input("", "", &[("placeholder", "For example: “Milk”, “Eggs”…")]), false), "Milk");
    assert_eq!(v(&input("", "Name", &[("placeholder", "Stretch for 5 minutes")]), false), "Morning run");
    // numbers: what a prefilled field holds, inside min/max, on the step; decimals only where allowed
    assert_eq!(v(&input("number", "Glass size (ml)", &[("value", "250")]), false), "250");
    assert_eq!(v(&input("number", "Glass size (ml)", &[]), false), "250");
    assert_eq!(v(&input("number", "Goal", &[("min", "1"), ("max", "5")]), false), "3");
    assert_eq!(v(&input("number", "Amount", &[]), false), "24");
    assert_eq!(v(&input("", "Amount", &[("inputmode", "decimal")]), false), "24.5");
    assert_eq!(v(&input("number", "Weight", &[("step", "2.5"), ("min", "0")]), false), "60");
    assert_eq!(v(&input("number", "Rating", &[("min", "1"), ("max", "5")]), false), "4");
    assert_eq!(v(&input("", "Monthly limit", &[("inputmode", "decimal")]), false), "400");
    // a second try after the app rejected the first: a different value inside the range
    assert_eq!(v(&input("number", "Reps", &[("min", "1"), ("value", "8")]), true), "2");
    // a prefilled text field keeps what the app put there
    assert_eq!(v(&input("", "Habit name", &[("value", "Drink water")]), false), "Drink water");
}

#[test]
fn test_what_the_check_made_is_reported() {
    assert_eq!(
        data_shape(&json!({"v": 1, "habits": [{"name": "a", "done": {"2026-09-24": 1}}, {"name": "b", "done": {}}]})),
        "habits 2, habits.done 1"
    );
    assert_eq!(data_shape(&json!({"v": 1, "items": []})), "");
    assert_eq!(data_shape(&Value::Null), "");
    assert_eq!(CheckReport { photo_data: json!({"v": 1, "items": [1, 2]}), ..Default::default() }.made(), "items 2");
    assert_eq!(CheckReport::default().made(), "no records");
    assert!(CheckReport::default().summary().ends_with("0 saves"));
}

#[test]
fn test_the_stand_in_is_telegrams_normal_mode_with_nothing_inset_at_the_top() {
    assert!(STUBS_JS.contains("const safe = { top: 0,") && STUBS_JS.contains("isFullscreen: false"));
}

#[test]
fn test_the_pencils_place_in_the_check_is_where_eve_js_draws_it() {
    let p = PENCIL;
    assert_eq!(p.as_dict(), json!({"top": 6, "right": 6, "size": 44}));
    let eve_js = &APPS_MAKER[APPS_MAKER.find("EVE_JS = \"\"\"").expect("EVE_JS in apps_maker.py")..];
    assert!(eve_js.contains(&format!("width: {}px !important; height: {}px !important", p.size, p.size)));
    assert!(eve_js.contains(&format!("top: calc({}px + var(--tg-safe-area-inset-top", p.top)));
    assert!(eve_js.contains(&format!("right: calc({}px + var(--tg-safe-area-inset-right", p.right)));
}

#[test]
fn the_page_scripts_are_pythons() {
    for (name, js) in [
        ("STUBS_JS", STUBS_JS),
        ("CONTROLS_JS", CONTROLS_JS),
        ("REACH_JS", REACH_JS),
        ("PRESS_JS", PRESS_JS),
        ("ERRORS_JS", ERRORS_JS),
        ("VIEW_JS", VIEW_JS),
        ("CLOSE_JS", CLOSE_JS),
        ("LOOK_JS", LOOK_JS),
        ("BAR_JS", BAR_JS),
        ("LAYOUT_JS", LAYOUT_JS),
    ] {
        assert!(PY_SOURCE.contains(&format!("{name} = r\"\"\"{js}\"\"\"")), "{name} differs from app_check.py");
    }
    // the press label probe in _press, the same bytes
    let probe = "\"(n) => { const b = window.Telegram.WebApp[n]; return b && b.isVisible \"\n                                \"&& b.isActive !== false ? (b.text || n) : null; }\"";
    assert!(PY_SOURCE.contains(probe));
    assert_eq!(
        PRESSABLE_JS,
        "(n) => { const b = window.Telegram.WebApp[n]; return b && b.isVisible && b.isActive !== false ? (b.text || n) : null; }"
    );
}

/// Every constant of app_check.py as Python evaluates it, through python3 (skipped where there is none).
#[test]
fn the_constants_are_pythons() {
    let script = r#"
import ast, json, sys
tree = ast.parse(open(sys.argv[1]).read())
out = {}
for n in tree.body:
    if isinstance(n, ast.Assign) and len(n.targets) == 1 and isinstance(n.targets[0], ast.Name):
        try:
            out[n.targets[0].id] = ast.literal_eval(n.value)
        except Exception:
            pass
    if isinstance(n, ast.AnnAssign) and isinstance(n.target, ast.Name) and n.value is not None:
        try:
            out[n.target.id] = ast.literal_eval(n.value)
        except Exception:
            pass
print(json.dumps(out))
"#;
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge/app_check.py");
    let Ok(out) = std::process::Command::new("python3").arg("-I").arg("-c").arg(script).arg(path).output() else {
        eprintln!("python3 not found: skipped");
        return;
    };
    let py: Value = serde_json::from_slice(&out.stdout).expect("python printed JSON");
    let num = |k: &str| py[k].as_f64().unwrap_or_else(|| panic!("{k}"));
    let s = |k: &str| py[k].as_str().unwrap_or_else(|| panic!("{k}")).to_string();
    assert_eq!(s("APP_URL"), APP_URL);
    assert_eq!(s("APP_HOST"), APP_HOST);
    assert_eq!(py["ALLOWED_HOSTS"], json!(ALLOWED_HOSTS));
    assert_eq!(py["STUBBED_HOSTS"], json!(STUBBED_HOSTS));
    assert_eq!(py["PHONE"], json!({"width": PHONE.width, "height": PHONE.height}));
    assert_eq!(py["SMALL_PHONE"], json!({"width": SMALL_PHONE.width, "height": SMALL_PHONE.height}));
    for (k, v) in [
        ("MAX_TAPS", MAX_TAPS as f64),
        ("MAX_FIELDS", MAX_FIELDS as f64),
        ("MAX_PER_GROUP", MAX_PER_GROUP as f64),
        ("MAX_ROUNDS", MAX_ROUNDS as f64),
        ("MAX_ISSUES", MAX_ISSUES as f64),
        ("MAX_TRACE", MAX_TRACE as f64),
        ("SETTLE_MS", SETTLE_MS as f64),
        ("TAP_WAIT_MS", TAP_WAIT_MS as f64),
        ("CLICK_TIMEOUT_MS", CLICK_TIMEOUT_MS as f64),
        ("LOAD_TIMEOUT_MS", LOAD_TIMEOUT_MS as f64),
        ("CHECK_TIMEOUT_S", CHECK_TIMEOUT_S),
        ("CLOSE_GRACE_MS", CLOSE_GRACE_MS as f64),
        ("LATE_MS", LATE_MS as f64),
        ("COVERED_WAIT_MS", COVERED_WAIT_MS as f64),
        ("COVERED_FOR_S", COVERED_FOR_S),
        ("BAR_H", BAR_H as f64),
    ] {
        assert_eq!(num(k), v, "{k}");
    }
    assert_eq!(AN_HOUR_MS, 61 * 60 * 1000);
    assert_eq!(s("SANDBOX"), SANDBOX);
    assert_eq!(py["CHROMIUM_ARGS"], json!(CHROMIUM_ARGS));
    let theme = |t: &Theme| Value::Object(t.iter().map(|(k, v)| (k.to_string(), json!(v))).collect());
    assert_eq!(py["LIGHT"], theme(LIGHT));
    assert_eq!(py["DARK"], theme(DARK));
    // key order too: the stand-in paints the variables in this order
    let keys: Vec<&str> = py["LIGHT"].as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, LIGHT.iter().map(|(k, _)| *k).collect::<Vec<_>>());
    let looks: Vec<(String, String)> =
        py["LOOKS"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
    assert_eq!(looks, LOOKS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>());
    assert_eq!(py["PENCIL"], PENCIL.as_dict());
    let numbers: Vec<(String, f64)> = py["_NUMBERS"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_f64().unwrap()))
        .collect();
    assert_eq!(numbers, NUMBERS.iter().map(|(r, v)| (r.to_string(), *v)).collect::<Vec<_>>());
    let words: Vec<(String, String)> = py["_WORDS"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(words, WORDS.iter().map(|(r, v)| (r.to_string(), v.to_string())).collect::<Vec<_>>());
}

#[test]
fn the_regexes_are_pythons() {
    for pattern in [
        r#"_PRIMARY = re.compile(r"^\s*(\+|(add|save|create|log|done|check.?in|start|new|record|track|mark)\b)", re.I)"#,
        r#"_NOTE = re.compile(r"note|comment|descr|memo|journal|thought|detail", re.I)"#,
        r#"_EXAMPLE = re.compile(r"^\s*(?:e\.?\s?g\.?|for example|for instance|such as|like|try)\s*[:,]?\s*(.+)$", re.I)"#,
        r#"re.split(r",\s|\s+or\s+|\s[·/|]\s", m.group(1))"#,
        r#"re.search(re.escape(APP_HOST) + r"/apps/check/[^:\s]*:(\d+):\d+", stack or "")"#,
        r#"re.search(r"search|filter", words)"#,
    ] {
        assert!(PY_SOURCE.contains(pattern), "{pattern}");
    }
    assert_eq!(PRIMARY.as_str(), r"(?i)^\s*(\+|(add|save|create|log|done|check.?in|start|new|record|track|mark)\b)");
    assert_eq!(NOTE.as_str(), r"(?i)note|comment|descr|memo|journal|thought|detail");
}

#[test]
fn the_examples_in_placeholders_are_read_as_python_reads_them() {
    assert_eq!(example("e.g. Stretch for 5 minutes"), "Stretch for 5 minutes");
    assert_eq!(example("eg: Floss"), "Floss");
    assert_eq!(example("Try \"Drink water\" or \"Walk\""), "Drink water");
    assert_eq!(example("such as Milk / Eggs"), "Milk");
    assert_eq!(example("like Paris…"), "Paris");
    assert_eq!(example("e.g. Floss\n"), "Floss"); // Python's $ before a final newline
    assert_eq!(example("New habit"), "");
    assert_eq!(example(&format!("e.g. {}", "x".repeat(80))).chars().count(), 60);
}

#[test]
fn numbers_round_as_python_rounds() {
    // round() is to even: 24.5 on a whole-number field is 24, 25.5 is 26
    assert_eq!(number(&input("number", "Amount", &[]), "amount ", false).unwrap(), "24");
    assert_eq!(number(&input("number", "", &[("value", "25.5")]), " ", false).unwrap(), "26");
    assert_eq!(number(&input("number", "", &[("value", "-0.001")]), " ", false).unwrap(), "0");
    assert_eq!(number(&input("", "", &[("value", " 1_000 ")]), " ", false).unwrap(), "1000");
    assert_eq!(number(&input("number", "", &[("max", "10"), ("step", "4")]), " ", false).unwrap(), "8");
    // a value Python's round() cannot take: the field is skipped
    assert_eq!(number(&input("number", "", &[("value", "nan")]), " ", false), None);
    assert_eq!(number(&input("", "", &[("value", "inf")]), " ", false).unwrap(), "inf");
    assert_eq!(py_float("1e3"), Some(1000.0));
    assert_eq!(py_float("_1"), None);
    assert_eq!(py_float("abc"), None);
    assert_eq!(py_float(""), None);
}

#[test]
fn the_line_of_the_apps_code_is_read_from_a_stack() {
    assert_eq!(where_("TypeError: x\n    at f (https://app.eve.test/apps/check/:212:9)"), " (line 212)");
    assert_eq!(where_("at https://cdn.jsdelivr.net/x.js:3:1"), "");
    assert_eq!(where_(""), "");
}

#[test]
fn a_look_finding_is_formatted_as_python_formats_it() {
    let picker = LOOKS.iter().find(|(k, _)| *k == "picker").unwrap().1;
    let text = format_look(picker, "x", "\"Date\"");
    assert!(text.contains("Declare :root { color-scheme: light dark; }") && text.contains("field \"Date\" draws"));
    // a value holding a field name is never read as one
    assert_eq!(format_look("{action}: {items}", "{items}", "a"), "{items}: a");
}

#[test]
fn saved_data_is_kept_outside_the_page_with_its_richest_version() {
    let s = Session::new(Some(&json!({"v": 1, "items": [1]})));
    assert_eq!(s.load().as_deref(), Some(r#"{"v":1,"items":[1]}"#));
    assert!(s.save(r#"{"v":1,"items":[1,2]}"#.into()));
    assert!(s.save(r#"{"v":1,"items":[]}"#.into()));
    assert_eq!(s.data(), json!({"v": 1, "items": []}));
    assert_eq!(s.st.lock().richest.as_deref(), Some(r#"{"v":1,"items":[1,2]}"#));
    s.st.lock().closed = true;
    s.save(r#"{"late":1}"#.into());
    assert_eq!(s.saves(), 2);
    assert_eq!(s.st.lock().late.as_deref(), Some(r#"{"late":1}"#));
    s.note("a");
    s.note("a");
    s.step("x");
    s.step("x");
    assert_eq!(s.issues(), vec!["a"]);
    assert_eq!(s.trace(), vec!["x"]);
    assert_eq!(richness(Some("not json")), 0);
    assert_eq!(richness(None), 0);
    assert_eq!(richness(Some(r#"{"a":[1,{"b":2}]}"#)), 5);
}

#[test]
fn the_theme_reaches_the_page_as_python_writes_it() {
    let cfg = check_cfg(DARK, true);
    assert!(cfg.starts_with("window.__checkCfg = {\"theme\": {\"bg_color\": \"#212121\", \"text_color\": \"#ffffff\""));
    assert!(cfg.ends_with("\"section_separator_color\": \"#000000\"}, \"scheme\": \"dark\"};"));
    assert_eq!(rgb("#212121"), "rgb(33, 33, 33)");
    assert_eq!(rgb("#ffffff"), "rgb(255, 255, 255)");
}

#[test]
fn requests_are_told_apart_by_host_as_python_splits_them() {
    assert_eq!(host_of("https://cdn.jsdelivr.net/npm/x"), "cdn.jsdelivr.net");
    assert_eq!(host_of("http://127.0.0.1:8080/x"), "127.0.0.1:8080");
    assert_eq!(host_of("data:text/plain,x"), "");
    assert_eq!(console_text(Some(&json!([{"type": "string", "value": "boom"}, {"type": "number", "value": 2}]))), "boom 2");
    assert_eq!(console_text(Some(&json!([{"type": "undefined"}]))), "undefined");
}

#[test]
fn the_router_serves_the_app_sandboxed_and_fences_the_rest() {
    let sess = Session::new(None);
    let html: Arc<str> = Arc::from("<p>app</p>");
    match route(&sess, &html, "https://app.eve.test/apps/check/?x=1#top") {
        Reply::Fulfill(200, headers, body) => {
            assert_eq!(&*body, "<p>app</p>");
            assert!(headers.contains(&("Content-Security-Policy", SANDBOX)));
        }
        _ => panic!("the app is served"),
    }
    assert!(matches!(route(&sess, &html, "https://telegram.org/js/telegram-web-app.js"), Reply::Fulfill(200, _, b) if b.is_empty()));
    assert!(matches!(route(&sess, &html, "https://app.eve.test/apps/eve.js"), Reply::Fulfill(200, _, _)));
    assert!(matches!(route(&sess, &html, "https://app.eve.test/favicon.ico"), Reply::Fulfill(404, _, _)));
    assert!(matches!(route(&sess, &html, "https://cdn.jsdelivr.net/npm/chart.js"), Reply::Continue));
    assert!(sess.issues().is_empty());
    sess.step("tapping \"Sync\"");
    assert!(matches!(route(&sess, &html, "https://api.example.com/x"), Reply::Abort));
    assert_eq!(
        sess.issues(),
        vec!["The app tried to reach api.example.com while tapping \"Sync\"; it may only load libraries from \
              cdn.jsdelivr.net and must keep its data with eve.save."]
    );
}

#[test]
fn the_pages_calls_reach_the_session_in_the_order_it_made_them() {
    let sess = Session::new(None);
    assert_eq!(answer(&sess, &json!({"name": "load", "id": 1, "args": []})).unwrap(), "window.__checkDeliver(1, null)");
    for (id, raw) in [(2, r#"{"n":1}"#), (3, r#"{"n":2}"#)] {
        assert_eq!(
            answer(&sess, &json!({"name": "save", "id": id, "args": [raw]})).unwrap(),
            format!("window.__checkDeliver({id}, true)")
        );
    }
    assert_eq!(sess.data(), json!({"n": 2}));
    assert_eq!(
        answer(&sess, &json!({"name": "load", "id": 4, "args": []})).unwrap(),
        r#"window.__checkDeliver(4, "{\"n\":2}")"#
    );
    assert_eq!(answer(&sess, &json!({"name": "socket", "id": 1, "args": ["ws://127.0.0.1:9/x"]})).unwrap(), "window.__checkSocketClosed(1)");
    assert!(sess.issues()[0].starts_with("The app tried to open a WebSocket to ws://127.0.0.1:9/x while opening the app;"));
    assert_eq!(answer(&sess, &json!({"name": "other"})), None);
}
