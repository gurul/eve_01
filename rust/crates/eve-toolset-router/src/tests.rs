//! Port of bridge/tests/test_toolset_router.py, plus the parts of the tests_ref files that exercise this crate
//! (test_lean_routedturn_replay.py, test_github_repos.py, test_menu_tools.py, test_events.py) and parity checks
//! against the Python module's own output on the routing datasets. Fakes only: no Jev call, no token.

use super::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

const ROUTES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/tests/fixtures/routes");

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn nouls(values: &[(&str, f64)]) -> Value {
    let mut answers = Map::new();
    for name in TOOLSETS {
        let v = values.iter().find(|(k, _)| *k == name).map(|(_, v)| *v).unwrap_or(0.0);
        answers.insert(name.to_string(), json!({"noul": v}));
    }
    json!({"answers": answers})
}

type Seen = Arc<Mutex<Vec<(Value, Value)>>>;

/// `fake(**values)`: a predict that answers these Nouls and records what it was asked.
fn fake(values: &[(&str, f64)]) -> (impl Fn(&Value, &Value) -> anyhow::Result<Value> + Send + Sync + 'static, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let out = nouls(values);
    let s = seen.clone();
    let predict = move |state: &Value, qs: &Value| -> anyhow::Result<Value> {
        s.lock().push((state.clone(), qs.clone()));
        Ok(out.clone())
    };
    (predict, seen)
}

fn half() -> IndexMap<String, f64> {
    TOOLSETS.iter().map(|n| (n.to_string(), 0.5)).collect()
}

fn zero() -> f64 {
    0.0
}

fn answer(nouls: &[(&str, f64)]) -> ToolsetAnswer {
    ToolsetAnswer::new(nouls.iter().map(|(k, v)| (*k, *v)))
}

// ---- the question ---------------------------------------------------------------------------------------------

#[test]
fn test_the_state_is_the_message_and_the_previous_one_and_every_toolset_is_one_noul() {
    let (predict, seen) = fake(&[(CALENDAR, 0.9)]);
    ask(&predict, "  my next   class ", "  whats   up ", &zero).unwrap();
    let (state, qs) = seen.lock()[0].clone();
    assert_eq!(state, json!({"about": ABOUT, "previous": "whats up", "request": "my next class"}));
    let ids: BTreeSet<String> = qs.as_object().unwrap().keys().cloned().collect();
    assert_eq!(ids, set(&TOOLSETS));
    assert!(qs.as_object().unwrap().values().all(|q| q["type"] == "noul"
        && q["instructions"].as_str().unwrap().starts_with("To do what the owner asks")));
    // the state's keys and the questions' ids go over the wire in Python's dict order
    assert_eq!(state.as_object().unwrap().keys().collect::<Vec<_>>(), ["about", "previous", "request"]);
    assert_eq!(qs.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(), TOOLSETS);
}

#[test]
fn test_the_state_clips_each_message_to_500_characters() {
    let long = "é".repeat(600);
    let s = state(&long, &format!("a{}", " b".repeat(400)));
    assert_eq!(s["request"].as_str().unwrap().chars().count(), 500);
    assert_eq!(s["previous"].as_str().unwrap().chars().count(), 500);
}

#[test]
fn test_the_instructions_name_no_internal_tool_or_vendor_the_owner_would_not_say() {
    // the question ids are the toolsets' own names; the instructions are the job, in the owner's words
    let words: String = questions()
        .as_object()
        .unwrap()
        .values()
        .map(|q| q["instructions"].as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    for name in ["composio", "open_app", "apps_mcp", "imessage", "google", "jev", "toolset"] {
        assert!(!words.contains(name), "{name}");
    }
}

#[test]
fn test_the_questions_are_the_ones_the_cached_answers_were_asked_with_where_they_overlap() {
    // the cached tuning answers recorded the call Jev was asked; the seven Nouls that remain must be byte-identical
    // (the fitted gates hold only for that call)
    let raw: Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{ROUTES}/toolset_tuning_answers.json")).unwrap()).unwrap();
    let asked = raw["questions"].as_object().unwrap();
    for (name, q) in questions().as_object().unwrap() {
        assert_eq!(&asked[name], q, "{name}");
    }
}

#[test]
fn test_an_answer_is_read_per_toolset_and_a_missing_or_odd_noul_is_zero() {
    let predict = |_: &Value, _: &Value| -> anyhow::Result<Value> {
        Ok(json!({"answers": {"calendar": {"noul": 0.7}, "maps": {"noul": true}, "gmail": "x"}}))
    };
    let a = ask(&predict, "x", "", &zero).unwrap();
    assert_eq!(a.nouls["calendar"], 0.7);
    assert_eq!(a.nouls["maps"], 0.0);
    assert_eq!(a.nouls["gmail"], 0.0);
    assert!(a.error.is_empty());
    assert_eq!(a.nouls.keys().map(String::as_str).collect::<Vec<_>>(), TOOLSETS);
}

#[test]
fn test_a_reply_that_is_not_an_object_is_an_error_answer_and_answers_that_are_not_one_raise() {
    let listy = |_: &Value, _: &Value| -> anyhow::Result<Value> { Ok(json!([1])) };
    let a = ask(&listy, "x", "", &zero).unwrap();
    assert_eq!(a.error, "AttributeError: 'list' object has no attribute 'get'");
    assert!(a.nouls.is_empty());
    let odd = |_: &Value, _: &Value| -> anyhow::Result<Value> { Ok(json!({"answers": "x"})) };
    let e = ask(&odd, "x", "", &zero).unwrap_err();
    assert_eq!((e.kind.as_str(), e.message.as_str()), ("AttributeError", "'str' object has no attribute 'get'"));
    let empty = |_: &Value, _: &Value| -> anyhow::Result<Value> { Ok(json!({"answers": []})) };
    let a = ask(&empty, "x", "", &zero).unwrap();
    assert!(a.error.is_empty() && a.nouls.values().all(|v| *v == 0.0));
}

#[test]
fn test_ask_times_the_call_on_the_clock_it_is_given() {
    let ticks = AtomicUsize::new(0);
    let clock = || ticks.fetch_add(1, Ordering::SeqCst) as f64 * 0.25;
    let (predict, _) = fake(&[]);
    let a = ask(&predict, "x", "", &clock).unwrap();
    assert_eq!(a.ms, 250.0);
}

// ---- the rule -----------------------------------------------------------------------------------------------------

#[test]
fn test_decide_opens_each_toolset_whose_noul_reaches_its_own_gate() {
    let a = answer(&[("calendar", 0.5), ("maps", 0.49), ("share", 0.99)]).with_ms(100.0);
    let mut gates = half();
    gates.insert("share".into(), 1.01);
    assert_eq!(decide(&a, &gates, f64::INFINITY), set(&["calendar"]));
}

#[test]
fn test_a_toolset_without_a_gate_never_opens() {
    let a = answer(&[("calendar", 1.0)]);
    assert!(decide(&a, &IndexMap::new(), f64::INFINITY).is_empty());
}

#[test]
fn test_an_error_or_an_answer_slower_than_the_budget_opens_nothing() {
    assert!(decide(&answer(&[("calendar", 1.0)]).with_error("JevError: x"), &half(), f64::INFINITY).is_empty());
    assert!(decide(&answer(&[("calendar", 1.0)]).with_ms(900.0), &half(), 800.0).is_empty());
    assert_eq!(decide(&answer(&[("calendar", 1.0)]).with_ms(700.0), &half(), 800.0), set(&["calendar"]));
}

#[test]
fn test_a_predict_that_raises_is_an_error_answer_not_a_crash() {
    let predict = |_: &Value, _: &Value| -> anyhow::Result<Value> { Err(Raised::new("TimeoutError", "slow").into()) };
    let a = ask(&predict, "x", "", &zero).unwrap();
    assert!(a.error.starts_with("TimeoutError"), "{}", a.error);
    assert_eq!(a.error, "TimeoutError: slow");
    assert!(decide(&a, &half(), f64::INFINITY).is_empty());
}

#[test]
fn test_a_jev_failure_keeps_its_python_class_name_and_is_clipped_to_160() {
    let predict = |_: &Value, _: &Value| -> anyhow::Result<Value> {
        Err(eve_jev::PredictFailure::Jev(eve_jev::JevError("x".repeat(400))).into())
    };
    let a = ask(&predict, "x", "", &zero).unwrap();
    assert!(a.error.starts_with("JevError: xxx"));
    assert_eq!(a.error.chars().count(), 160);
}

#[test]
fn test_the_answer_round_trips_through_its_json() {
    let a = answer(&[("calendar", 0.123456), ("maps", 0.9)]).with_ms(212.34).with_error("");
    let j = a.to_json();
    assert_eq!(j, json!({"nouls": {"calendar": 0.1235, "maps": 0.9}, "ms": 212.3, "error": ""}));
    assert_eq!(serde_json::to_string(&j).unwrap(), r#"{"nouls":{"calendar":0.1235,"maps":0.9},"ms":212.3,"error":""}"#);
    let back = ToolsetAnswer::from_json(&j).unwrap();
    assert_eq!(back, answer(&[("calendar", 0.1235), ("maps", 0.9)]).with_ms(212.3));
    // Python's float() and str() on what a cache may hold
    let odd = ToolsetAnswer::from_json(&json!({"nouls": {"maps": "0.5", "gmail": true}, "ms": null, "error": 5})).unwrap();
    assert_eq!(odd, answer(&[("maps", 0.5), ("gmail", 1.0)]).with_error("5"));
    assert_eq!(ToolsetAnswer::from_json(&json!({})).unwrap(), ToolsetAnswer::default());
    assert_eq!(ToolsetAnswer::from_json(&json!({"nouls": {"maps": "x"}})).unwrap_err().kind, "ValueError");
    assert_eq!(ToolsetAnswer::from_json(&json!({"nouls": [1]})).unwrap_err().kind, "AttributeError");
    assert_eq!(ToolsetAnswer::from_json(&json!([])).unwrap_err().kind, "AttributeError");
}

#[test]
fn test_every_cached_answer_reads_and_writes_back_the_same() {
    for name in ["toolset_tuning_answers.json", "toolset_holdout_answers.json"] {
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(format!("{ROUTES}/{name}")).unwrap()).unwrap();
        for (id, cached) in raw["answers"].as_object().unwrap() {
            let a = ToolsetAnswer::from_json(cached).unwrap();
            assert_eq!(&a.to_json(), cached, "{name} {id}");
        }
    }
}

// ---- the fit ------------------------------------------------------------------------------------------------------

#[test]
fn test_fit_keeps_the_highest_gate_that_still_reaches_the_tuning_recall() {
    let answers: Vec<ToolsetAnswer> = [0.9, 0.62, 0.3].iter().map(|v| answer(&[("calendar", *v)])).collect();
    let needed = vec![set(&["calendar"]), set(&["calendar"]), set(&[])];
    let gates = fit(&answers, &[set(&[]), set(&[]), set(&[])], &needed, TIMEOUT_S * 1000.0).unwrap();
    assert_eq!(gates["calendar"], 0.6); // 0.65 loses the 0.62 case; 0.6 keeps both, and is the highest
}

#[test]
fn test_fit_credits_keywords_so_a_case_they_catch_never_lowers_the_gate() {
    let answers = vec![answer(&[("calendar", 0.9)]), answer(&[("calendar", 0.1)])];
    let gates = fit(&answers, &[set(&[]), set(&["calendar"])], &[set(&["calendar"]), set(&["calendar"])], TIMEOUT_S * 1000.0)
        .unwrap();
    assert_eq!(gates["calendar"], 0.9);
}

#[test]
fn test_fit_with_no_gate_reaching_the_recall_keeps_the_lowest_on_the_grid() {
    let answers = vec![answer(&[("maps", 0.0)]); 3];
    let gates = fit(&answers, &vec![set(&[]); 3], &vec![set(&["maps"]); 3], TIMEOUT_S * 1000.0).unwrap();
    assert_eq!(gates["maps"], GRID[0]);
}

#[test]
fn test_fit_counts_an_answer_slower_than_the_turn_budget_as_none() {
    let answers = vec![answer(&[("gmail", 0.99)]).with_ms(5000.0)];
    assert_eq!(fit(&answers, &[set(&[])], &[set(&["gmail"])], TIMEOUT_S * 1000.0).unwrap()["gmail"], GRID[0]);
}

#[test]
fn test_fit_refuses_lists_of_different_lengths_as_python_zip_strict_does() {
    let e = fit(&[answer(&[])], &[], &[], 800.0).unwrap_err();
    assert_eq!((e.kind.as_str(), e.message.as_str()), ("ValueError", "zip() argument 2 is shorter than argument 1"));
    let e = fit(&[], &[set(&[])], &[], 800.0).unwrap_err();
    assert_eq!(e.message, "zip() argument 2 is longer than argument 1");
    let e = fit(&[], &[], &[set(&["maps"])], 800.0).unwrap_err();
    assert_eq!(e.kind, "IndexError");
}

#[test]
fn test_recall_is_one_when_nothing_needs_the_toolset() {
    assert_eq!(recall(&[set(&[])], &[set(&[])], "maps").unwrap(), 1.0);
    assert_eq!(recall(&[set(&["maps"]), set(&[])], &[set(&["maps"]), set(&["maps"])], "maps").unwrap(), 0.5);
}

#[test]
fn test_the_grid_is_python_rounded_steps_of_five_hundredths() {
    for (i, g) in GRID.iter().enumerate() {
        assert_eq!(*g, py_round(0.05 * (i + 1) as f64, 2));
    }
}

#[test]
fn test_the_fitted_gates_in_the_module_are_the_ones_the_eval_wrote() {
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{ROUTES}/toolset_gates.json")).unwrap()).unwrap();
    let written: IndexMap<String, f64> =
        written["gates"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_f64().unwrap())).collect();
    assert_eq!(written, gates());
    assert_eq!(gates(), load_gates(&GATES_FILE));
    let names: BTreeSet<String> = gates().keys().cloned().collect();
    assert_eq!(names, set(&TOOLSETS));
    assert!(gates().values().all(|g| GRID.contains(g)));
}

#[test]
fn test_load_gates_falls_back_to_the_module_when_the_file_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(load_gates(&tmp.path().join("nope.json")), gates());
}

#[test]
fn test_load_gates_falls_back_when_a_toolset_is_missing_or_unreadable_and_reads_float_strings() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("g.json");
    std::fs::write(&p, r#"{"gates": {"calendar": 0.5}}"#).unwrap();
    assert_eq!(load_gates(&p), gates());
    std::fs::write(&p, "not json").unwrap();
    assert_eq!(load_gates(&p), gates());
    let all: Map<String, Value> = TOOLSETS.iter().map(|n| (n.to_string(), json!("0.3"))).collect();
    std::fs::write(&p, json!({"gates": all}).to_string()).unwrap();
    assert!(load_gates(&p).values().all(|g| *g == 0.3));
}

/// The datasets scored with the cached answers, as Python's module scores them today (computed 2026-10-20 with
/// toolset_router.py: keywords alone 169/220 on tuning and 98/120 on the holdout; keywords + Jev 218/220 and
/// 118/120; a refit on tuning moves github to 0.7). The tuning counts differ from the module comment's 2026-10-05
/// figures (163, 219) because apps_mcp's words have grown since; the holdout ones still match it. Recomputing
/// them proves the keywords, the decision and the fit agree with Python's on real data.
#[test]
fn test_the_datasets_score_what_the_module_says_they_scored() {
    for (cases_file, answers_file, kw_full, both_full) in [
        ("toolset_tuning.json", "toolset_tuning_answers.json", 169, 218),
        ("toolset_holdout.json", "toolset_holdout_answers.json", 98, 118),
    ] {
        let data: Value = serde_json::from_str(&std::fs::read_to_string(format!("{ROUTES}/{cases_file}")).unwrap()).unwrap();
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(format!("{ROUTES}/{answers_file}")).unwrap()).unwrap();
        let (mut kw_hits, mut both_hits) = (0, 0);
        let mut said_all = Vec::new();
        let mut needed_all = Vec::new();
        for c in data["cases"].as_array().unwrap() {
            let needed: BTreeSet<String> = c["toolsets"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap().to_string())
                .filter(|t| TOOLSETS.contains(&t.as_str()))
                .collect();
            let kw = keyword_toolsets(c["text"].as_str().unwrap(), c["previous"].as_str().unwrap());
            let mut a = ToolsetAnswer::from_json(&raw["answers"][c["id"].as_str().unwrap()]).unwrap();
            a.nouls.retain(|k, _| TOOLSETS.contains(&k.as_str())); // toolset_eval.known
            let said: BTreeSet<String> = kw.union(&decide(&a, &gates(), TIMEOUT_S * 1000.0)).cloned().collect();
            kw_hits += usize::from(needed.is_subset(&kw));
            both_hits += usize::from(needed.is_subset(&said));
            said_all.push(said);
            needed_all.push(needed);
        }
        assert_eq!((kw_hits, both_hits), (kw_full, both_full), "{cases_file}");
        if cases_file == "toolset_tuning.json" {
            // the fit on the tuning set gives what Python's fit gives today
            let answers: Vec<ToolsetAnswer> = data["cases"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| {
                    let mut a = ToolsetAnswer::from_json(&raw["answers"][c["id"].as_str().unwrap()]).unwrap();
                    a.nouls.retain(|k, _| TOOLSETS.contains(&k.as_str()));
                    a
                })
                .collect();
            let kws: Vec<BTreeSet<String>> = data["cases"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| keyword_toolsets(c["text"].as_str().unwrap(), c["previous"].as_str().unwrap()))
                .collect();
            let mut refit = gates();
            refit.insert(GITHUB.to_string(), 0.7);
            assert_eq!(fit(&answers, &kws, &needed_all, TIMEOUT_S * 1000.0).unwrap(), refit);
        }
    }
}

#[test]
fn test_the_datasets_carry_the_toolsets_and_the_one_removed_which_the_eval_drops() {
    // The datasets are history and are not edited: they still label the toolset removed on 2026-10-05 (ordering
    // delivery). The eval reads only the toolsets that exist now.
    let mut removed = BTreeSet::new();
    for name in ["toolset_tuning.json", "toolset_holdout.json"] {
        let data: Value = serde_json::from_str(&std::fs::read_to_string(format!("{ROUTES}/{name}")).unwrap()).unwrap();
        for c in data["cases"].as_array().unwrap() {
            for t in c["toolsets"].as_array().unwrap() {
                let t = t.as_str().unwrap();
                if !TOOLSETS.contains(&t) {
                    removed.insert(t.to_string());
                }
            }
            for key in ["id", "previous", "text", "toolsets"] {
                assert!(c.get(key).is_some(), "{name} {key}");
            }
        }
    }
    assert_eq!(removed.len(), 1);
}

// ---- keywords -----------------------------------------------------------------------------------------------------

#[test]
fn test_the_own_keywords_catch_the_plain_asks() {
    for (text, toolset) in [
        ("text ana the deck", SHARE),
        ("email the cover letter on my desktop to jordan", SHARE),
        ("imessage dad the screenshot from my desktop", SHARE),
        ("shoot the resume to my mom", SHARE),
        ("any slack dms", WORKSPACE),
        ("whats new in #eng", WORKSPACE),
        ("did anyone @ me", WORKSPACE),
        ("check my linear tickets", WORKSPACE),
        ("open the notion page for the hackathon", WORKSPACE),
    ] {
        assert!(keyword_toolsets(text, "").contains(toolset), "{text}");
    }
}

#[test]
fn test_the_own_keywords_stay_off_the_hard_negatives() {
    for (text, toolset) in [
        ("send me the pdf", SHARE),
        ("open slack on my mac", WORKSPACE),
        ("notion vs obsidian?", WORKSPACE),
        ("lights blue", WORKSPACE),
    ] {
        assert!(!keyword_toolsets(text, "").contains(toolset), "{text}");
    }
}

#[test]
fn test_keywords_use_apps_mcp_words_for_the_direct_apps() {
    assert!(keyword_toolsets("my next class and a maps link", "").is_superset(&set(&[CALENDAR, MAPS])));
    assert!(keyword_toolsets("any open prs on eve", "").contains(GITHUB));
}

#[test]
fn test_a_follow_up_that_points_back_adds_the_previous_messages_keywords_and_a_plain_one_does_not() {
    assert_eq!(keyword_toolsets("and send it to ana", "find my resume in drive"), set(&[DRIVE, SHARE]));
    assert_eq!(keyword_toolsets("dimmer", "lights blue"), set(&[]));
    assert!(!keyword_toolsets("and send it to ana", "").contains(DRIVE));
    // test_telegram_toolsets.py's hand-off: a share with no share word, so the words alone open nothing
    assert_eq!(keyword_toolsets("get my resume over to ana before her interview", ""), set(&[]));
}

#[test]
fn test_work_mail_and_calendar_open_the_workspace_where_composio_holds_them() {
    // 2026-10-05: the owner's work Gmail and calendar are Composio's (composio_tools.OTHER_ACCOUNT_TOOLKITS).
    for (text, opens) in [
        ("anything new in my work email", true),
        ("whats on my work calendar tmrw", true),
        ("check the era inbox", true),
        ("i have work tomorrow", false),
        ("email me the work schedule pdf", false),
    ] {
        assert_eq!(keyword_toolsets(text, "").contains("workspace"), opens, "{text}");
    }
}

/// Every routing-dataset message (and a set of edge cases: Unicode case folding, whitespace, the follow-up's
/// twelve-word limit) through Python's `keyword_toolsets` and `_words`, recorded once in
/// tests/fixtures/keywords_parity.json; Rust must name the same toolsets for each.
#[test]
fn test_the_keywords_match_python_on_every_dataset_message() {
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/keywords_parity.json");
    let cases: Value = serde_json::from_str(&std::fs::read_to_string(fixture).unwrap()).unwrap();
    let mut wrong = Vec::new();
    for c in cases.as_array().unwrap() {
        let (text, previous) = (c["text"].as_str().unwrap(), c["previous"].as_str().unwrap());
        let want = |k: &str| -> BTreeSet<String> {
            c[k].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect()
        };
        let got = keyword_toolsets(text, previous);
        if got != want("keywords") {
            wrong.push(format!("keyword_toolsets({text:?}, {previous:?}) = {got:?}, Python {:?}", want("keywords")));
        }
        let plain = words(&collapse_ws(text));
        if plain != want("words") {
            wrong.push(format!("_words({text:?}) = {plain:?}, Python {:?}", want("words")));
        }
    }
    assert!(cases.as_array().unwrap().len() > 300);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// test_github_repos.py: one of the owner's repository names opens GitHub as a word (apps_mcp's REPO_NAMES).
#[test]
fn test_a_repository_name_opens_github() {
    eve_apps_mcp::set_repo_names(["zorbcar".to_string()].into_iter().collect());
    let named = keyword_toolsets("what's failing on zorbcar", "");
    let plain = keyword_toolsets("can you remix this", "");
    eve_apps_mcp::set_repo_names(BTreeSet::new());
    assert!(named.contains("github"));
    assert!(!plain.contains("github"));
}

// ---- toolsets_for: keywords always, Jev only adds -----------------------------------------------------------------

#[test]
fn test_toolsets_for_is_keywords_union_jev() {
    let route = router_from(fake(&[(CALENDAR, 0.9), (MAPS, 0.1)]).0, zero, Some(half()));
    assert_eq!(toolsets_for("my next class and a maps link", "", Some(&route), TIMEOUT_S), set(&[CALENDAR, MAPS]));
    let route = router_from(fake(&[(CALENDAR, 0.9)]).0, zero, Some(half()));
    assert_eq!(toolsets_for("whens my thing w the advisor", "", Some(&route), TIMEOUT_S), set(&[CALENDAR]));
}

#[test]
fn test_jev_hears_the_previous_message_on_a_follow_up() {
    let (predict, seen) = fake(&[(DRIVE, 0.95), (SHARE, 0.95)]);
    let route = router_from(predict, zero, Some(half()));
    assert_eq!(toolsets_for("and send it to ana", "find my resume in drive", Some(&route), TIMEOUT_S), set(&[DRIVE, SHARE]));
    assert_eq!(seen.lock()[0].0["previous"], "find my resume in drive");
}

#[test]
fn test_a_router_with_no_gates_or_empty_gates_reads_the_module_gates() {
    // `gates or GATES`: share's gate is 0.95, so a 0.9 share stays shut while a 0.9 calendar opens
    let values = [(CALENDAR, 0.9), (SHARE, 0.9)];
    let none = router_from(fake(&values).0, zero, None);
    let empty = router_from(fake(&values).0, zero, Some(IndexMap::new()));
    assert_eq!(none("x", "").unwrap().toolsets, set(&[CALENDAR]));
    assert_eq!(empty("x", "").unwrap().toolsets, set(&[CALENDAR]));
}

#[test]
fn test_a_router_that_times_out_leaves_the_keywords_alone() {
    let (release, wait) = mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    let slow: Router = Arc::new(move |_: &str, _: &str| {
        let _ = wait.lock().recv_timeout(Duration::from_secs(2));
        Ok(Route::new([GITHUB], 0.0, ""))
    });
    let out = route_turn("any slack dms", "", Some(&slow), 0.05);
    let _ = release.send(());
    assert_eq!(out.toolsets(), set(&[WORKSPACE]));
    assert!(out.asked);
    assert_eq!(out.error, "timeout");
    assert!(out.ms >= 50.0 && out.ms < 2000.0, "{}", out.ms);
}

#[test]
fn test_a_router_that_raises_or_errs_or_is_none_leaves_the_keywords_alone() {
    let boom: Router = Arc::new(|_: &str, _: &str| Err(Raised::new("RuntimeError", "x").into()));
    assert_eq!(toolsets_for("any slack dms", "", Some(&boom), TIMEOUT_S), set(&[WORKSPACE]));
    let r = route_turn("any slack dms", "", Some(&boom), TIMEOUT_S);
    assert_eq!((r.error.as_str(), r.ms, r.asked), ("RuntimeError", 0.0, true));
    let erring = router_from(
        |_: &Value, _: &Value| -> anyhow::Result<Value> { Err(Raised::new("TimeoutError", "").into()) },
        zero,
        Some(half()),
    );
    assert_eq!(toolsets_for("any slack dms", "", Some(&erring), TIMEOUT_S), set(&[WORKSPACE]));
    assert_eq!(route_turn("any slack dms", "", Some(&erring), TIMEOUT_S).error, "TimeoutError: ");
    assert_eq!(toolsets_for("any slack dms", "", None, TIMEOUT_S), set(&[WORKSPACE]));
    let off = route_turn("any slack dms", "", None, TIMEOUT_S);
    assert!(!off.asked && off.ms == 0.0 && off.error.is_empty());
}

#[test]
fn test_a_router_whose_answers_are_malformed_errs_with_the_python_class() {
    // ask raises AttributeError outside its try; the worker catches it and the keywords stand
    let odd = router_from(|_: &Value, _: &Value| -> anyhow::Result<Value> { Ok(json!({"answers": [1]})) }, zero, None);
    let r = route_turn("any slack dms", "", Some(&odd), TIMEOUT_S);
    assert_eq!(r.error, "AttributeError");
    assert_eq!(r.toolsets(), set(&[WORKSPACE]));
}

#[test]
fn test_a_route_naming_toolsets_that_are_not_routed_adds_only_the_routed_ones() {
    let r: Router = Arc::new(|_: &str, _: &str| Ok(Route::new(["calendar", "contacts", "menu", "nope"], 180.0, "")));
    let out = route_turn("hi", "", Some(&r), TIMEOUT_S);
    assert_eq!(out.jev, set(&[CALENDAR]));
    assert_eq!(out.ms, 180.0);
}

// ---- found by the adversarial check of RoutedTurn.lean (test_lean_routedturn_replay.py) ----------------------

const ASKED: &str = "when is the dentist on my calendar"; // the keywords alone open the calendar; not a rundown

#[test]
fn test_a_router_that_raises_a_base_exception_still_leaves_the_keyword_toolsets() {
    assert_eq!(keyword_toolsets(ASKED, ""), set(&[CALENDAR]));
    let gone: Router = Arc::new(|_: &str, _: &str| -> anyhow::Result<Route> { std::panic::panic_any("gone") });
    let r = route_turn(ASKED, "", Some(&gone), 2.0);
    assert!(r.toolsets().contains(CALENDAR));
    assert!(r.asked && !r.error.is_empty());
    assert_eq!(r.error, "not a route");
}

#[test]
fn test_a_route_whose_ms_is_not_a_number_still_formats() {
    let r: Router = Arc::new(|_: &str, _: &str| Ok(Route::new(Vec::<String>::new(), f64::NAN, "")));
    let out = route_turn(ASKED, "", Some(&r), 2.0);
    assert!(out.toolsets().contains(CALENDAR));
    assert_eq!(out.log_line(), "calendar (keywords); jev nan ms");
}

// ---- the log line -------------------------------------------------------------------------------------------------

#[test]
fn test_the_log_line_names_where_each_toolset_came_from_and_never_the_message() {
    let t = TurnToolsets {
        keywords: set(&["maps"]),
        jev: set(&["calendar", "maps"]),
        ms: 212.4,
        asked: true,
        error: String::new(),
    };
    assert_eq!(t.log_line(), "calendar (jev), maps (both); jev 212 ms");
    assert_eq!(TurnToolsets { keywords: set(&["share"]), ..Default::default() }.log_line(), "share (keywords); jev off");
    let timeout = TurnToolsets { asked: true, ms: 800.6, error: "timeout".into(), ..Default::default() };
    assert_eq!(timeout.log_line(), "none; jev 801 ms (timeout)");
    // Python's .0f rounds half to even on the exact value
    assert_eq!(py_f0(0.5), "0");
    assert_eq!(py_f0(1.5), "2");
    assert_eq!(py_f0(2.5), "2");
    assert_eq!(py_f0(f64::INFINITY), "inf");
}

// ---- the switch ---------------------------------------------------------------------------------------------------

#[test]
fn test_the_mode_reads_eve_toolset_router() {
    for (raw, want) in [("", "auto"), ("on", "on"), ("1", "on"), ("off", "off"), ("no", "off"), ("bogus", "auto"),
                        (" ON ", "on"), ("True", "on"), ("FALSE", "off")] {
        assert_eq!(mode(&env(&[("EVE_TOOLSET_ROUTER", raw)])), want, "{raw:?}");
    }
    assert_eq!(mode(&env(&[])), "auto");
}

#[test]
fn test_make_router_follows_the_mode_and_shipped_and_needs_a_jev_key() {
    let cache = JevCache::new();
    let key = [("OPENROUTER_API_KEY", "k"), ("EVE_JEV_ROUTE", "openrouter")];
    let with = |mode: &str| {
        let mut e = env(&key);
        e.insert("EVE_TOOLSET_ROUTER".into(), mode.into());
        e
    };
    assert!(make_router_in(&cache, &with("off"), true).is_none());
    assert!(make_router_in(&cache, &env(&[("EVE_TOOLSET_ROUTER", "on"), ("EVE_JEV_ROUTE", "openrouter")]), true).is_none());
    let on = make_router_in(&cache, &with("on"), true).unwrap();
    assert!(Arc::ptr_eq(&on, &make_router_in(&cache, &with("on"), true).unwrap())); // once a process
    assert!(make_router_in(&cache, &with("auto"), false).is_none());
    assert!(Arc::ptr_eq(&on, &make_router_in(&cache, &with("auto"), true).unwrap()));
    cache.clear();
    assert!(!Arc::ptr_eq(&on, &make_router_in(&cache, &with("on"), true).unwrap()));
}

#[test]
fn test_a_rotated_key_makes_a_new_router() {
    // Review A10 (2026-10-18): the cache was keyed on (url, model), so a rotated key kept the old one for the
    // process's life. The key's fingerprint (never the key) is part of the cache key now.
    let cache = JevCache::new();
    let make = |k: &str| {
        make_router_in(
            &cache,
            &env(&[("OPENROUTER_API_KEY", k), ("EVE_JEV_ROUTE", "openrouter"), ("EVE_TOOLSET_ROUTER", "on")]),
            true,
        )
    };
    let old = make("k-old").unwrap();
    let new = make("k-new").unwrap();
    assert!(!Arc::ptr_eq(&old, &new));
    let shown = format!("{cache:?}");
    assert!(!shown.contains("k-new") && !shown.contains("k-old"), "{shown}");
    // the fingerprint is Python's: sha256 of the key, first 16 hex digits
    assert!(shown.contains(&hex::encode(Sha256::digest(b"k-new"))[..16]), "{shown}");
    assert!(shown.contains("typesafe/jev-1.13"));
}

// ---- the always-carried toolsets (test_menu_tools.py, test_events.py) ----------------------------------------

#[test]
fn test_the_always_carried_toolsets_are_outside_the_routed_ones() {
    let menu = ALWAYS.iter().find(|(n, _)| *n == MENU).unwrap().1;
    assert_eq!(
        menu,
        ["spend_report", "daily_rundown", "claude_relay", "codex_relay", "set_stealth", "open_apps", "pair_eve_link", "read_file"]
    );
    assert_eq!(ALWAYS.iter().find(|(n, _)| *n == EVENTS).unwrap().1, ["events_search"]);
    assert!(ALWAYS.iter().all(|(n, _)| !TOOLSETS.contains(n)));
    assert!(menu.iter().all(|t| always_toolset(t) == MENU));
    assert_eq!(always_toolset("events_search"), EVENTS);
    assert_eq!(always_toolset("web_search"), "");
}
