#!/usr/bin/env python3
"""Run the real Laya checkpoint over every eye label on explicit synthetic conversation cases.

The cases cover the original eleven labels only (the eight added 2026-10-12 are scored by jev_eyes_eval.py).
"""

import hashlib
import json
from pathlib import Path

from eve_bridge import eye_model
from eve_bridge.eye_model import LABELS, QUESTION, ConversationContext, LiveEyeModel
from eve_bridge.live_expressions import DEFAULT_MODEL

# Written before evaluating; includes user reactions and Eve replies.
CASES = [
    ("calm", "user", "", "What time is it?"),
    ("calm", "assistant", "What is two plus two?", "Two plus two is four."),
    ("calm", "assistant", "Set the timer.", "The timer is set for ten minutes."),
    ("happy", "user", "", "I had a really nice day and everything went well."),
    ("happy", "assistant", "I finished my homework.", "Nice work! You got it done."),
    ("happy", "user", "", "I'm feeling happy and satisfied with my progress."),
    ("curious", "user", "", "How do octopuses change their color?"),
    ("curious", "assistant", "Look at that odd machine.", "I wonder how the gears inside it work."),
    ("curious", "user", "", "I'd love to explore that cave and learn what's inside."),
    ("affection", "user", "", "I love having you around, Eve. You're my friend."),
    ("affection", "assistant", "Thank you for keeping me company.", "I'm glad I can be here with you."),
    ("affection", "user", "", "Thank you for always being there for me."),
    ("surprised", "user", "", "Whoa! A deer just walked through our front door!"),
    (
        "surprised",
        "assistant",
        "This old painting is worth a million dollars.",
        "Wait, a million? I did not expect that!",
    ),
    ("surprised", "user", "", "I just found a secret room behind the bookcase!"),
    ("sad", "user", "", "My dog died yesterday and I miss him terribly."),
    (
        "sad",
        "assistant",
        "My best friend moved away and I feel lonely.",
        "I'm sorry. It's hard when someone you love is far away.",
    ),
    ("sad", "user", "", "I failed the interview and I feel so disappointed."),
    ("worried", "user", "", "I'm scared about my surgery tomorrow."),
    (
        "worried",
        "assistant",
        "I smell smoke coming from the kitchen.",
        "That sounds concerning. Let's make sure you're safe.",
    ),
    ("worried", "user", "", "My cat hasn't come home in two days. I'm afraid something happened."),
    ("skeptical", "user", "", "That ad says this bracelet can cure every illness. I don't believe it."),
    (
        "skeptical",
        "assistant",
        "Someone says the moon is made of cheese.",
        "That claim doesn't add up. What evidence do they have?",
    ),
    ("skeptical", "user", "", "Are you sure that's true? Those numbers don't look right."),
    ("frustrated", "user", "", "This stupid printer jammed again! I've tried fixing it five times!"),
    (
        "frustrated",
        "assistant",
        "The app crashed and deleted all my work again.",
        "Ugh, losing all that work is so frustrating.",
    ),
    ("frustrated", "user", "", "Nothing works! I keep getting the same error and I'm fed up."),
    ("excited", "user", "", "I GOT INTO MY DREAM UNIVERSITY! I CAN'T WAIT!"),
    (
        "excited",
        "assistant",
        "We're going on our first adventure tomorrow!",
        "Yes! I can't wait to go exploring with you!",
    ),
    ("excited", "user", "", "We won the championship! This is the best day ever!"),
    ("sad", "assistant", "My dog died.", "Oh. I'm so sorry to hear that."),
    ("happy", "assistant", "I got a good grade.", "Oh, that's really good to hear."),
    (
        "worried",
        "assistant",
        "I'm worried I might lose my job.",
        "That sounds stressful. How are you holding up?",
    ),
    ("curious", "assistant", "I built something new.", "Oh? Tell me how it works."),
    (
        "skeptical",
        "assistant",
        "This stranger guarantees a hundredfold return overnight.",
        "That sounds too good to be true.",
    ),
    ("affection", "assistant", "I appreciate you, Eve.", "I appreciate you too, my friend."),
    ("wink", "user", "", "Give me a wink, Eve!"),
    ("wink", "assistant", "Can you keep our secret?", "Your secret is safe with me. Wink wink!"),
    ("wink", "user", "", "Wink at me!"),
]


def main():
    model = LiveEyeModel(DEFAULT_MODEL)
    results = []
    for expected, who, previous, text in CASES:
        context = ConversationContext()
        if previous:
            context.state("user", previous, 0)
        answer = model.predict(context.state(who, text, 1))
        results.append({"expected": expected, "speaker": who, "previous": previous, "text": text, **answer})
    matched = sum(r["label"] == r["expected"] for r in results)
    report = {
        "kind": "authored development cases reused during prompt selection, not held-out or human validation",
        "runtime_sha256": hashlib.sha256(Path(eye_model.__file__).read_bytes()).hexdigest(),
        "matched": matched,
        "total": len(results),
        "schema": QUESTION,
        "schema_sha256": hashlib.sha256(json.dumps(QUESTION, sort_keys=True).encode()).hexdigest(),
        "cases": results,
    }
    path = Path("docs/stackchan/laya-expressions/context-results.json")
    path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"{matched}/{len(results)} labels match")
    for r in results:
        if r["label"] != r["expected"]:
            print(r["expected"], "->", r["label"], r["text"])
    assert len(results) == 39 and sum(r["label"] == r["expected"] for r in results[:36]) >= 28
    assert all(r["label"] == "wink" for r in results[-3:])
    assert set(LABELS[:11]) <= {r["label"] for r in results}


if __name__ == "__main__":
    main()
