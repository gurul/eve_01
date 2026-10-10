"""intent.py: the phrase table (with a negative set that must stay none) and the
async classifier wrapper (fake client: confidence floor, timeout, errors, length cap)."""

from __future__ import annotations

import asyncio
import time
from types import SimpleNamespace

import pytest

from eve_bridge.intent import (
    LABELS,
    LEAVE,
    LESSON,
    LOOK,
    MUTE,
    NONE,
    UNMUTE,
    OpenAIIntentClient,
    fast_intent,
    make_classifier,
    parse_intent,
)

LEAVES = [
    "Goodbye.", "bye", "Bye eve!", "okay bye bye", "Alright, thanks, bye.", "see you later",
    "good night eve", "that's all for now", "That's it, thanks.", "We're done here.",
    "stop listening", "Please stop listening now.", "go away", "just go", "go to sleep",
    "I want you to leave.", "I want you to leave now", "we need you to go away", "leave me alone",
    "you can go now", "You can leave.", "get lost", "end the conversation", "I'm done talking",
    "Later.", "talk to you later", "Okay, see ya.",
]
MUTES = ["mute", "Mute yourself.", "be quiet", "shh", "hush eve", "no more beeping",
         "turn your sound off", "stop making noise", "go silent", "sound off please"]
UNMUTES = ["unmute", "Unmute yourself.", "sound back on", "turn your sound back on", "you can make sounds again"]
# Same words, about something else: the table must not claim these.
NOT_MINE = [
    "leave the tab open", "leave it on the desk", "mute Spotify", "mute the video", "tell Sam goodbye",
    "say bye to mom on Messages", "put the Mac to sleep", "make the computer go to sleep",
    "open it later", "is that all?", "that's all the files I need", "turn the sound off on YouTube",
    "go to the settings page", "what do you see", "play some music",
    "can you go and check my email", "stop the music",
    "look up the weather", "turn the volume down", "turn it down", "turn off the lights",
    "look at this email", "turn on the TV", "look for the file on the desktop",
    # Questions and computer jobs that share words with a lesson request.
    "what is ownership in Rust", "what's 17 times 23", "teach the kids to say hi on Messages",
    "I learned a lot today", "I want to learn more about this email", "help me understand this file",
    "walk me through the settings page",
]
LESSONS = [
    "teach me about Rust ownership", "Hey eve, teach me about Rust ownership.", "quiz me on French verbs",
    "can you tutor me", "I want a lesson", "I want to learn fractions", "help me work through this proof",
    "walk me through recursion", "let's do a lesson", "Could you help me understand the chain rule?",
    "test me on the periodic table", "I'd like to practice long division", "start a lesson",
]
LOOKS = [
    "look to your left", "Look a bit up and to your left.", "look behind you", "turn around",
    "Look around, eve.", "look at me", "can you look up at the ceiling", "turn your head to the right",
    "look down at the desk", "now look over there", "face the window", "look straight ahead",
]


@pytest.mark.parametrize("text", LOOKS)
def test_head_moves(text: str) -> None:
    assert fast_intent(text) == LOOK


@pytest.mark.parametrize("text", LESSONS)
def test_lesson_requests(text: str) -> None:
    assert fast_intent(text) == LESSON


@pytest.mark.parametrize("text", LEAVES)
def test_leave_phrases(text: str) -> None:
    assert fast_intent(text) == LEAVE


@pytest.mark.parametrize("text", MUTES)
def test_mute_phrases(text: str) -> None:
    assert fast_intent(text) == MUTE


@pytest.mark.parametrize("text", UNMUTES)
def test_unmute_phrases(text: str) -> None:
    assert fast_intent(text) == UNMUTE


@pytest.mark.parametrize("text", NOT_MINE)
def test_requests_about_the_computer_are_not_intents(text: str) -> None:
    assert fast_intent(text) is None


def test_empty_is_none() -> None:
    assert fast_intent("") is None and fast_intent("  ...  ") is None


def test_parse_intent_bounds_and_unknown_labels() -> None:
    assert parse_intent('{"intent":"leave","confidence":0.93}') == ("leave", 0.93)
    assert parse_intent('{"intent":"dance","confidence":2}') == ("none", 1.0)
    assert parse_intent('{"intent":"mute","confidence":"x"}') == ("mute", 0.0)


class FakeClient:
    def __init__(self, answer=("leave", 0.9), delay: float = 0.0, raises: Exception | None = None) -> None:
        self.answer, self.delay, self.raises = answer, delay, raises
        self.seen: list[str] = []

    def classify(self, text: str):
        self.seen.append(text)
        if self.delay:
            time.sleep(self.delay)
        if self.raises:
            raise self.raises
        return self.answer


def test_classifier_returns_label_above_the_floor() -> None:
    c = FakeClient(("leave", 0.8))
    assert asyncio.run(make_classifier(c)("alright, I'm heading out")) == LEAVE
    assert c.seen == ["alright, I'm heading out"]


def test_classifier_below_floor_or_none_is_no_intent() -> None:
    assert asyncio.run(make_classifier(FakeClient(("leave", 0.4)))("maybe later")) is None
    assert asyncio.run(make_classifier(FakeClient((NONE, 0.99)))("open Safari")) is None


def test_classifier_timeout_and_error_are_no_intent() -> None:
    assert asyncio.run(make_classifier(FakeClient(delay=0.3), timeout=0.05)("hmm")) is None
    assert asyncio.run(make_classifier(FakeClient(raises=RuntimeError("503")))("hmm")) is None


def test_long_turns_are_not_classified() -> None:
    c = FakeClient()
    long = " ".join(["word"] * 40)
    assert asyncio.run(make_classifier(c)(long)) is None
    assert c.seen == []


def test_request_is_not_stored_and_is_strict_json() -> None:
    body = OpenAIIntentClient("gpt-5.4-nano", api_key="sk-test").request("bye")
    assert body["store"] is False
    fmt = body["text"]["format"]
    assert fmt["strict"] is True and set(fmt["schema"]["properties"]["intent"]["enum"]) == set(LABELS)


def test_classify_parses_the_sdk_answer() -> None:
    c = OpenAIIntentClient("gpt-5.4-nano", api_key="sk-test")
    c._client = SimpleNamespace(responses=SimpleNamespace(
        create=lambda **kw: SimpleNamespace(output_text='{"intent":"unmute","confidence":0.7}')))
    assert c.classify("you can beep again") == (UNMUTE, 0.7)
