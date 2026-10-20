//! Differential parity against Python, from `fixtures/parity.json`
//! (regenerate with `fixtures/gen_fixtures.py`): every pattern compiles
//! exactly when Python's `re.compile` does and `search` answers what
//! `re.search` answers; `is_simple` and `classify_command` agree with the
//! Python module on a few thousand commands.

use std::path::Path;

use eve_matchers::{
    classify_command, is_simple, load_config, MatcherConfig, Pattern, PatternError,
};
use serde_json::Value;

/// Patterns the port knowingly refuses where Python compiles (PARITY.md,
/// Differences). The test fails if one starts to agree or differs otherwise.
const KNOWN: &[(&str, &str)] = &[(
    r"\N{LATIN SMALL LETTER A}",
    "no Unicode name table in the port",
)];

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/parity.json")).expect("parity.json")
}

#[test]
fn regex_translation_matches_python_re() {
    let f = fixture();
    let texts: Vec<&str> = f["texts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    let mut diffs = Vec::new();
    for case in f["regex"].as_array().unwrap() {
        let p = case["pattern"].as_str().unwrap();
        let compiles = case["compiles"].as_str().unwrap();
        if let Some(why) = KNOWN.iter().find(|(k, _)| *k == p).map(|(_, why)| why) {
            // Recorded difference: Python compiles it, the port refuses it.
            assert_eq!(compiles, "ok", "{p:?}: {why}");
            assert!(
                matches!(Pattern::new(p), Err(PatternError::Bad(_))),
                "{p:?}"
            );
            continue;
        }
        match (Pattern::new(p), compiles) {
            (Err(PatternError::RepetitionTooLarge), "overflow") => {}
            (Ok(pat), "ok") => {
                for (t, want) in texts.iter().zip(case["matches"].as_array().unwrap()) {
                    let got = pat.search(t).expect("search");
                    if got != want.as_bool().unwrap() {
                        diffs.push(format!("{p:?} on {t:?}: rust {got}, python {want}"));
                    }
                }
            }
            (Err(PatternError::Bad(_)), "error") => {}
            (got, want) => diffs.push(format!("{p:?}: rust {got:?}, python {want}")),
        }
    }
    assert!(
        diffs.is_empty(),
        "{} differences:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

#[test]
fn is_simple_and_classify_match_python() {
    let f = fixture();
    let cfg = load_config(Some(Path::new("/nonexistent.toml"))).unwrap();
    let strict = MatcherConfig {
        strict: true,
        ..cfg.clone()
    };
    let mut diffs = Vec::new();
    let cases = f["commands"].as_array().unwrap();
    for case in cases {
        let c = case["command"].as_str().unwrap();
        if is_simple(c) != case["is_simple"].as_bool().unwrap() {
            diffs.push(format!("is_simple({c:?})"));
        }
        if classify_command(c, &cfg) != case["default"].as_str().unwrap() {
            diffs.push(format!("classify({c:?}) = {}", classify_command(c, &cfg)));
        }
        if classify_command(c, &strict) != case["strict"].as_str().unwrap() {
            diffs.push(format!(
                "strict classify({c:?}) = {}",
                classify_command(c, &strict)
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "{} differences:\n{}",
        diffs.len(),
        diffs[..diffs.len().min(30)].join("\n")
    );
    // The corpus exercises every answer.
    for want in ["allow", "ask", "default"] {
        assert!(cases.iter().any(|c| c["default"] == want), "no {want} case");
    }
}
