//! Reading the owner's yes or no, for every reply that gates an action.
//!
//! A permission prompt from Claude Code, a Composio app action, a browser step's approval: each waits for the
//! owner's next message and acts on a yes. The rule used to be a prefix match ("y" admitted "yikes, no"), in
//! two different word orders at two sites and an exact set at a third. It is one rule now, and it fails
//! closed:
//!
//! * **yes**: the first word is a yes-word (or the reply opens with "do it" / "go ahead"), AND no word of
//!   the reply negates or holds it ("yeah no", "ok wait", "sure, but don't", "do not").
//! * **no**: the first word is a no-word.
//! * anything else is neither: nothing is approved, and a caller that can defer (a permission prompt) leaves
//!   the decision to the dialog on the Mac.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

pub const YES_WORDS: &[&str] = &[
    "yes", "y", "yeah", "yep", "yup", "ya", "ok", "okay", "k", "sure", "go", "allow", "allowed", "approve",
    "approved", "confirm", "confirmed", "affirmative", "proceed", "absolutely", "definitely",
];
pub const YES_OPENINGS: &[&str] = &["do it", "go ahead", "go for it", "sounds good", "of course"];
pub const NO_WORDS: &[&str] = &[
    "no", "n", "nope", "nah", "deny", "denied", "stop", "don't", "dont", "block", "cancel", "decline", "reject",
    "never", "negative",
];
/// A yes that also says one of these is not a yes: it takes it back, or asks to hold.
pub const HOLD_WORDS: &[&str] = &[
    "no", "not", "nope", "nah", "don't", "dont", "never", "stop", "wait", "cancel", "hold", "deny", "block",
    "but", "unless", "later",
];

// A bare answer is short and made only of these: the yes and no words, the words of the yes openings, and a
// little politeness. "yes", "no, leave it", "go ahead please" are bare; "ok, also update the README" and
// "go check the logs" are sentences that happen to open with a yes-word (owner, 2026-09-23).
pub const BARE_MAX_WORDS: usize = 4;
pub const BARE_EXTRA_WORDS: &[&str] = &["please", "thanks", "thank", "you", "leave", "it", "that", "this", "one", "now"];

static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z]+(?:'[a-z]+)?").expect("valid word regex"));

static BARE_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    YES_WORDS
        .iter()
        .chain(NO_WORDS)
        .chain(BARE_EXTRA_WORDS)
        .copied()
        .chain(YES_OPENINGS.iter().flat_map(|o| o.split_whitespace()))
        .collect()
});

fn words(reply: &str) -> Vec<String> {
    let lowered = reply.to_lowercase().replace('\u{2019}', "'");
    WORD.find_iter(&lowered).map(|m| m.as_str().to_string()).collect()
}

/// The reply is an unambiguous yes.
pub fn approves(reply: &str) -> bool {
    let words = words(reply);
    let Some(first) = words.first() else {
        return false;
    };
    let joined = words.join(" ");
    let opens = YES_WORDS.contains(&first.as_str()) || YES_OPENINGS.iter().any(|o| joined.starts_with(o));
    opens && !words.iter().any(|w| HOLD_WORDS.contains(&w.as_str()))
}

/// The reply is a no.
pub fn denies(reply: &str) -> bool {
    words(reply).first().is_some_and(|w| NO_WORDS.contains(&w.as_str()))
}

/// "allow", "deny", or "" when the reply is neither (the caller defers).
pub fn decision(reply: &str) -> &'static str {
    if approves(reply) {
        "allow"
    } else if denies(reply) {
        "deny"
    } else {
        ""
    }
}

/// "allow" or "deny" only when the whole reply is a yes or a no, else "". For a prompt that has another
/// way to be answered (Allow/Deny buttons on the screen), where anything that is not plainly the answer
/// must go on to where it was meant to go instead of being eaten. [`decision`] stays the rule where the
/// next message is the answer by construction.
pub fn bare_decision(reply: &str) -> &'static str {
    let words = words(reply);
    if words.is_empty() || words.len() > BARE_MAX_WORDS || words.iter().any(|w| !BARE_WORDS.contains(w.as_str())) {
        return "";
    }
    decision(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a_clear_yes() {
        for reply in [
            "yes", "Yes.", "y", "yeah", "yep", "ok", "okay", "OK!", "sure", "go", "allow", "approve", "approved",
            "do it", "go ahead", "yes please", "ok go ahead", "sure thing", "confirm", "yes, do it", "  yes  ",
        ] {
            assert!(approves(reply) && decision(reply) == "allow", "{reply:?}");
        }
    }

    #[test]
    fn test_anything_less_is_not_a_yes() {
        for reply in [
            "yikes, no", // the old prefix rule read this as yes: it starts with "y"
            "you know what, no",
            "yeah no",
            "ok wait",
            "sure, but don't send it",
            "do not",
            "yes but only once",
            "ok not now",
            "okay later",
            "",
            "???",
            "maybe",
            "what is it doing?",
        ] {
            assert!(!approves(reply), "{reply:?}");
        }
    }

    #[test]
    fn test_go_away_is_honestly_a_yes_by_this_rule() {
        // The first word is a yes-word and nothing holds it. Pinned so a change here is a decision, not an accident.
        assert!(approves("go away"));
    }

    #[test]
    fn test_a_clear_no() {
        for reply in [
            "no", "No.", "n", "nope", "nah", "deny", "stop", "don't", "dont do that", "block it", "cancel", "never",
        ] {
            assert!(denies(reply) && decision(reply) == "deny", "{reply:?}");
        }
    }

    #[test]
    fn test_neither_defers() {
        for reply in ["maybe", "hmm", "what does it do", "", "yeah no"] {
            assert_eq!(decision(reply), "", "{reply:?}");
        }
    }

    #[test]
    fn test_curly_apostrophes_are_read() {
        assert!(denies("don\u{2019}t") && !approves("ok don\u{2019}t"));
    }

    #[test]
    fn test_a_bare_answer_is_read() {
        for (reply, verdict) in [
            ("yes", "allow"),
            ("Yes!", "allow"),
            ("go ahead", "allow"),
            ("yes go", "allow"),
            ("ok please", "allow"),
            ("do it", "allow"),
            ("no", "deny"),
            ("no, leave it", "deny"),
            ("nope", "deny"),
            ("deny", "deny"),
        ] {
            assert_eq!(bare_decision(reply), verdict, "{reply:?}");
        }
    }

    #[test]
    fn test_a_sentence_that_opens_with_yes_or_no_is_not_a_bare_answer() {
        for reply in [
            "ok, also update the README when you're done",
            "no worries, keep going with X",
            "go check the logs",
            "yes but first run the tests",
            "maybe",
            "",
            "no worries",
        ] {
            assert_eq!(bare_decision(reply), "", "{reply:?}");
        }
    }

    // Parity details of the Python rule beyond its test file.

    #[test]
    fn test_opening_is_a_string_prefix_of_the_joined_words() {
        // Python: " ".join(words).startswith(YES_OPENINGS), so punctuation between words does not matter.
        assert!(approves("Do, it!"));
        assert!(approves("of course"));
        assert!(!approves("do"));
    }

    #[test]
    fn test_word_pattern_takes_one_apostrophe_group() {
        assert_eq!(words("Don't've X1y"), vec!["don't", "ve", "x", "y"]);
    }
}
