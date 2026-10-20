//! Port of bridge/tests/test_thought_screen.py: what earns a place on the robot's screen. Pure — a
//! hand-rolled clock, no board, no model.

use std::collections::BTreeSet;

use super::*;

const VENT: &str = "The vent is annoyingly centered today, as if it were posing.";
const VENT_AGAIN: &str = "The vent is still exactly where it plotted to be, posing again.";
const PLANT: &str = "Someone carried a tall plant past the window and did not come back.";

// Anything that clears the floor without being remarkable: importance=6, novelty=5, cool=0.3.
const GI: i64 = 6;
const GN: i64 = 5;
const GC: f64 = 0.3;

fn screen() -> ThoughtScreen {
    ThoughtScreen::default()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn offer(s: &mut ThoughtScreen, now: f64, text: &str, tags: &[&str]) -> bool {
    s.offer(now, text, tags, GI, GN, GC)
}

fn refusal(s: &ThoughtScreen, now: f64, text: &str, tags: &[&str]) -> String {
    s.refusal(now, text, tags, GI, GN, GC).unwrap_or_default()
}

// ---- the measures ---------------------------------------------------------------------------

#[test]
fn test_content_words_drop_the_words_that_say_nothing() {
    assert_eq!(content_words("The vent is still where it was"), set(&["vent"]));
    assert_eq!(
        content_words("Someone left a blue mug on the desk"),
        set(&["someone", "left", "blue", "mug", "desk"])
    );
    assert_eq!(content_words(""), BTreeSet::new());
}

#[test]
fn test_similarity_is_one_for_the_same_sentence_and_zero_for_unrelated() {
    assert_eq!(similarity(VENT, VENT), 1.0);
    assert_eq!(similarity(VENT, PLANT), 0.0);
    assert_eq!(similarity("", VENT), 0.0);
}

/// The finding that shapes this module: eve's real repeats do not look
/// alike enough for any threshold anyone ships to catch them. The subject
/// rule has to do the work.
#[test]
fn test_surface_similarity_alone_does_not_separate_the_repetitive_pair() {
    assert!(similarity(VENT, VENT_AGAIN) < 0.5);
    assert!(similarity(VENT, PLANT) < similarity(VENT, VENT_AGAIN));
}

#[test]
fn test_trigrams_catch_a_repeated_turn_of_phrase() {
    let a = "the blue mug has a friend now, apparently";
    let b = "over by the window, blue mug has friend energy";
    assert!(trigrams("blue mug has friend").intersection(&trigrams(a)).next().is_some());
    assert!(trigrams(a).intersection(&trigrams("nothing whatsoever in common here")).next().is_none());
    assert!(trigrams("two words").is_empty());
    assert!(similarity(a, b) < 0.6); // not similar enough to veto on words alone
    assert!(trigrams(a).intersection(&trigrams(b)).next().is_some()); // ... but the phrase repeats
}

#[test]
fn test_tag_overlap() {
    assert_eq!(tag_overlap(&set(&["vent"]), &set(&["vent"])), 1.0);
    assert_eq!(tag_overlap(&set(&["vent", "wall"]), &set(&["vent", "desk"])), 1.0 / 3.0);
    assert_eq!(tag_overlap(&set(&[]), &set(&["vent"])), 0.0);
}

// ---- the floor ------------------------------------------------------------------------------

#[test]
fn test_a_dull_thought_never_reaches_the_screen() {
    let mut s = screen();
    assert!(!s.offer(0.0, PLANT, ["plant"], 3, 4, 0.2));
    assert!(s.refusal(0.0, PLANT, ["plant"], 3, 4, 0.2).unwrap_or_default().contains("below the floor"));
    assert!(s.shown.is_empty());
}

#[test]
fn test_any_one_of_the_three_clears_the_floor() {
    for (i, n, c) in [(6, 1, 0.0), (1, 7, 0.0), (1, 1, 0.70)] {
        assert!(screen().offer(0.0, PLANT, ["plant"], i, n, c), "{i} {n} {c}");
    }
}

/// The diary also writes after three hours of silence, which keeps thoughts
/// that are dull by construction; those must not reach the screen.
#[test]
fn test_the_floor_is_not_whether_the_diary_wrote_it() {
    let mut s = screen();
    assert!(!s.offer(0.0, "Nothing has moved since breakfast.", ["desk"], 2, 3, 0.1));
}

#[test]
fn test_an_empty_thought_is_never_shown() {
    let mut s = screen();
    assert!(!offer(&mut s, 0.0, "   ", &["x"]));
    assert_eq!(s.refusal(0.0, "", [] as [&str; 0], GI, GN, GC).as_deref(), Some("empty"));
}

// ---- repetition -----------------------------------------------------------------------------

#[test]
fn test_the_first_thought_goes_up() {
    let mut s = screen();
    assert!(offer(&mut s, 0.0, VENT, &["vent"]));
    assert!(s.shown.len() == 1 && s.refused == 0);
}

#[test]
fn test_a_new_sentence_about_the_same_subject_is_still_the_same_subject() {
    let mut s = screen();
    offer(&mut s, 0.0, VENT, &["vent"]);
    let now = MIN_INTERVAL_SECS + 1.0;
    let fresh = "Nothing up there has moved since breakfast, which is its own kind of news.";
    assert!(similarity(fresh, VENT) < s.similarity); // words alone would let it through
    assert!(!offer(&mut s, now, fresh, &["vent", "wall"]));
    assert!(refusal(&s, now, fresh, &["vent", "wall"]).contains("vent"));
    // After the subject window it is fair game again.
    let later = now + s.subject_window_secs + 1.0;
    assert!(offer(&mut s, later, fresh, &["vent", "wall"]));
}

#[test]
fn test_a_subject_gets_only_so_many_turns_in_a_day() {
    let mut s = ThoughtScreen { topic_limit: 2, ..ThoughtScreen::default() };
    let lines = [
        "The vent presides over everything, aloof as a magistrate.",
        "Four slats up there, and not one of them has ever moved.",
        "Grille duty must be dull work; nobody thanks it.",
        "Somebody should dust that thing before spring arrives.",
    ];
    let mut t = 0.0;
    for line in &lines[..2] {
        assert!(offer(&mut s, t, line, &["vent"]), "{line}");
        t += s.subject_window_secs + 1.0; // past the acute veto each time
    }
    assert!(!offer(&mut s, t, lines[2], &["vent"]));
    assert!(refusal(&s, t, lines[2], &["vent"]).contains("today already"));
    // ... and past the day window it may return.
    let back = t + s.topic_window_secs;
    assert!(offer(&mut s, back, lines[3], &["vent"]));
}

#[test]
fn test_a_repeated_turn_of_phrase_is_refused_even_with_different_tags() {
    let mut s = screen();
    offer(&mut s, 0.0, "The blue mug has a friend now on the shelf.", &["mug"]);
    let now = MIN_INTERVAL_SECS + 1.0;
    assert!(!offer(&mut s, now, "Down by the door, blue mug has friend energy again.", &["door"]));
    assert!(refusal(&s, now, "Down by the door, blue mug has friend energy.", &["door"])
        .contains("phrase already used"));
}

#[test]
fn test_a_near_literal_restatement_is_refused_by_the_backstop() {
    let mut s = screen();
    offer(&mut s, 0.0, PLANT, &["plant"]);
    let now = MIN_INTERVAL_SECS + 1.0;
    assert!(!offer(&mut s, now, "Someone carried a tall plant past the window.", &["greenery"]));
}

/// Nearly every frame has a desk in it. If 'desk' counted as the subject,
/// eve would fall silent after its first remark.
#[test]
fn test_ubiquitous_tags_stop_identifying_anything() {
    let mut s = ThoughtScreen { subject_window_secs: 1e9, ..ThoughtScreen::default() };
    let mut t = 0.0;
    for (thing, line) in [
        ("vent", "The vent, aloof as ever, presides."),
        ("mug", "A mug appears, steaming quietly."),
        ("chair", "The chair has been pushed in neatly."),
        ("window", "Rain has started against the glass."),
    ] {
        assert!(offer(&mut s, t, line, &["desk", thing]), "{line}");
        t += MIN_INTERVAL_SECS + 1.0;
    }
    assert!(s.ubiquitous_tags().contains("desk"));
    assert_eq!(s.subject_of(["desk", "lamp"]), set(&["lamp"]));
    // A thought about something new still gets through despite sharing "desk".
    assert!(offer(&mut s, t, "A parcel has arrived and nobody has opened it.", &["desk", "parcel"]));
}

#[test]
fn test_a_thought_whose_tags_are_all_ubiquitous_keeps_them() {
    let mut s = ThoughtScreen { subject_window_secs: 1e9, ..ThoughtScreen::default() };
    let mut t = 0.0;
    for line in [
        "The desk is tidy today, unusually.",
        "A pen rolled off the desk edge.",
        "The desk lamp is warm already.",
        "Papers cover the desk corner now.",
    ] {
        offer(&mut s, t, line, &["desk"]);
        t += MIN_INTERVAL_SECS + 1.0;
    }
    assert_eq!(s.subject_of(["desk"]), set(&["desk"])); // nothing else to fall back on
}

// ---- rate -----------------------------------------------------------------------------------

#[test]
fn test_the_screen_stays_quiet_between_captions() {
    let mut s = screen();
    offer(&mut s, 0.0, VENT, &["vent"]);
    assert!(!offer(&mut s, MIN_INTERVAL_SECS - 1.0, PLANT, &["plant"]));
    assert!(refusal(&s, 10.0, PLANT, &["plant"]).contains("since the last one"));
    assert!(offer(&mut s, MIN_INTERVAL_SECS + 1.0, PLANT, &["plant"]));
}

#[test]
fn test_an_hourly_cap_stops_a_talkative_hour() {
    let mut s = ThoughtScreen { per_hour: 3, min_interval_secs: 1.0, ..ThoughtScreen::default() };
    for (i, line) in ["A parcel arrived unopened.", "Rain against the glass now.", "The kettle went cold."]
        .iter()
        .enumerate()
    {
        let tag = format!("t{i}");
        assert!(s.offer(i as f64 * 10.0, line, [tag.as_str()], GI, GN, GC), "{line}");
    }
    assert!(!offer(&mut s, 40.0, "Somebody moved the chair by the door.", &["chair"]));
    assert!(refusal(&s, 40.0, "Another line entirely.", &["z"]).contains("already this hour"));
    assert!(offer(&mut s, 3700.0, "Somebody moved the chair by the door.", &["chair"]));
}

/// Nothing is held back to show later: by then eve is looking elsewhere.
#[test]
fn test_a_refused_thought_is_dropped_not_queued() {
    let mut s = screen();
    offer(&mut s, 0.0, VENT, &["vent"]);
    offer(&mut s, 10.0, PLANT, &["plant"]); // refused, too soon
    assert_eq!(s.shown.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), vec![VENT]);
    assert!(offer(&mut s, MIN_INTERVAL_SECS + 1.0, "A third remark, about the door this time.", &["door"]));
    assert!(s.shown.last().unwrap().text.starts_with("A third remark"));
}

#[test]
fn test_only_the_recent_captions_are_compared_against() {
    let mut s = ThoughtScreen {
        history: 2,
        min_interval_secs: 1.0,
        subject_window_secs: 1.0,
        topic_limit: 99,
        ..ThoughtScreen::default()
    };
    offer(&mut s, 0.0, VENT, &["vent"]);
    offer(&mut s, 10.0, "A remark about the kettle and its habits.", &["kettle"]);
    offer(&mut s, 20.0, "A remark about the doorway and its draught.", &["door"]);
    assert!(offer(&mut s, 30.0, VENT, &["vent"]));
}

#[test]
fn test_history_does_not_grow_without_bound() {
    let mut s = ThoughtScreen {
        min_interval_secs: 1.0,
        per_hour: 1000,
        history: 4,
        subject_window_secs: 0.0,
        topic_limit: 1_000_000,
        ..ThoughtScreen::default()
    };
    for i in 0..300 {
        let text = format!("Remark {i} concerning item {i} and nothing else at all.");
        let tag = format!("t{i}");
        s.offer(i as f64 * 2.0, &text, [tag.as_str()], GI, GN, GC);
    }
    assert!(s.shown.len() as i64 <= s.history.max(s.per_hour * 8));
}

#[test]
fn test_offer_keeps_the_tags_of_a_generator() {
    // offer() ran refusal() first, which used up a generator; the thought was recorded with no tags
    let mut s = screen();
    let generator = ["Vent", "ceiling"].into_iter().map(|t| t.to_string());
    assert!(s.offer(0.0, VENT, generator, GI, GN, GC));
    assert_eq!(s.shown.last().unwrap().tags, set(&["vent", "ceiling"]));
}

#[test]
fn test_record_shows_a_thought_already_judged_without_judging_it_again() {
    // The daemon asks refusal() itself to log the reason; offer() then ran the whole screen a second time.
    // record() never calls refusal(): it takes no scores and cannot refuse, so a thought that would now be
    // refused (too soon after itself) is still recorded.
    let mut s = screen();
    assert_eq!(s.refusal(0.0, VENT, ["vent"], GI, GN, GC), None);
    s.record(0.0, VENT, ["vent"]);
    assert_eq!(s.shown.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), vec![VENT]);
    assert_eq!(s.shown.last().unwrap().tags, set(&["vent"]));
    s.record(1.0, VENT, ["vent"]);
    assert_eq!(s.shown.len(), 2);
    assert_eq!(s.refused, 0);
}

// ---- exact refusal reasons (the daemon logs them) -------------------------------------------
// Each expected string is what the Python module returns for the same inputs.

#[test]
fn refusal_reasons_match_python_text() {
    let s = screen();
    assert_eq!(
        s.refusal(0.0, PLANT, ["plant"], 3, 4, 0.2).as_deref(),
        Some("below the floor (importance 3, novelty 4, cool 0.20)")
    );
    let mut s = screen();
    offer(&mut s, 0.0, VENT, &["vent"]);
    assert_eq!(refusal(&s, 10.0, PLANT, &["plant"]), "only 10s since the last one");
    assert_eq!(refusal(&s, 241.0, "Something else.", &["Wall", "VENT"]), "about vent again");
    let mut s = ThoughtScreen { per_hour: 1, ..ThoughtScreen::default() };
    offer(&mut s, 0.0, VENT, &["vent"]);
    assert_eq!(refusal(&s, 300.0, PLANT, &["plant"]), "1 already this hour");

    let mut s = ThoughtScreen { topic_limit: 1, ..ThoughtScreen::default() };
    offer(&mut s, 0.0, VENT, &["vent", "wall"]);
    assert_eq!(refusal(&s, 5000.0, PLANT, &["wall", "vent"]), "1 about vent/wall today already");

    let mut s = screen();
    offer(&mut s, 0.0, "The blue mug has a friend now on the shelf.", &["mug"]);
    assert_eq!(
        refusal(&s, 241.0, "Down by the door, blue mug has friend energy.", &["door"]),
        "phrase already used: ('blue', 'mug', 'friend')"
    );
    let mut s = screen();
    offer(&mut s, 0.0, PLANT, &["plant"]);
    assert_eq!(
        refusal(&s, 241.0, "Someone carried a tall plant past the window.", &["greenery"]),
        "phrase already used: ('carried', 'tall', 'plant')"
    );
    assert_eq!(
        refusal(&s, 241.0, "The window: a plant, tall, carried past by someone.", &["greenery"]),
        "says the same as 'Someone carried a tall plant past the wi' (0.75)"
    );
    let mut s = screen();
    offer(&mut s, 0.0, "Don't-panic mode, it's raining", &["x"]);
    assert_eq!(
        refusal(&s, 241.0, "don't-panic mode it's raining again", &["y"]),
        "phrase already used: (\"don't-panic\", 'mode', \"it's\")"
    );
}

#[test]
fn whitespace_is_squashed_before_judging_and_recording() {
    let mut s = screen();
    assert!(offer(&mut s, 0.0, "  The   vent\n is\tthere. ", &["  ", "Vent"]));
    assert_eq!(s.shown[0].text, "The vent is there.");
    assert_eq!(s.shown[0].tags, set(&["vent"]));
}

#[test]
fn zero_history_compares_against_everything_like_python_slicing() {
    // Python: shown[-0:] is the whole list, and del shown[:-0] deletes nothing.
    let mut s = ThoughtScreen { history: 0, per_hour: 0, min_interval_secs: 0.0, ..ThoughtScreen::default() };
    s.record(0.0, VENT, ["vent"]);
    s.record(1.0, PLANT, ["plant"]);
    assert_eq!(s.shown.len(), 2);
    assert_eq!(s.refusal(2.0, VENT, ["other"], GI, GN, GC).as_deref(), Some("0 already this hour"));
}
