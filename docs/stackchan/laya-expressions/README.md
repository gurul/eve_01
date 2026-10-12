# Live expressions

> **2026-09-24: Jev picks the eye by default.** The owner asked for Jev here too ("use jev for that too").
> The worker below is unchanged; only the picker moved. `EVE_EXPRESSION_BACKEND=jev` (default) asks
> TypeSafe's Jev one `choice` over the same eleven labels, in its native shape, over the same speaker-labelled
> context; `laya` keeps the local checkpoint described in the rest of this page. Measured with
> `bridge/tools/jev_eyes_eval.py` on the 84 hand-written cases in `bridge/tests/fixtures/emotion` (both pickers
> asked the eleven-label question, both folded onto the sets' six labels the same way; `jev-eyes-results.json`):
>
> | Picker | 48 scenario tests | 36 confirmation cases | Latency |
> |---|---|---|---|
> | **Jev** (`typesafe/jev-1.13` via OpenRouter) | **62.5%** acc, 0.583 macro-F1 | **72.2%** acc, 0.679 macro-F1 | 200 ms p50, 449 ms p95 |
> | Untouched local Laya | 47.9% acc, 0.471 macro-F1 | 52.8% acc, 0.492 macro-F1 | 21 ms p50 |
>
> 14 of Jev's 32 misses are the sets' "startled" read as "surprised" — the fold has no startled eye. Cost:
> $0.0018 for the 84 calls. Jev is a network call: the last turn or two of conversation leaves the Mac while
> expressions are enabled (`expressions.json`). No Jev key, or no network: the worker reports the error in its
> status and shows ordinary phase eyes; set `laya` to stay local.

> **2026-10-12: the phone's eyes.** The owner asked "can u route the expressions into the eyes": the board is
> retired, so Eve Link's voice-mode face now draws them. The daemon runs a second worker for the phone
> (`Daemon._start_phone_expressions`): the owner's words and each sentence eve reads out on an Eve Link call whose
> app says it can draw them are offered to it, and each pick goes back on that call as one `expression` message
> ([voice mode, Expressions](../../voice-mode.md#expressions)). It is **on by default** and needs no
> `expressions.json`; `EVE_PHONE_EXPRESSIONS=0` turns it off. It loads Jev only at the first offer, so with no
> phone call it makes no Jev call; during one, the last turn or two leave the Mac for Jev, as above.
> `bridge/tools/expression_live.py react "text"` shows on the phone when no board is connected and a call is
> live; `status` reports the phone's worker under `phone`.

Laya (or Jev) controls **eyes only**. Eve's original chirps, caption babble, sound settings, and phase sounds are preserved. The owner explicitly requested reverting the experimental Laya chirps after trying them.

The model runs in the Mac bridge; the ESP32 renders its temporary expression overlay during speech and idle/explore. Spoken user turns, stable clauses in Eve's streamed replies, and diary thoughts selected for the screen feed one dedicated worker. One pending event replaces older work; inference stays off the daemon event loop. Events are sent at most every 1.2 seconds, discarded after four seconds, and independently expired by the board.

Laya chooses eleven expressions: calm, happy, curious, affection, surprised, sad, worried, skeptical, frustrated, excited, and wink. Each has a distinct combination of eyelid shape, size, symmetry, rounding and blink tempo. A wink lets the viewer-right rounded-square eye collapse into a closed lid of the same width, with a flatter middle and softly rounded 22 px corner zones, then reopen after 650 ms, once per event, while the viewer-left eye keeps its normal rounded-square shape with a lifted brow (following the owner's visual reference); it then reopens. Listening, permission prompts, error phases, screen-off and clock display retain priority. The firmware expression handler has no sound or motor control. Caption sounds follow their original behavior even while a Laya eye overlay is active.

## Model and evidence

This is **experimental expression selection, not calibrated emotion recognition**. It selects Eve's reaction from transcript meaning, not the person's hidden emotional state. Recent user and Eve turns are speaker-labelled and retained for up to 90 seconds, so a reply can be interpreted in context. Listening still keeps the attentive eyes; the reaction appears when the speaking phase permits it.

The original 322M multilingual Laya weights remain unchanged. The separate older six-label tuning experiment did worse on fresh scenarios ([research and sources](../laya-emotion/README.md)). For this broader vocabulary, each candidate is a binary Laya question; the worker batches eleven questions and ranks their yes-versus-no logits. Normalized scores are relative support, not calibrated probabilities. No keyword animation script or new trained weights are used.

Four early prompt formulations scored 27/36, 27/39, 24/39 and 29/39 on authored examples (retained as `context-attempt-*.json`). The final binary formulation scored **34/39**, including all three wink cases. Those cases were reused during prompt selection, so this is a development measurement, not held-out accuracy or human validation. Errors include an ordinary time question becoming curious, some factual replies becoming surprised, and some ambiguity between sadness/frustration. Current measured results and provenance are in `context-results.json`.

Model: `~/.config/eve-bridge/models/laya-multilingual-mlx`. Dependencies: the bridge's `laya` extra (`laya-mlx` on Apple silicon; `eve-bridge update` installs it there). Override with `EVE_EXPRESSION_MODEL`. Enabling persists in `~/.config/eve-bridge/expressions.json`; new installations default off. Model load/inference errors leave ordinary phase expressions available.

## Controls

From the repository root:

```sh
bridge/.venv/bin/python bridge/tools/expression_live.py on
bridge/.venv/bin/python bridge/tools/expression_live.py status
bridge/.venv/bin/python bridge/tools/expression_live.py audition "I passed my final exam!"
bridge/.venv/bin/python bridge/tools/expression_live.py off
```

`react "text"` sends a cue without changing conversation phase. `audition` temporarily uses the speaking phase for six seconds, refuses an active conversation or audition, and restores the previous phase. `--phase listening` checks that listening wins. `eve-bridge sound off` keeps eyes active and silences sounds.

`--check` exercises the actual local model and board with happy, sad, skeptical, excited and wink cues during speaking and a suppressed affection cue during listening. It verifies eye application, expiry, one wink animation for the wink event, no Laya sound request, and an unchanged sound setting. It does not mute or unmute Eve. Board telemetry confirms the eye-render path ran; it is not a camera measurement of the screen.

Verification is recorded in [GATES.md](GATES.md) and `device-results.json`. Earlier tests of the superseded sound-enabled build are historical only. The final gate run targets eyes-only behavior and includes regression tests that normal caption chirps are preserved while Laya is enabled.

## Final device verification — 2026-09-21

Firmware `900435c` was flashed over USB with hashes verified. Six real-model cases passed on Eve: happy, sad, skeptical, excited, wink, and a listening-suppressed affection cue. The wink emitted exactly one animation event, reported the closed rounded-square lid after the eye collapsed, then reopening before expiry. Original sound stayed `on`. Event-to-command latency across these samples was 49.5–157.4 ms (a small bench sample). The host suite passed 116 tests; firmware shape/timing/priority checks passed; all six acceptance gates are met. See `device-results.json` for board acknowledgments.
