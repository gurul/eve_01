"""thought_screen.py: what earns a place on the robot's screen. Pure — a
hand-rolled clock, no board, no model."""

from __future__ import annotations

from eve_bridge.thought_screen import (
    MIN_INTERVAL_SECS,
    ThoughtScreen,
    content_words,
    similarity,
    tag_overlap,
    trigrams,
)

VENT = "The vent is annoyingly centered today, as if it were posing."
VENT_AGAIN = "The vent is still exactly where it plotted to be, posing again."
PLANT = "Someone carried a tall plant past the window and did not come back."

# Anything that clears the floor without being remarkable.
GOOD = dict(importance=6, novelty=5, cool=0.3)


def _screen(**kw) -> ThoughtScreen:
    return ThoughtScreen(**kw)


# ---- the measures ---------------------------------------------------------------------------

def test_content_words_drop_the_words_that_say_nothing() -> None:
    assert content_words("The vent is still where it was") == {"vent"}
    assert content_words("Someone left a blue mug on the desk") == {"someone", "left", "blue", "mug", "desk"}
    assert content_words("") == set()


def test_similarity_is_one_for_the_same_sentence_and_zero_for_unrelated() -> None:
    assert similarity(VENT, VENT) == 1.0
    assert similarity(VENT, PLANT) == 0.0
    assert similarity("", VENT) == 0.0


def test_surface_similarity_alone_does_not_separate_the_repetitive_pair() -> None:
    """The finding that shapes this module: eve's real repeats do not look
    alike enough for any threshold anyone ships to catch them. The subject
    rule has to do the work."""
    assert similarity(VENT, VENT_AGAIN) < 0.5
    assert similarity(VENT, PLANT) < similarity(VENT, VENT_AGAIN)


def test_trigrams_catch_a_repeated_turn_of_phrase() -> None:
    a = "the blue mug has a friend now, apparently"
    b = "over by the window, blue mug has friend energy"
    assert trigrams("blue mug has friend") & trigrams(a)
    assert not (trigrams(a) & trigrams("nothing whatsoever in common here"))
    assert trigrams("two words") == set()
    assert similarity(a, b) < 0.6                    # not similar enough to veto on words alone
    assert trigrams(a) & trigrams(b)                 # ... but the phrase repeats


def test_tag_overlap() -> None:
    assert tag_overlap({"vent"}, {"vent"}) == 1.0
    assert tag_overlap({"vent", "wall"}, {"vent", "desk"}) == 1 / 3
    assert tag_overlap(set(), {"vent"}) == 0.0


# ---- the floor ------------------------------------------------------------------------------

def test_a_dull_thought_never_reaches_the_screen() -> None:
    s = _screen()
    assert not s.offer(0.0, PLANT, {"plant"}, importance=3, novelty=4, cool=0.2)
    assert "below the floor" in (s.refusal(0.0, PLANT, {"plant"}, importance=3, novelty=4, cool=0.2) or "")
    assert s.shown == []


def test_any_one_of_the_three_clears_the_floor() -> None:
    for kw in (dict(importance=6, novelty=1, cool=0.0),
               dict(importance=1, novelty=7, cool=0.0),
               dict(importance=1, novelty=1, cool=0.70)):
        assert _screen().offer(0.0, PLANT, {"plant"}, **kw), kw


def test_the_floor_is_not_whether_the_diary_wrote_it() -> None:
    """The diary also writes after three hours of silence, which keeps thoughts
    that are dull by construction; those must not reach the screen."""
    s = _screen()
    dull_but_written = dict(importance=2, novelty=3, cool=0.1)
    assert not s.offer(0.0, "Nothing has moved since breakfast.", {"desk"}, **dull_but_written)


def test_an_empty_thought_is_never_shown() -> None:
    s = _screen()
    assert not s.offer(0.0, "   ", {"x"}, **GOOD)
    assert s.refusal(0.0, "", (), **GOOD) == "empty"


# ---- repetition -----------------------------------------------------------------------------

def test_the_first_thought_goes_up() -> None:
    s = _screen()
    assert s.offer(0.0, VENT, {"vent"}, **GOOD)
    assert len(s.shown) == 1 and s.refused == 0


def test_a_new_sentence_about_the_same_subject_is_still_the_same_subject() -> None:
    s = _screen()
    s.offer(0.0, VENT, {"vent"}, **GOOD)
    now = MIN_INTERVAL_SECS + 1
    fresh = "Nothing up there has moved since breakfast, which is its own kind of news."
    assert similarity(fresh, VENT) < s.similarity          # words alone would let it through
    assert not s.offer(now, fresh, {"vent", "wall"}, **GOOD)
    assert "vent" in (s.refusal(now, fresh, {"vent", "wall"}, **GOOD) or "")
    # After the subject window it is fair game again.
    later = now + s.subject_window_secs + 1
    assert s.offer(later, fresh, {"vent", "wall"}, **GOOD)


def test_a_subject_gets_only_so_many_turns_in_a_day() -> None:
    s = _screen(topic_limit=2)
    lines = ["The vent presides over everything, aloof as a magistrate.",
             "Four slats up there, and not one of them has ever moved.",
             "Grille duty must be dull work; nobody thanks it.",
             "Somebody should dust that thing before spring arrives."]
    t = 0.0
    for line in lines[:2]:
        assert s.offer(t, line, {"vent"}, **GOOD), line
        t += s.subject_window_secs + 1                      # past the acute veto each time
    assert not s.offer(t, lines[2], {"vent"}, **GOOD)
    assert "today already" in (s.refusal(t, lines[2], {"vent"}, **GOOD) or "")
    # ... and past the day window it may return.
    assert s.offer(t + s.topic_window_secs, lines[3], {"vent"}, **GOOD)


def test_a_repeated_turn_of_phrase_is_refused_even_with_different_tags() -> None:
    s = _screen()
    s.offer(0.0, "The blue mug has a friend now on the shelf.", {"mug"}, **GOOD)
    now = MIN_INTERVAL_SECS + 1
    assert not s.offer(now, "Down by the door, blue mug has friend energy again.", {"door"}, **GOOD)
    assert "phrase already used" in (s.refusal(now, "Down by the door, blue mug has friend energy.", {"door"}, **GOOD) or "")


def test_a_near_literal_restatement_is_refused_by_the_backstop() -> None:
    s = _screen()
    s.offer(0.0, PLANT, {"plant"}, **GOOD)
    now = MIN_INTERVAL_SECS + 1
    assert not s.offer(now, "Someone carried a tall plant past the window.", {"greenery"}, **GOOD)


def test_ubiquitous_tags_stop_identifying_anything() -> None:
    """Nearly every frame has a desk in it. If 'desk' counted as the subject,
    eve would fall silent after its first remark."""
    s = _screen(subject_window_secs=10 ** 9)
    t = 0.0
    for _i, (thing, line) in enumerate([("vent", "The vent, aloof as ever, presides."),
                                       ("mug", "A mug appears, steaming quietly."),
                                       ("chair", "The chair has been pushed in neatly."),
                                       ("window", "Rain has started against the glass.")]):
        assert s.offer(t, line, {"desk", thing}, **GOOD), line
        t += MIN_INTERVAL_SECS + 1
    assert "desk" in s.ubiquitous_tags()
    assert s.subject_of({"desk", "lamp"}) == {"lamp"}
    # A thought about something new still gets through despite sharing "desk".
    assert s.offer(t, "A parcel has arrived and nobody has opened it.", {"desk", "parcel"}, **GOOD)


def test_a_thought_whose_tags_are_all_ubiquitous_keeps_them() -> None:
    s = _screen(subject_window_secs=10 ** 9)
    t = 0.0
    for _i, line in enumerate(["The desk is tidy today, unusually.",
                              "A pen rolled off the desk edge.",
                              "The desk lamp is warm already.",
                              "Papers cover the desk corner now."]):
        s.offer(t, line, {"desk"}, **GOOD)
        t += MIN_INTERVAL_SECS + 1
    assert s.subject_of({"desk"}) == {"desk"}          # nothing else to fall back on


# ---- rate -----------------------------------------------------------------------------------

def test_the_screen_stays_quiet_between_captions() -> None:
    s = _screen()
    s.offer(0.0, VENT, {"vent"}, **GOOD)
    assert not s.offer(MIN_INTERVAL_SECS - 1, PLANT, {"plant"}, **GOOD)
    assert "since the last one" in (s.refusal(10.0, PLANT, {"plant"}, **GOOD) or "")
    assert s.offer(MIN_INTERVAL_SECS + 1, PLANT, {"plant"}, **GOOD)


def test_an_hourly_cap_stops_a_talkative_hour() -> None:
    s = _screen(per_hour=3, min_interval_secs=1.0)
    for i, line in enumerate(["A parcel arrived unopened.", "Rain against the glass now.", "The kettle went cold."]):
        assert s.offer(i * 10.0, line, {f"t{i}"}, **GOOD), line
    assert not s.offer(40.0, "Somebody moved the chair by the door.", {"chair"}, **GOOD)
    assert "already this hour" in (s.refusal(40.0, "Another line entirely.", {"z"}, **GOOD) or "")
    assert s.offer(3700.0, "Somebody moved the chair by the door.", {"chair"}, **GOOD)


def test_a_refused_thought_is_dropped_not_queued() -> None:
    """Nothing is held back to show later: by then eve is looking elsewhere."""
    s = _screen()
    s.offer(0.0, VENT, {"vent"}, **GOOD)
    s.offer(10.0, PLANT, {"plant"}, **GOOD)            # refused, too soon
    assert [x.text for x in s.shown] == [VENT]
    assert s.offer(MIN_INTERVAL_SECS + 1, "A third remark, about the door this time.", {"door"}, **GOOD)
    assert s.shown[-1].text.startswith("A third remark")


def test_only_the_recent_captions_are_compared_against() -> None:
    s = _screen(history=2, min_interval_secs=1.0, subject_window_secs=1.0, topic_limit=99)
    s.offer(0.0, VENT, {"vent"}, **GOOD)
    s.offer(10.0, "A remark about the kettle and its habits.", {"kettle"}, **GOOD)
    s.offer(20.0, "A remark about the doorway and its draught.", {"door"}, **GOOD)
    assert s.offer(30.0, VENT, {"vent"}, **GOOD)


def test_history_does_not_grow_without_bound() -> None:
    s = _screen(min_interval_secs=1.0, per_hour=1000, history=4, subject_window_secs=0.0, topic_limit=10 ** 6)
    for i in range(300):
        s.offer(i * 2.0, f"Remark {i} concerning item {i} and nothing else at all.", {f"t{i}"}, **GOOD)
    assert len(s.shown) <= max(s.history, s.per_hour * 8)
