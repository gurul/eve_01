"""spoken_names: the name the owner meant when speech-to-text wrote it differently. Every name here is made up."""

from __future__ import annotations

import pytest

from eve_bridge import spoken_names as N

DEVICES = ["iPhone", "Sam’s MacBook Pro", "Sam’s MacBook Air", "bluefin7", "Kitchen Speaker", "Den TV"]


@pytest.mark.parametrize("said, meant", [
    ("bluefin7", "bluefin7"),
    ("Blue Fin 7", "bluefin7"),                # split by the transcript
    ("blue-fin 7", "bluefin7"),
    ("blue fin seven", "bluefin7"),            # a number said as a word
    ("blew fin seven", "bluefin7"),            # sounds the same
    ("bluefin 7", "bluefin7"),
    ("blue fen 7", "bluefin7"),                # one wrong letter
    ("kitchen speeker", "Kitchen Speaker"),
    ("speaker kitchen", "Kitchen Speaker"),    # another order
    ("sams macbook air", "Sam’s MacBook Air"),
    ("macbook air", "Sam’s MacBook Air"),
    ("den tee vee", "Den TV"),                  # letters said one by one sound like the name
    ("tv", "Den TV"),                           # the one name with that word
    ("cellar speaker", None),                   # a word that is no name's: not a guess
])
def test_resolve_finds_the_one_name(said: str, meant: str | None) -> None:
    got = [DEVICES[i] for i in N.resolve(said, DEVICES)]
    assert got == ([meant] if meant else []), got


def test_two_names_about_as_close_are_both_returned_so_the_caller_asks() -> None:
    got = {DEVICES[i] for i in N.resolve("macbook", DEVICES)}
    assert got == {"Sam’s MacBook Pro", "Sam’s MacBook Air"}


@pytest.mark.parametrize("said", ["garage", "jazz", "the", "", "daft punk", "lights", "seven", "7"])
def test_resolve_does_not_invent_a_match(said: str) -> None:
    assert N.resolve(said, DEVICES) == []


def test_words_turns_number_words_into_digits() -> None:
    assert N.words("Twenty One Pilots") == ["21", "pilots"]
    assert N.words("room twelve") == ["room", "12"]
    assert N.squash("Blue Fin 7") == N.squash("bluefin7") == "bluefin7"
    assert N.skeleton("blew fin seven") == N.skeleton("bluefin7")


def test_short_words_that_merely_share_consonants_do_not_match() -> None:
    assert N.resolve("tubi", ["you tube", "Netflix"]) == []
    assert N.resolve("den", ["Dean's Lamp"]) == []
