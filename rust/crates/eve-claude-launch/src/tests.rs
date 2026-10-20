//! Port of `bridge/tests/test_claude_launch.py` (the tree, the harness rule, opening the terminal), plus parity
//! checks against Python's own output for the pieces Python's standard library did.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::*;

/// The `root` fixture: a personal and a work area, a hidden folder, a file, and a folder outside the areas.
fn root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    for name in ["eve", "car", "portfolio", "spotify-knob"] {
        fs::create_dir_all(base.join("personal").join(name)).unwrap();
    }
    for name in [
        "era-maker", "era-maker-213", "era-maker-214", "era-hub-api", "era-ingress", "portfolio", "spotify-knob",
        ".hidden",
    ] {
        fs::create_dir_all(base.join("work").join(name)).unwrap();
    }
    fs::write(base.join("work").join("notes.txt"), "not a folder").unwrap();
    fs::create_dir_all(base.join("Second Brain")).unwrap();
    (dir, base)
}

fn flow(root: &Path) -> LaunchFlow {
    LaunchFlow::new(root).with_areas(&DEFAULT_AREAS)
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// ---- matching and the list ----------------------------------------------------------------------

#[test]
fn test_match_is_forgiving_and_exact_wins() {
    let names = ["era-maker", "era-maker-213", "era-hub-api", "Era-Hub"];
    assert_eq!(match_names("era maker", &names), strings(&["era-maker"])); // spaces for dashes
    assert_eq!(match_names("ERA_HUB_API", &names), strings(&["era-hub-api"]));
    assert_eq!(match_names("era-mak", &names), strings(&["era-maker", "era-maker-213"])); // prefix, shortest first
    assert_eq!(match_names("hub", &names), strings(&["Era-Hub", "era-hub-api"])); // substring
    assert_eq!(match_names("era-hub-apu", &names)[0], "era-hub-api"); // a close spelling, best first
    assert!(match_names("zzz", &names).is_empty());
}

#[test]
fn test_menu_folds_numbered_worktrees_into_their_base() {
    let names = ["era-maker", "era-maker-213", "era-maker-214", "fw-ERA-4640", "era-hub-api"];
    assert_eq!(
        menu(&names),
        vec![("era-maker".to_string(), 2), ("fw-ERA-4640".to_string(), 0), ("era-hub-api".to_string(), 0)]
    );
}

#[test]
fn test_folders_skip_hidden_and_files() {
    let (_d, root) = root();
    let names = folders(&root.join("work"));
    assert!(!names.contains(&".hidden".to_string()));
    assert!(!names.contains(&"notes.txt".to_string()));
}

// ---- the tree -----------------------------------------------------------------------------------

#[test]
fn test_first_question_is_personal_or_work() {
    let (_d, root) = root();
    let step = flow(&root).start("");
    assert!(step.text.starts_with("Personal or Work?"));
    assert!(step.folder.is_none() && !step.done);
}

#[test]
fn test_area_then_general_opens_the_area_itself() {
    let (_d, root) = root();
    let mut f = flow(&root);
    f.start("");
    assert!(f.answer("work").text.contains("general, or which folder"));
    let step = f.answer("general");
    assert!(step.done && step.folder == Some(root.join("work")) && step.harness.as_deref() == Some("era-code"));
}

#[test]
fn test_area_then_folder_name() {
    let (_d, root) = root();
    let mut f = flow(&root);
    f.start("");
    f.answer("p"); // one letter picks the area
    let step = f.answer("Eve");
    assert_eq!(step.folder, Some(root.join("personal").join("eve")));
    assert_eq!(step.harness.as_deref(), Some("claude"));
}

#[test]
fn test_list_shows_folded_folders_and_takes_a_number() {
    let (_d, root) = root();
    let mut f = flow(&root);
    f.start("work");
    let step = f.answer("list");
    assert!(step.text.contains("era-maker (+2 numbered)"));
    assert!(!step.text.contains("era-maker-213") && !step.text.contains(".hidden"));
    let n = step.text.lines().find(|l| l.ends_with("era-ingress")).unwrap().split('.').next().unwrap().to_string();
    assert_eq!(f.answer(&n).folder, Some(root.join("work").join("era-ingress")));
}

#[test]
fn test_a_folder_name_first_skips_the_area_when_unique() {
    let (_d, root) = root();
    let step = flow(&root).start("car");
    assert_eq!(step.folder, Some(root.join("personal").join("car")));
    assert_eq!(step.harness.as_deref(), Some("claude"));
}

#[test]
fn test_a_name_in_both_areas_becomes_a_numbered_choice() {
    let (_d, root) = root();
    let mut f = flow(&root);
    let step = f.start("portfolio");
    assert!(step.text.contains("1. portfolio (personal)") && step.text.contains("2. portfolio (work)"));
    let picked = f.answer("2");
    assert_eq!(picked.folder, Some(root.join("work").join("portfolio")));
    assert_eq!(picked.harness.as_deref(), Some("era-code"));
}

#[test]
fn test_one_line_request_with_area_and_folder() {
    let (_d, root) = root();
    let step = flow(&root).start("work era hub api");
    assert_eq!(step.folder, Some(root.join("work").join("era-hub-api")));
}

#[test]
fn test_unknown_folder_says_so_and_stays_open() {
    let (_d, root) = root();
    let mut f = flow(&root);
    let step = f.start("work nope");
    assert!(step.text.contains("No folder like") && !step.done);
    assert_eq!(f.answer("era-ingress").folder, Some(root.join("work").join("era-ingress")));
}

#[test]
fn test_out_of_range_number() {
    let (_d, root) = root();
    let mut f = flow(&root);
    f.start("portfolio");
    assert!(f.answer("7").text.contains("from 1 to 2"));
}

#[test]
fn test_recent_numbers_are_offered_first() {
    let (_d, root) = root();
    let recent = vec![Recent::new(root.join("work").join("era-maker"), "claude"), Recent::new(root.join("gone"), "claude")];
    let mut f = flow(&root).with_recent(recent);
    let step = f.start("");
    assert!(step.text.contains("1. era-maker (work)") && !step.text.contains("gone"));
    let picked = f.answer("1");
    assert_eq!(picked.folder, Some(root.join("work").join("era-maker")));
    assert_eq!(picked.harness.as_deref(), Some("era-code")); // the area decides, not what that session used last time
}

// ---- the harness: implicit by area, switched only when said -------------------------------------

#[test]
fn test_split_harness() {
    let cases: [(&str, &str, Option<&str>); 7] = [
        ("work era-maker", "work era-maker", None),
        ("era maker, but I want it in claude instead of era code", "era maker", Some("claude")),
        ("eve instead of claude", "eve", Some("era-code")),
        ("eve in era code", "eve", Some("era-code")),
        ("work atlas claude instead", "work atlas", Some("claude")),
        ("car with claude code", "car", Some("claude")),
        ("work claude-debrief", "work claude-debrief", None), // a folder named claude-something stays a name
    ];
    for (text, words, harness) in cases {
        assert_eq!(split_harness(text), (words.to_string(), harness), "{text}");
    }
}

#[test]
fn test_explicit_override_switches_the_area_default() {
    let (_d, root) = root();
    let step = flow(&root).start("work era maker, but I want it in claude instead of era code");
    assert_eq!(step.folder, Some(root.join("work").join("era-maker")));
    assert_eq!(step.harness.as_deref(), Some("claude"));
    let step = flow(&root).start("eve in era-code");
    assert_eq!(step.folder, Some(root.join("personal").join("eve")));
    assert_eq!(step.harness.as_deref(), Some("era-code"));
}

#[test]
fn test_override_given_early_survives_later_questions() {
    let (_d, root) = root();
    let mut f = flow(&root);
    f.start("work with claude");
    assert_eq!(f.answer("general").harness.as_deref(), Some("claude"));
}

#[test]
fn test_request_is_the_tool_path() {
    let (_d, root) = root();
    assert_eq!(flow(&root).request("work", "era maker", "").harness.as_deref(), Some("era-code"));
    assert_eq!(flow(&root).request("work", "era maker", "claude").harness.as_deref(), Some("claude"));
    assert!(flow(&root).request("personal", "", "").text.contains("general, or which folder"));
    assert!(flow(&root).request("", "", "").text.starts_with("Personal or Work?"));
    assert!(flow(&root).request("", "spotify knob", "").text.starts_with("Which one?"));
}

#[test]
fn test_trigger_words() {
    for text in ["new claude", "/newclaude", "New Claude session", "claude new work era-maker", "start claude eve"] {
        assert!(TRIGGER.is_match(text), "{text}");
    }
    for text in ["claude on", "claude: fix the tests", "open claude app", "renew claude"] {
        assert!(!TRIGGER.is_match(text), "{text}");
    }
}

// ---- opening the terminal -----------------------------------------------------------------------

#[test]
fn test_warp_config_is_quoted_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("we\"ird: name");
    let text = warp_config(&folder, harness_command("era-code").unwrap());
    assert!(text.contains("name: \"eve-claude\""));
    assert!(text.contains(&format!("cwd: {}", json_str(&folder.to_string_lossy()))));
    assert!(text.contains(&format!("cwd: \"{}/we\\\"ird: name\"", dir.path().display())));
    assert!(text.contains("exec: \"era-code claude --dangerously-skip-permissions\""));
}

type Calls = Rc<RefCell<Vec<Vec<String>>>>;

fn recorder(code: i32, err: &'static str) -> (Calls, impl Fn(Vec<String>) -> std::future::Ready<io::Result<(i32, String)>>) {
    let calls: Calls = Rc::new(RefCell::new(Vec::new()));
    let seen = calls.clone();
    let run = move |argv: Vec<String>| {
        seen.borrow_mut().push(argv);
        std::future::ready(Ok((code, err.to_string())))
    };
    (calls, run)
}

fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
}

#[test]
fn test_open_session_warp_writes_config_opens_by_name_and_remembers() {
    let (_d, root) = root();
    let tmp = tempfile::tempdir().unwrap();
    let (calls, run) = recorder(0, "");
    let recent = tmp.path().join("recent.json");
    let opts = OpenOptions {
        app: Some("warp".into()),
        warp_dir: Some(tmp.path().join("warp")),
        recent_path: Some(recent.clone()),
        ..Default::default()
    };
    let said = block_on(open_session_with(&root.join("work").join("era-maker"), "era-code", opts, run)).unwrap();
    assert_eq!(*calls.borrow(), vec![strings(&["open", "warp://launch/eve-claude"])]);
    let config = fs::read_to_string(tmp.path().join("warp").join("eve-claude.yaml")).unwrap();
    assert!(config.contains("era-code claude --dangerously-skip-permissions"));
    assert!(said.contains("starting in era-maker"));
    // The folder is resolved first (macOS's /var is /private/var), as Path.resolve() does.
    let resolved = eve_envfile::realpath(&root.join("work").join("era-maker")).unwrap();
    assert_eq!(load_recent(&recent), vec![Recent::new(resolved, "era-code")]);
}

#[test]
fn test_open_session_terminal_quotes_the_path() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("it's here");
    fs::create_dir(&folder).unwrap();
    let (calls, run) = recorder(0, "");
    let opts = OpenOptions { app: Some("terminal".into()), recent_path: Some(tmp.path().join("r.json")), ..Default::default() };
    block_on(open_session_with(&folder, "claude", opts, run)).unwrap();
    let script = calls.borrow()[0][2].clone();
    assert!(script.contains("claude --dangerously-skip-permissions"));
    let resolved = eve_envfile::realpath(&folder).unwrap();
    let quoted = shlex_quote(&resolved.to_string_lossy()).replace('\\', "\\\\").replace('"', "\\\"");
    assert!(script.contains(&format!("cd {quoted} && "))); // shell-quoted, then AppleScript-escaped
    assert_eq!(calls.borrow()[0][0], "osascript");
    assert_eq!(calls.borrow()[0][4], "tell application \"Terminal\" to activate");
}

#[test]
fn test_open_session_failure_is_reported_and_not_remembered() {
    let (_d, root) = root();
    let tmp = tempfile::tempdir().unwrap();
    let (_calls, run) = recorder(1, "no such app");
    let recent = tmp.path().join("recent.json");
    let opts = OpenOptions {
        app: Some("warp".into()),
        warp_dir: Some(tmp.path().join("warp")),
        recent_path: Some(recent.clone()),
        ..Default::default()
    };
    let said = block_on(open_session_with(&root.join("personal").join("car"), "claude", opts, run)).unwrap();
    assert!(said.contains("couldn't open Warp") && !recent.exists());
    assert_eq!(said, "I couldn't open Warp: no such app");
}

#[test]
fn test_recent_keeps_five_newest_without_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("r.json");
    for i in 0..7 {
        remember(&tmp.path().join(i.to_string()), "claude", &path);
    }
    remember(&tmp.path().join("3"), "era-code", &path);
    let got = load_recent(&path);
    let names: Vec<String> = got.iter().map(|r| r.folder.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, strings(&["3", "6", "5", "4", "2"]));
    assert_eq!(got[0].harness, "era-code");
}

// ---- tap buttons --------------------------------------------------------------------------------

#[test]
fn test_every_question_offers_tap_buttons_and_a_tapped_number_picks() {
    let (_d, root) = root();
    let mut flow = LaunchFlow::new(&root).with_clock(|| 0.0);
    let step = flow.start("");
    assert_eq!(step.buttons[..2], strings(&["Personal", "Work"])[..]);
    assert_eq!(step.buttons.last().unwrap(), "Cancel");
    let step = flow.answer("Work");
    assert_eq!(step.buttons, strings(&["List", "General", "Cancel"]));
    let step = flow.answer("List");
    assert_eq!(step.buttons[step.buttons.len() - 2..], strings(&["General", "Cancel"])[..]);
    let tap = step.buttons.iter().find(|b| b.ends_with(". era-hub-api")).unwrap().clone();
    let step = flow.answer(&tap); // the button's own text, as Telegram sends it
    assert!(step.done && step.folder == Some(root.join("work").join("era-hub-api")));
}

// ---- test_work_sessions.py (tests_ref) ----------------------------------------------------------

#[test]
fn test_code_root_defaults_to_documents_under_home() {
    let empty: HashMap<String, String> = HashMap::new();
    assert_eq!(code_root(&empty), home().unwrap().join("Documents"));
}

// ---- parity with Python -------------------------------------------------------------------------

#[test]
fn environment_settings_parse_as_python() {
    let mut env: HashMap<String, String> = HashMap::new();
    assert_eq!(area_names(&env), strings(&["personal", "work"]));
    env.insert("EVE_CODE_AREAS".into(), " , ".into());
    assert_eq!(area_names(&env), strings(&["personal", "work"]));
    env.insert("EVE_CODE_AREAS".into(), " side , ,work ".into());
    assert_eq!(area_names(&env), strings(&["side", "work"]));
    env.insert("EVE_CODE_ROOT".into(), "  /srv//code/ ".into());
    assert_eq!(code_root(&env), PathBuf::from("/srv/code"));
    env.insert("EVE_CODE_ROOT".into(), "~/src".into());
    assert_eq!(code_root(&env), home().unwrap().join("src"));
    env.insert("EVE_CLAUDE_TERMINAL".into(), " Terminal ".into());
    assert_eq!(terminal_app(&env), "terminal");
    env.insert("EVE_CLAUDE_TERMINAL".into(), "WARP".into());
    assert_eq!(terminal_app(&env), "warp");
    env.insert("EVE_CLAUDE_TERMINAL".into(), "iterm".into());
    let detected = if Path::new("/Applications/Warp.app").exists() { "warp" } else { "terminal" };
    assert_eq!(terminal_app(&env), detected);
}

#[test]
fn close_spellings_rank_as_difflib_does() {
    // Expected values printed by Python 3's difflib.
    assert_eq!(
        difflib::get_close_matches("erahubapu", &["eramaker", "eramaker213", "erahubapi", "erahub"], 5, 0.7),
        strings(&["erahubapi", "erahub"])
    );
    assert_eq!(difflib::get_close_matches("ab", &["ba", "ab_", "bab", "aab"], 5, 0.6), strings(&["bab", "ab_", "aab"]));
    assert_eq!(difflib::get_close_matches("appel", &["ape", "apple", "peach", "puppy"], 3, 0.6), strings(&["apple", "ape"]));
    let cases: [(String, String, f64); 7] = [
        ("erahubapi".into(), "erahubapu".into(), 0.8888888888888888),
        ("abcd".into(), "bcde".into(), 0.75),
        ("spotifyknob".into(), "spotfyknob".into(), 0.9523809523809523),
        ("aaab".into(), "abaa".into(), 0.5),
        ("x".repeat(250) + "y", "x".repeat(100) + "yy", 0.5722379603399433), // autojunk: "x" is popular in b
        (String::new(), String::new(), 1.0),
        ("a".into(), String::new(), 0.0),
    ];
    for (a, b, want) in cases {
        assert_eq!(difflib::ratio(&a, &b), want, "{a:.10} {b:.10}");
    }
}

#[test]
fn recent_file_is_written_as_python_writes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("deep").join("recent.json");
    remember(Path::new("/tmp/é😀\u{7f}\u{1}/"), "claude", &path);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "{\n  \"recent\": [\n    {\n      \"folder\": \"/tmp/\\u00e9\\ud83d\\ude00\\u007f\\u0001\",\n      \"harness\": \"claude\"\n    }\n  ]\n}\n"
    );
    assert_eq!(load_recent(&path), vec![Recent::new("/tmp/é😀\u{7f}\u{1}", "claude")]);
}

#[test]
fn malformed_recent_rows_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("r.json");
    assert!(load_recent(&path).is_empty()); // missing
    fs::write(&path, "not json").unwrap();
    assert!(load_recent(&path).is_empty());
    fs::write(&path, "[1, 2]").unwrap();
    assert!(load_recent(&path).is_empty());
    fs::write(&path, r#"{"recent": {"folder": "/a"}}"#).unwrap(); // a dict iterates its keys: none is a row
    assert!(load_recent(&path).is_empty());
    fs::write(
        &path,
        r#"{"recent": [{"folder": "/a//b/./", "harness": "claude"}, {"folder": "/c", "harness": "codex"},
            {"folder": 3, "harness": "claude"}, "x", {"folder": "/d", "harness": "era-code"}]}"#,
    )
    .unwrap();
    assert_eq!(load_recent(&path), vec![Recent::new("/a/b", "claude"), Recent::new("/d", "era-code")]);
}

#[test]
fn an_unwritable_recent_file_is_not_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("file"), "x").unwrap();
    remember(Path::new("/x"), "claude", &tmp.path().join("file").join("r.json"));
    assert!(!tmp.path().join("file").join("r.json").exists());
}

#[test]
fn shlex_quote_matches_python() {
    let cases = [("", "''"), ("abc", "abc"), ("a b", "'a b'"), ("it's", "'it'\"'\"'s'"), ("é", "'é'"), ("a@%+=:,./-_", "a@%+=:,./-_")];
    for (s, want) in cases {
        assert_eq!(shlex_quote(s), want, "{s}");
    }
}

#[test]
fn capitalize_matches_python() {
    assert_eq!(capitalize("personal"), "Personal");
    assert_eq!(capitalize("WORK"), "Work");
    assert_eq!(capitalize("ǆx"), "ǅx");
    assert_eq!(capitalize("ÉCOLE"), "École");
    assert_eq!(capitalize("ß"), "Ss");
    assert_eq!(capitalize("ΑΣ"), "Ας"); // a final sigma stays final after the first letter
    assert_eq!(capitalize(""), "");
}

#[test]
fn trigger_argument_is_the_rest_of_the_words() {
    assert_eq!(trigger_argument("new claude"), Some(String::new()));
    assert_eq!(trigger_argument("/new_claude work eve"), Some("work eve".to_string()));
    assert_eq!(trigger_argument("start claude\n"), Some(String::new())); // Python's $ before a final newline
    assert_eq!(trigger_argument("claude session two\nlines"), Some("two\nlines".to_string()));
    assert_eq!(trigger_argument("claude on"), None);
}

#[test]
fn a_question_expires_after_five_minutes() {
    let now = Arc::new(parking_lot_free_clock::Clock::default());
    let (_d, root) = root();
    let c = now.clone();
    let mut f = LaunchFlow::new(&root).with_clock(move || c.get());
    now.set(1000.0);
    f.start("");
    now.set(1300.0);
    assert!(!f.expired());
    now.set(1300.5);
    assert!(f.expired());
    f.answer("work");
    assert!(!f.expired());
    assert!(LaunchFlow::new(&root).expired() || monotonic() <= FLOW_TIMEOUT_SECS);
}

mod parking_lot_free_clock {
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Default)]
    pub struct Clock(AtomicU64);

    impl Clock {
        pub fn set(&self, v: f64) {
            self.0.store(v.to_bits(), Ordering::SeqCst);
        }
        pub fn get(&self) -> f64 {
            f64::from_bits(self.0.load(Ordering::SeqCst))
        }
    }
}

#[test]
fn edge_answers_follow_python() {
    let (_d, root) = root();
    let mut f = flow(&root);
    f.start("");
    // "list" before an area asks the area again; an empty reply repeats the question.
    assert!(f.answer("list").text.starts_with("Personal or Work?"));
    assert!(f.answer("  ...!").text.starts_with("Personal or Work?"));
    f.answer("work");
    assert!(f.answer("").text.starts_with("Work: general"));
    // A number with nothing to choose from is a folder name.
    assert!(f.answer("0").text.starts_with("No folder like \"0\" in work. Say list to see them."));
    let mut f = flow(&root);
    f.start("portfolio");
    assert_eq!(f.answer("0").text, "Pick a number from 1 to 2.");
    assert_eq!(f.answer("99999999999999999999999").text, "Pick a number from 1 to 2.");
    // Nowhere to look.
    let empty = tempfile::tempdir().unwrap();
    let step = flow(empty.path()).start("");
    assert!(step.done);
    assert_eq!(step.text, format!("I can't find personal, work under {}.", empty.path().display()));
    // No folder anywhere: both areas named, no list hint.
    assert_eq!(flow(&root).start("zzzz").text, "No folder like \"zzzz\" in personal or work.");
    // An empty area.
    fs::create_dir_all(root.join("side")).unwrap();
    let mut f = LaunchFlow::new(&root).with_areas(&["side"]);
    f.start("side");
    let step = f.answer("ls");
    assert_eq!(step.text, "Side has no folders. Say general to open it.");
    let step = f.answer("general.");
    assert_eq!(step.text, "Opening side (general) with claude --dangerously-skip-permissions.");
}

#[test]
fn list_step_text_and_title() {
    let (_d, root) = root();
    let mut f = flow(&root);
    f.start("work");
    let step = f.answer("list");
    assert_eq!(step.title, "Work folders");
    assert_eq!(
        step.text,
        "1. era-hub-api\n2. era-ingress\n3. era-maker (+2 numbered)\n4. portfolio\n5. spotify-knob\n\n\
         Tap one, or reply a number or a name, or general."
    );
    let opened = f.answer("3");
    assert_eq!(opened.text, "Opening era-maker (work) with era-code claude --dangerously-skip-permissions.");
    assert_eq!(opened.title, "New Claude session");
}

#[test]
fn many_hits_are_capped_at_ten() {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..13 {
        fs::create_dir_all(tmp.path().join("work").join(format!("app{i:02}"))).unwrap();
    }
    let step = flow(tmp.path()).start("work app");
    assert!(step.text.starts_with("Which one?\n1. app00 (work)\n"));
    assert!(step.text.ends_with("10. app09 (work)\n…and 3 more; type more of the name."));
    assert_eq!(step.buttons.len(), 11);
}

#[test]
fn open_session_refuses_a_missing_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let (calls, run) = recorder(0, "");
    let missing = tmp.path().join("gone");
    let said = block_on(open_session_with(&missing, "claude", OpenOptions::default(), run)).unwrap();
    let resolved = eve_envfile::realpath(&missing).unwrap();
    assert_eq!(said, format!("{} is not a folder any more.", resolved.display()));
    assert!(calls.borrow().is_empty());
}

#[test]
fn open_session_reports_an_unwritable_warp_folder() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("file"), "x").unwrap();
    let (calls, run) = recorder(0, "");
    let opts = OpenOptions { app: Some("warp".into()), warp_dir: Some(tmp.path().join("file")), ..Default::default() };
    let said = block_on(open_session_with(tmp.path(), "claude", opts, run)).unwrap();
    assert_eq!(said, "I couldn't write the Warp launch configuration (FileExistsError).");
    assert!(calls.borrow().is_empty());
}

#[test]
fn run_returns_code_and_first_stream() {
    let (code, text) = block_on(run(strings(&["sh", "-c", "echo out; echo '  err  ' >&2; exit 3"]))).unwrap();
    assert_eq!((code, text.as_str()), (3, "err"));
    let (code, text) = block_on(run(strings(&["sh", "-c", "printf 'only out\\n'"]))).unwrap();
    assert_eq!((code, text.as_str()), (0, "only out"));
    let (code, _) = block_on(run(strings(&["sh", "-c", "kill -9 $$"]))).unwrap();
    assert_eq!(code, -9);
    assert!(block_on(run(strings(&["/nonexistent/program"]))).is_err());
}

#[test]
fn more_phrasings_match_python() {
    // Expected values printed by the Python module.
    let cases: [(&str, &str, Option<&str>); 10] = [
        ("eve, with era-code instead", "eve", Some("era-code")),
        ("but use claude", "", Some("claude")),
        ("work era maker as claude code", "work era maker", Some("claude")),
        ("instead of claude code", "", Some("era-code")),
        ("work eve via ERA CODE.", "work eve", Some("era-code")),
        ("eve in claude", "eve", Some("claude")),
        ("claudette", "claudette", None),
        ("work era-coder", "work era-coder", None),
        ("x, but i want it in eracode instead of claude.", "x", Some("era-code")),
        ("era code instead", "", Some("era-code")),
    ];
    for (text, words, harness) in cases {
        assert_eq!(split_harness(text), (words.to_string(), harness), "{text}");
    }
    let names = ["eve", "Eve-App", "eve_bridge", "spotify-knob", "spotify"];
    assert_eq!(match_names("EVE", &names), strings(&["eve"]));
    assert_eq!(match_names("ev", &names), strings(&["eve", "Eve-App", "eve_bridge"]));
    assert_eq!(match_names("knob", &names), strings(&["spotify-knob"]));
    assert_eq!(match_names("spotfy", &names), strings(&["spotify", "spotify-knob"]));
    assert_eq!(match_names("bridg e", &names), strings(&["eve_bridge"]));
    assert!(match_names("", &names).is_empty());
    assert_eq!(
        menu(&["a-1", "a", "b-2", "b-x-3", "b-x", "c-01"]),
        vec![("a".to_string(), 1), ("b-2".to_string(), 0), ("b-x".to_string(), 1), ("c-01".to_string(), 0)]
    );
}

#[test]
fn paths_in_owner_text_are_written_as_python_writes_them() {
    // `str(Path(...))`: a trailing slash or a `.` part is not part of the path Python prints.
    let empty = tempfile::tempdir().unwrap();
    let loose = format!("{}/./", empty.path().display());
    let step = LaunchFlow::new(&loose).start("");
    assert_eq!(step.text, format!("I can't find personal, work under {}.", empty.path().display()));
    let (_d, root) = root();
    let f = flow(&root);
    assert_eq!(f.label(Path::new("/elsewhere//x/")), "/elsewhere/x");
}
